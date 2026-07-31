// PC 受信: カメラ (getUserMedia) → jsQR (タイル分割) → ブロックごと Fountain 復元。
// スマホアプリと同一のストリーミング形式 (manifest/data)。復元したブロックはメモリに
// 保持し、完成で Blob としてダウンロード保存する (PC は RAM に余裕がある前提の v1。
// 将来 File System Access API でディスク直書きにすれば GB 級も RAM 一定にできる)。

import { FountainDecoder } from "./pkg/vloom_core_wasm.js";
import { FRAME_MANIFEST, FRAME_DATA, StreamManifest, parseDataQr } from "./protocol.js";
import { detectQRCodesGrid } from "./qr_util.js";
import { openCamera, ScanStats, cameraInfoText, lumaText } from "./camera.js";

const DIAG_INTERVAL_MS = 500;

const hex = (b, n) => {
  let s = "";
  const len = Math.min(n, b.length);
  for (let i = 0; i < len; i++) s += b[i].toString(16).padStart(2, "0");
  return s;
};

export class Receiver {
  constructor({ video, onProgress, onDone, onError, onDiag }) {
    this.video = video;
    this.onProgress = onProgress;
    this.onDone = onDone;
    this.onError = onError;
    this.onDiag = onDiag || (() => {});
    this.captureCanvas = document.createElement("canvas");
    this.stream = null;
    this.rafId = null;
    this.grid = "1x2";
  }

  async start(gridStr, deviceId) {
    this.grid = gridStr;
    this._reset();
    this.stream = await openCamera(deviceId);
    this.video.srcObject = this.stream;
    await this.video.play();
    const loop = () => {
      if (!this.stream) return;
      this._scanFrame();
      this.rafId = requestAnimationFrame(loop);
    };
    this.rafId = requestAnimationFrame(loop);
  }

  _reset() {
    this.stats = new ScanStats();
    this._lastDiag = 0;
    this.manifest = null;
    this.blocks = null;         // Uint8Array[] (復元済みブロック)
    this.doneBlocks = new Set();
    this.decoders = new Map();  // blockIndex -> FountainDecoder
    this.seen = new Map();      // blockIndex -> Set(packetKey)
    this.finished = false;
  }

  stop() {
    if (this.rafId) cancelAnimationFrame(this.rafId);
    this.rafId = null;
    if (this.stream) {
      this.stream.getTracks().forEach((t) => t.stop());
      this.stream = null;
    }
  }

  _scanFrame() {
    if (this.finished) return;
    const vw = this.video.videoWidth, vh = this.video.videoHeight;
    if (!vw || !vh) return;
    const crop = Math.min(vw, vh);
    const cx = (vw - crop) >> 1, cy = (vh - crop) >> 1;
    const [rows, cols] = this.grid.split("x").map(Number);
    // grid 別に必要十分な解像度 (アプリ側と同じ考え方: 1x1 は crop 実寸、多グリッドはタイル基準)
    const single = Math.max(rows, cols) === 1;
    const target = single
      ? Math.min(1280, Math.max(640, crop))
      : Math.min(1440, Math.max(720, 460 * Math.max(rows, cols)));
    const cv = this.captureCanvas;
    cv.width = target; cv.height = target;
    const ctx = cv.getContext("2d", { willReadFrequently: true });
    ctx.drawImage(this.video, cx, cy, crop, crop, 0, 0, target, target);
    const img = ctx.getImageData(0, 0, target, target);
    this.stats.tick(img.data, 4);
    this._diag(crop, target);

    const codes = detectQRCodesGrid(img, rows, cols);
    let changed = false;
    for (const bytes of codes) {
      if (!bytes || bytes.length < 2) continue;
      if (bytes[0] === FRAME_MANIFEST) {
        if (!this.manifest) {
          const m = StreamManifest.tryParse(bytes);
          if (m) {
            this.manifest = m;
            this.blocks = new Array(m.blockCount).fill(null);
            changed = true;
          }
        }
      } else if (bytes[0] === FRAME_DATA) {
        if (this.manifest && this._feedData(bytes)) changed = true;
      }
    }
    if (changed) this._report();
  }

  _feedData(bytes) {
    const d = parseDataQr(bytes);
    if (!d) return false;
    const m = this.manifest;
    const idx = d.blockIndex;
    if (idx < 0 || idx >= m.blockCount || this.doneBlocks.has(idx)) return false;
    let seen = this.seen.get(idx);
    if (!seen) { seen = new Set(); this.seen.set(idx, seen); }
    const key = hex(d.packet, 4);
    if (seen.has(key)) return false;
    seen.add(key);
    let dec = this.decoders.get(idx);
    if (!dec) {
      try { dec = new FountainDecoder(m.otiFor(idx)); } catch (_) { return false; }
      this.decoders.set(idx, dec);
    }
    try {
      if (dec.addPacket(new Uint8Array(d.packet))) {
        const payload = dec.payload();
        if (payload) {
          this.blocks[idx] = payload;
          this.doneBlocks.add(idx);
          this.decoders.delete(idx);
          this.seen.delete(idx);
          if (this.doneBlocks.size === m.blockCount) this._finish();
        }
      }
    } catch (_) {}
    return true;
  }

  // カメラ実解像度・スキャン fps・明るさ の実測。読めないときの切り分け用。
  _diag(crop, target) {
    const now = performance.now();
    if (now - this._lastDiag < DIAG_INTERVAL_MS) return;
    this._lastDiag = now;
    const size = crop === target ? `${target}px` : `${crop}→${target}px`;
    this.onDiag(
      `${cameraInfoText(this.stream)}\n` +
      `スキャン ${size} · ${this.stats.fps.toFixed(1)} fps · ${lumaText(this.stats)}`
    );
  }

  _report() {
    const m = this.manifest;
    this.onProgress({
      name: m ? m.name : null,
      size: m ? m.totalSize : 0,
      done: this.doneBlocks.size,
      total: m ? m.blockCount : 0,
      inflight: this.decoders.size,
    });
  }

  _finish() {
    this.finished = true;
    const m = this.manifest;
    const blob = new Blob(this.blocks, { type: m.type });
    this.stop();
    this.onDone({ name: m.name, type: m.type, size: m.totalSize, blob });
  }
}
