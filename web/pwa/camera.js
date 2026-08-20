// 受信 3 画面 (受信 / V受信 / 校正-受信) で共有するカメラ選択・実測診断・露出制御。
//
// PC には背面カメラが無いので facingMode:"environment" は満たされず、ブラウザは
// 「最も近いデバイス」を暗黙に選ぶ。Windows Hello の IR (赤外線) カメラや仮想カメラ
// (OBS 等) を掴むと、映像が真っ暗・低解像度になり何を映しても復号できない。
// 明示選択 + 実測表示で「読めない原因がカメラ側か解像度不足か」を切り分ける。

const LS_KEY = "vloom.cameraId";
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

// ExposureGuard の判定値。sat / mean は ScanStats の間引きサンプルの実測比・平均。
// 注意: 1bit コード (白黒) は白セルが半分ほど 250 超になり sat≈0.5 が正常。飽和が多い
// だけでは白飛びと見なせないので、平均輝度も高い「画面全体がほぼ白」のときだけ下げる。
const EXPO_WHITEOUT_SAT = 0.7;   // 飽和画素がこの割合を超え、かつ↓も満たせば真の白飛び
const EXPO_WHITEOUT_MEAN = 200;  // 平均輝度がこれ以上 = 画面全体が白く飛んでいる
const EXPO_RECOVER_MEAN = 130;   // 平均がこれ未満に下がったら (明るい対象が外れた) 露出を戻す
const EXPO_RECOVER_TICKS = 3;    // ↑が連続 (500ms × 3) したら露出を倍に戻す

/**
 * 白飛びをカメラ露出で解消するコントローラ。部屋より明るいスマホ画面を写すと、
 * 自動露出 (AE) は視野全体の平均に合わせるため画面領域が飽和して「真っ白」になり、
 * 何を映しても復号できない。飽和した画素は情報が失われておりソフトでは復元できない
 * ので、露出制御対応カメラ (Chromium + UVC カメラが典型) では露出時間を手動で
 * 段階的に下げて飽和自体を解消する。未対応カメラでは何もしない (lumaText がヒントを
 * 出す)。update() は診断と同じ周期 (500ms) で呼ぶこと。
 */
export class ExposureGuard {
  constructor(stream) {
    this.track = stream ? stream.getVideoTracks()[0] : null;
    const caps = this.track && this.track.getCapabilities ? this.track.getCapabilities() : {};
    const modes = caps.exposureMode || [];
    const ok = modes.includes("manual") && modes.includes("continuous") && caps.exposureTime;
    this.range = ok ? caps.exposureTime : null; // 露出時間 {min, max} (100µs 単位)
    this.value = null;  // 手動設定中の露出時間。null = 自動露出のまま
    this._auto0 = 0;    // 介入直前の自動露出値 (ここまで戻したら自動露出へ返す)
    this._skip = 0;     // 設定反映待ちの tick 数 (反映前の映像で再判断しない)
    this._dark = 0;     // 暗すぎ状態の連続 tick 数
    this._busy = false;
    this._manual = false; // ユーザーが手動露出を選んでいる間は自動制御を止める
  }

  get supported() { return this.range !== null; }
  get active() { return this.value !== null; }
  get manual() { return this._manual; }
  get min() { return this.range ? this.range.min : 0; }
  get max() { return this.range ? this.range.max : 0; }
  /** スライダー初期値用。手動設定中ならその値、なければ現在の自動露出値。 */
  currentValue() { return this.value !== null ? this.value : this._currentTime(); }

  /** 手動露出に切り替え、露出時間 v を設定する。 */
  setManual(v) {
    if (!this.range) return;
    this._manual = true;
    const val = Math.max(this.range.min, Math.min(this.range.max, v));
    this._apply({ exposureMode: "manual", exposureTime: val }, val);
  }

  /** 自動露出へ戻す (手動モード解除)。 */
  setAuto() {
    this._manual = false;
    this._dark = 0;
    if (this.value !== null) this._apply({ exposureMode: "continuous" }, null);
  }

  /** 現在の手動露出時間の表示用文字列 ("12ms")。active のときだけ呼ぶ。 */
  valueText() {
    const ms = this.value / 10;
    return (ms >= 10 ? Math.round(ms) : ms.toFixed(1)) + "ms";
  }

  update(stats) {
    if (!this.range || this._busy || this._manual) return; // 手動モード中は自動制御しない
    if (this._skip > 0) { this._skip--; return; }
    // 露出を下げるのは「真の白飛び」= 画面全体がほぼ白のときだけ。1bit コードの白セル
    // (半分ほど飽和) を白飛びと誤判定して暗く潰さないよう、平均輝度でも門を切る。
    if (stats.sat >= EXPO_WHITEOUT_SAT && stats.mean >= EXPO_WHITEOUT_MEAN) {
      this._dark = 0;
      if (this.value === null) this._auto0 = this._currentTime();
      const cur = this.value !== null ? this.value : this._auto0;
      const next = Math.max(this.range.min, cur / 2);
      if (this.value === null || next < this.value) {
        this._apply({ exposureMode: "manual", exposureTime: next }, next);
      }
      return;
    }
    if (this.value === null) return; // 介入していなければ自動露出のまま (通常はここ)
    // 明るい対象が視野から外れて暗くなったら、下げていた露出を倍々で戻し、介入前の値まで
    // 戻ったら自動露出へ返す。白飛びが再発すれば上の分岐で即下がるので発振しない。
    this._dark = stats.mean < EXPO_RECOVER_MEAN ? this._dark + 1 : 0;
    if (this._dark < EXPO_RECOVER_TICKS) return;
    this._dark = 0;
    const next = this.value * 2;
    if (next >= this._auto0) this._apply({ exposureMode: "continuous" }, null);
    else this._apply({ exposureMode: "manual", exposureTime: next }, next);
  }

  _currentTime() {
    const s = this.track.getSettings ? this.track.getSettings() : {};
    return s.exposureTime || Math.sqrt(this.range.min * this.range.max);
  }

  _apply(constraints, value) {
    this.value = value;
    this._busy = true;
    this._skip = 1;
    const done = () => { this._busy = false; };
    this.track.applyConstraints(constraints).then(done, () => {
      // getCapabilities が対応を謳っていても実機で失敗するなら、以後は触らない
      this.range = null;
      this.value = null;
      done();
    });
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

/** 明るさの実測を "明るさ 118 (飽和 2%)" 形式で返す。guard (ExposureGuard) を渡すと
 *  露出の手動調整状態を併記し、非対応カメラで白飛びしたときは対処ヒントを出す。 */
export function lumaText(stats, guard) {
  const sat = Math.round(stats.sat * 100);
  let note = "";
  // 1bit コードは白セルで飽和 50% 前後が正常。真の白飛び (ほぼ全面が白) のみ警告する。
  if (stats.mean < 60) note = " ⚠暗すぎ";
  else if (sat >= 70) note = " ⚠白飛び";
  if (guard && guard.active) note += ` · 露出 ${guard.valueText()} (白飛び対策)`;
  else if (sat >= 70 && guard && !guard.supported) note += " → 送信側の画面輝度を下げてください";
  return `明るさ ${Math.round(stats.mean)} (飽和 ${sat}%)${note}`;
}

/** vcode のセル幅 = grid_w × ブロック一辺 (Rust 側 Layout::BLOCK と一致させる) */
const VCODE_BLOCK = 20;
const cellsWide = (grid) => parseInt(grid.split("x")[0], 10) * VCODE_BLOCK;

/**
 * vcode のガイド枠幅から、1 セルあたり何画素で写るかの理論値を返す。
 * 実機では 6px/セル 以上あれば安定、4px を切ると輝度 4 値はまず復号できない。
 * grid は "auto" (候補総当たり) か "9x8" のような固定指定。
 */
export function cellPxText(guidePx, grid = "auto") {
  const grids = grid === "auto" ? ["7x6", "5x4"] : [grid];
  const fmt = (g) => `${(guidePx / cellsWide(g)).toFixed(1)} px/セル (${g.replace("x", "×")})`;
  const densest = guidePx / cellsWide(grids[0]);
  const mark = densest >= 6 ? "" : densest >= 4.5 ? " ⚠余裕なし" : " ⚠解像度不足";
  return `ガイド枠 ${Math.round(guidePx)}px → ${grids.map(fmt).join(" / ")}${mark}`;
}
