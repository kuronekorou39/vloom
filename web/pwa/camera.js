// 受信 3 画面 (受信 / V受信 / 校正-受信) で共有するカメラ選択と実測診断。
//
// PC には背面カメラが無いので facingMode:"environment" は満たされず、ブラウザは
// 「最も近いデバイス」を暗黙に選ぶ。Windows Hello の IR (赤外線) カメラや仮想カメラ
// (OBS 等) を掴むと、映像が真っ暗・低解像度になり何を映しても復号できない。
// 明示選択 + 実測表示で「読めない原因がカメラ側か解像度不足か」を切り分ける。

const LS_KEY = "beyondqr.cameraId";
// Windows Hello の赤外線カメラ・深度センサ。可視光の像が得られないため既定から外す。
const IR_PATTERN = /(^|[^a-z])(ir|infrared|赤外線|depth)([^a-z]|$)/i;

const SIZE = { width: { ideal: 1920 }, height: { ideal: 1080 } };

export const isIrCamera = (label) => IR_PATTERN.test(label || "");

/** カメラ一覧。許可前のブラウザは deviceId を伏せるので、その分は落とす。 */
export async function listCameras() {
  if (!navigator.mediaDevices || !navigator.mediaDevices.enumerateDevices) return [];
  const all = await navigator.mediaDevices.enumerateDevices();
  return all.filter((d) => d.kind === "videoinput" && d.deviceId);
}

/** 実際に開かれたカメラの deviceId (選択 UI を実態に合わせるため)。 */
export function streamDeviceId(stream) {
  const track = stream && stream.getVideoTracks()[0];
  const s = track && track.getSettings ? track.getSettings() : {};
  return s.deviceId || null;
}

/**
 * カメラを開く。deviceId 指定があればそれを厳密に、無ければ背面カメラ優先 (スマホ) で開く。
 * PC で IR カメラを掴んでしまった場合は可視光カメラに開き直す。
 */
export async function openCamera(deviceId) {
  if (deviceId) {
    return navigator.mediaDevices.getUserMedia({
      video: { ...SIZE, deviceId: { exact: deviceId } },
      audio: false,
    });
  }
  const stream = await navigator.mediaDevices.getUserMedia({
    video: { ...SIZE, facingMode: "environment" },
    audio: false,
  });
  const label = stream.getVideoTracks()[0] ? stream.getVideoTracks()[0].label : "";
  if (!isIrCamera(label)) return stream;
  const visible = (await listCameras()).find((c) => !isIrCamera(c.label));
  if (!visible) return stream;
  stream.getTracks().forEach((t) => t.stop());
  return navigator.mediaDevices.getUserMedia({
    video: { ...SIZE, deviceId: { exact: visible.deviceId } },
    audio: false,
  });
}

/** <select> にカメラ一覧を出し、選択を localStorage に覚える。 */
export class CameraPicker {
  constructor(selectEl) {
    this.el = selectEl;
    this.el.addEventListener("change", () => {
      localStorage.setItem(LS_KEY, this.el.value);
    });
    this.refresh();
  }

  /** 一覧を再取得して反映する。ラベルは getUserMedia 許可後でないと空になるので、
   *  カメラ開始後にもう一度呼ぶこと。activeId を渡すと、未選択時にそれを選択状態にする
   *  (「自動選択」で実際にどれが使われたかを見えるようにする)。 */
  async refresh(activeId) {
    const cams = await listCameras();
    const saved = localStorage.getItem(LS_KEY) || "";
    const prev = this.el.value;
    this.el.innerHTML = "";
    const auto = document.createElement("option");
    auto.value = "";
    auto.textContent = cams.length ? "自動選択" : "カメラ未検出 (許可後に表示)";
    this.el.appendChild(auto);
    cams.forEach((c, i) => {
      const o = document.createElement("option");
      o.value = c.deviceId;
      o.textContent = (c.label || `カメラ ${i + 1}`) + (isIrCamera(c.label) ? " ⚠赤外線" : "");
      this.el.appendChild(o);
    });
    const want = prev || saved || activeId;
    if (want && cams.some((c) => c.deviceId === want)) this.el.value = want;
  }

  get deviceId() {
    return this.el.value || null;
  }
}

/** 実測値 (スキャン fps / 明るさ / 飽和) を集計する。 */
export class ScanStats {
  constructor() {
    this.fps = 0;
    this.mean = 0;
    this.sat = 0;
    this._frames = 0;
    this._since = performance.now();
  }

  /**
   * 1 フレーム分を記録する。buf は輝度 (bpp=1) か RGBA (bpp=4)。
   * 輝度統計は 64 画素おきの間引きで十分。
   */
  tick(buf, bpp = 1) {
    this._frames++;
    const now = performance.now();
    if (now - this._since >= 500) {
      this.fps = (this._frames * 1000) / (now - this._since);
      this._frames = 0;
      this._since = now;
    }
    const off = bpp === 4 ? 1 : 0; // RGBA は G を輝度の代表値にする
    const step = bpp * 64;
    let sum = 0, sat = 0, n = 0;
    for (let i = off; i < buf.length; i += step) {
      const v = buf[i];
      sum += v;
      if (v >= 250) sat++;
      n++;
    }
    if (n) {
      this.mean = sum / n;
      this.sat = sat / n;
    }
  }
}

/** カメラの実設定を "Integrated Webcam · 1280×720 @30fps" 形式で返す。 */
export function cameraInfoText(stream) {
  const track = stream && stream.getVideoTracks()[0];
  if (!track) return "カメラ情報なし";
  const s = track.getSettings ? track.getSettings() : {};
  const fps = s.frameRate ? ` @${Math.round(s.frameRate)}fps` : "";
  const warn = isIrCamera(track.label) ? " ⚠赤外線カメラ (可視光が写りません)" : "";
  return `${track.label || "カメラ"} · ${s.width || "?"}×${s.height || "?"}${fps}${warn}`;
}

/** 明るさの実測を "明るさ 118 (飽和 2%)" 形式で返す。 */
export function lumaText(stats) {
  const sat = Math.round(stats.sat * 100);
  let note = "";
  if (stats.mean < 60) note = " ⚠暗すぎ";
  else if (sat >= 20) note = " ⚠白飛び";
  return `明るさ ${Math.round(stats.mean)} (飽和 ${sat}%)${note}`;
}

/**
 * vcode のガイド枠幅から、1 セルあたり何画素で写るかの理論値を返す。
 * 実機では 6px/セル 以上あれば安定、4px を切ると輝度 4 値はまず復号できない。
 */
export function cellPxText(guidePx) {
  const dense = guidePx / 140; // 7×6 = 140 セル幅
  const std = guidePx / 100; //   5×4 = 100 セル幅
  const mark = dense >= 6 ? "" : dense >= 4.5 ? " ⚠余裕なし" : " ⚠解像度不足";
  return `ガイド枠 ${Math.round(guidePx)}px → ${dense.toFixed(1)} px/セル (7×6) / ${std.toFixed(1)} (5×4)${mark}`;
}
