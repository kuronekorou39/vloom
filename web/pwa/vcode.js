// PC 用 vcode 送受信。スマホアプリの vcode と同形式:
//   - 送信: 生ペイロード全体を VcodeTx で符号化 (チャンク無し=単一ペイロード)、フレーム循環表示
//   - 受信: カメラ→輝度Y→VcodeRx.scan→パケット→FountainDecoder→生バイト→型sniff→保存
// 受信側で CANDIDATES に無い格子は検出できないため、格子は 7x6 / 5x4 のみ。

import { VcodeTx, VcodeRx, FountainDecoder, vcodeUnwrapPayload, vcodeUnwrapFile } from "./pkg/beyond_qr_core_wasm.js";
import { openCamera, ScanStats, cameraInfoText, lumaText, cellPxText } from "./camera.js";

const REPAIR_RATE = 0.5;

// スキャナに渡すガイド枠幅 (中央正方形クロップの幅に対する比)。UI のガイド枠と一致させる。
// アプリ側 kVcodeGuideFrac と同値。
export const VCODE_GUIDE_FRAC = 0.8;
// スキャン画像の一辺の上限 (これを超える映像は縮小する)。上げるほど解像するが 1 フレームの
// 処理コストは面積比で増える。
const SCAN_MAX = 1280;
// 診断表示の更新間隔
const DIAG_INTERVAL_MS = 500;

export class VcodeSender {
  constructor({ canvas, onStatus }) {
    this.canvas = canvas;
    this.ctx = canvas.getContext("2d");
    this.onStatus = onStatus;
    this.off = document.createElement("canvas");
    this.running = false;
    this.seq = 0;
  }

  async start(fileOrBytes, gridStr, bpc, fps) {
    const payload = fileOrBytes instanceof Uint8Array
      ? fileOrBytes
      : new Uint8Array(await fileOrBytes.arrayBuffer());
    const [gw, gh] = gridStr.split("x").map(Number);
    const packetSize = bpc === 2 ? 92 : 42;
    const sourcePackets = Math.ceil(payload.length / packetSize);
    const extraRepair = Math.ceil(sourcePackets * REPAIR_RATE);
    const tx = new VcodeTx(payload, extraRepair, gw, gh, bpc);
    this.tx = tx;
    this.w = tx.frameWidth();
    this.h = tx.frameHeight();
    this.frameCount = tx.frameCount();
    this.off.width = this.w;
    this.off.height = this.h;
    this.running = true;
    const mySeq = ++this.seq;
    this._loop(mySeq, fps, payload.length);
  }

  _drawFrame(i) {
    const gray = this.tx.frameGray(i);
    const octx = this.off.getContext("2d");
    const id = octx.createImageData(this.w, this.h);
    for (let p = 0; p < this.w * this.h; p++) {
      const v = gray[p];
      id.data[p * 4] = v; id.data[p * 4 + 1] = v; id.data[p * 4 + 2] = v; id.data[p * 4 + 3] = 255;
    }
    octx.putImageData(id, 0, 0);

    const { ctx, canvas } = this;
    ctx.fillStyle = "#fff";
    ctx.fillRect(0, 0, canvas.width, canvas.height);
    ctx.imageSmoothingEnabled = false;
    // アスペクト維持で中央にフィット
    const scale = Math.min(canvas.width / this.w, canvas.height / this.h);
    const dw = Math.floor(this.w * scale), dh = Math.floor(this.h * scale);
    const dx = ((canvas.width - dw) / 2) | 0, dy = ((canvas.height - dh) / 2) | 0;
    ctx.drawImage(this.off, 0, 0, this.w, this.h, dx, dy, dw, dh);
  }

  async _loop(mySeq, fps, payloadLen) {
    const interval = 1000 / fps;
    let idx = 0, pass = 0;
    while (this.running && mySeq === this.seq) {
      const t0 = performance.now();
      this._drawFrame(idx % this.frameCount);
      idx++;
      if (idx % this.frameCount === 0) pass++;
      if (mySeq !== this.seq) break;
      this.onStatus(`vcode 送信中 · ${payloadLen}B · frame ${idx} · ${pass + 1} 巡目`);
      const dt = performance.now() - t0;
      if (dt < interval) await new Promise((r) => setTimeout(r, interval - dt));
    }
  }

  stop() { this.seq++; this.running = false; }
}

export class VcodeReceiver {
  constructor({ video, onProgress, onDone, onError, onDiag }) {
    this.video = video;
    this.onProgress = onProgress;
    this.onDone = onDone;
    this.onError = onError;
    this.onDiag = onDiag || (() => {});
    this.cap = document.createElement("canvas");
    this.stream = null;
    this.rafId = null;
    this.guideEl = null;
    this._onResize = () => this._positionGuide();
  }

  async start(deviceId) {
    this._reset();
    this.stream = await openCamera(deviceId);
    this.video.srcObject = this.stream;
    await this.video.play();
    this._ensureGuide();
    this.video.addEventListener("loadedmetadata", this._onResize);
    window.addEventListener("resize", this._onResize);
    this.rx = new VcodeRx();
    const loop = () => { if (!this.stream) return; this._scan(); this.rafId = requestAnimationFrame(loop); };
    this.rafId = requestAnimationFrame(loop);
  }

  _reset() {
    this.rx = null; this.dec = null; this.finished = false; this.frames = 0; this.detected = 0;
    this.stats = new ScanStats();
    this._lastDiag = 0;
  }

  // カメラ実解像度・スキャン fps・明るさ・理論 px/セル を出す。読めないときに
  // 「カメラが違う / 解像度不足 / 白飛び / コードが小さすぎ」を切り分けるための実測値。
  _diag(crop, target) {
    const now = performance.now();
    if (now - this._lastDiag < DIAG_INTERVAL_MS) return;
    this._lastDiag = now;
    const size = crop === target ? `${target}px` : `${crop}→${target}px`;
    this.onDiag(
      `${cameraInfoText(this.stream)}\n` +
      `スキャン ${size} · ${this.stats.fps.toFixed(1)} fps · ${lumaText(this.stats)}\n` +
      cellPxText(target * VCODE_GUIDE_FRAC)
    );
  }

  // scan() が探索する「中央・正方形クロップの GUIDE_FRAC 幅」ボックスを映像に重ねて描く。
  // ユーザーはこの枠にコードを収めれば、スキャナのガイド初期値と一致して検出が始まる。
  _ensureGuide() {
    if (this.guideEl || !this.video.parentElement) return;
    const el = document.createElement("div");
    el.style.cssText =
      "position:absolute;box-sizing:border-box;pointer-events:none;border:3px solid #f59e0b;" +
      "border-radius:6px;box-shadow:0 0 0 9999px rgba(0,0,0,0.28);transition:border-color .12s;";
    this.video.parentElement.appendChild(el);
    this.guideEl = el;
    this._positionGuide();
  }

  _positionGuide() {
    if (!this.guideEl) return;
    const vw = this.video.clientWidth, vh = this.video.clientHeight;
    if (!vw || !vh) return;
    // scan() は video 画素の中央正方形 (一辺 = min(w,h)) を切り出し、その GUIDE_FRAC 幅 ×
    // (コード高/幅) の中央ボックスをガイドにする。表示は等倍スケールなので client 座標でも同じ比。
    const ASPECT = 132 / 140; // 高さ/幅 (V1_DENSE 相当。V0=92/100 とほぼ同じ)
    const side = Math.min(vw, vh);
    const bw = side * VCODE_GUIDE_FRAC, bh = bw * ASPECT;
    const s = this.guideEl.style;
    s.width = `${bw}px`; s.height = `${bh}px`;
    s.left = `${(vw - bw) / 2}px`; s.top = `${(vh - bh) / 2}px`;
  }

  _setGuideLocked(locked) {
    if (this.guideEl) this.guideEl.style.borderColor = locked ? "#22c55e" : "#f59e0b";
  }

  _removeGuide() {
    if (this.guideEl) { this.guideEl.remove(); this.guideEl = null; }
  }

  stop() {
    if (this.rafId) cancelAnimationFrame(this.rafId);
    this.rafId = null;
    this.video.removeEventListener("loadedmetadata", this._onResize);
    window.removeEventListener("resize", this._onResize);
    this._removeGuide();
    if (this.stream) { this.stream.getTracks().forEach((t) => t.stop()); this.stream = null; }
  }

  _scan() {
    if (this.finished) return;
    this._positionGuide();
    const vw = this.video.videoWidth, vh = this.video.videoHeight;
    if (!vw || !vh) return;
    // 中央正方形を最大 SCAN_MAX に
    const crop = Math.min(vw, vh);
    const target = Math.min(SCAN_MAX, crop);
    const cx = (vw - crop) >> 1, cy = (vh - crop) >> 1;
    this.cap.width = target; this.cap.height = target;
    const ctx = this.cap.getContext("2d", { willReadFrequently: true });
    ctx.drawImage(this.video, cx, cy, crop, crop, 0, 0, target, target);
    const rgba = ctx.getImageData(0, 0, target, target).data;
    // RGBA → 輝度 Y
    const gray = new Uint8Array(target * target);
    for (let p = 0, q = 0; p < gray.length; p++, q += 4) {
      gray[p] = (rgba[q] * 77 + rgba[q + 1] * 150 + rgba[q + 2] * 29) >> 8;
    }
    this.frames++;
    this.stats.tick(gray);
    this._diag(crop, target);
    let report;
    try {
      report = this.rx.scan(gray, target, target, target, 0, VCODE_GUIDE_FRAC);
    } catch (_) { return; }
    this._setGuideLocked(report.detected);
    if (report.detected) {
      this.detected++;
      if (!this.dec) {
        try { this.dec = new FountainDecoder(report.oti); } catch (_) { return; }
      }
      const n = report.packetCount();
      let done = false;
      for (let i = 0; i < n; i++) {
        if (this.dec.addPacket(report.packet(i))) { done = true; break; }
      }
      this.onProgress({ frames: this.frames, detected: this.detected,
        blocks: report.blocksOk, blocksTotal: report.blocksTotal });
      if (done) {
        // エンドツーエンド CRC-32 検証。不一致 = 復元結果が破損 → デコーダを捨てて受信続行
        const payload = vcodeUnwrapPayload(this.dec.payload());
        if (!payload) {
          console.warn("[vcode-rx] 整合性エラー: 復元結果が破損。デコーダを再作成して受信続行");
          this.dec = null;
          return;
        }
        this._finish(payload);
      }
    } else {
      this.onProgress({ frames: this.frames, detected: this.detected, blocks: 0, blocksTotal: 0 });
    }
  }

  _finish(rawPayload) {
    this.finished = true;
    this.stop();
    // ファイル名/MIME ヘッダがあれば元の名前・種別で復元。無ければ従来どおり推測+タイムスタンプ名。
    const ts = new Date().toISOString().replace(/[:.]/g, "-").slice(0, 19);
    const meta = vcodeUnwrapFile(rawPayload);
    let data, name, mime;
    if (meta) {
      data = meta.data;
      const [ext, sm] = sniffType(data);
      name = meta.name || `vcode_${ts}.${ext}`;
      mime = meta.mime || sm;
    } else {
      data = rawPayload;
      const [ext, m] = sniffType(data);
      name = `vcode_${ts}.${ext}`;
      mime = m;
    }
    const blob = new Blob([data], { type: mime });
    this.onDone({ name, type: mime, size: data.length, blob });
  }
}

// スマホアプリ _sniffType と同一の型推定
function sniffType(b) {
  if (b.length > 3 && b[0] === 0xff && b[1] === 0xd8) return ["jpg", "image/jpeg"];
  if (b.length > 7 && b[0] === 0x89 && b[1] === 0x50) return ["png", "image/png"];
  if (b.length > 11 && b[8] === 0x57 && b[9] === 0x45 && b[10] === 0x42 && b[11] === 0x50) return ["webp", "image/webp"];
  // ISO-BMFF (オフセット 4 に 'ftyp'): HEIC/AVIF (iOS 写真の既定形式)
  if (b.length > 11 && b[4] === 0x66 && b[5] === 0x74 && b[6] === 0x79 && b[7] === 0x70) {
    const brand = String.fromCharCode(b[8], b[9], b[10], b[11]);
    if (["heic", "heix", "hevc", "heim", "heis", "mif1", "msf1"].includes(brand)) return ["heic", "image/heic"];
    if (brand === "avif" || brand === "avis") return ["avif", "image/avif"];
  }
  if (b.length > 3 && b[0] === 0x25 && b[1] === 0x50 && b[2] === 0x44 && b[3] === 0x46) return ["pdf", "application/pdf"];
  if (b.length > 1 && b[0] === 0x50 && b[1] === 0x4b) return ["zip", "application/zip"];
  const probe = b.subarray(0, 4096);
  let ctrl = 0;
  for (const c of probe) if (c < 9 || (c > 13 && c < 32) || c === 127) ctrl++;
  if (probe.length && ctrl / probe.length < 0.02) return ["txt", "text/plain;charset=utf-8"];
  return ["bin", "application/octet-stream"];
}
