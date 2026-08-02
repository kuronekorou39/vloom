// PC 送信: ファイルをブロック分割ストリーミングで アニメQR 表示する。
// File.slice() でブロック単位に読むため、GB 級でも RAM は 1 ブロック分で一定。
// フレーム構成はスマホアプリと同一 (20 フレームごとに 1 セルをマニフェストに)。

import { FountainEncoder } from "./pkg/vloom_core_wasm.js";
import { BLOCK_SIZE, PACKET_BY_GRID, StreamManifest, buildDataQr } from "./protocol.js";
import { drawQRInCell } from "./qr_util.js";

const MANIFEST_EVERY_FRAMES = 20;

function bytesToBinaryString(bytes) {
  let s = "";
  for (let i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i]);
  return s;
}

function makeQr(data, ec) {
  const qrcode = window.qrcode; // vendor/qrcode.js (classic script) が定義
  const qr = qrcode(0, ec); // 0 = 自動最小版
  qr.addData(bytesToBinaryString(data), "Byte");
  qr.make();
  return qr;
}

export class Sender {
  constructor({ canvas, onStatus }) {
    this.canvas = canvas;
    this.ctx = canvas.getContext("2d");
    this.onStatus = onStatus;
    this.running = false;
    this.seq = 0;
  }

  async start(file, gridStr, ec, fps) {
    const [rows, cols] = gridStr.split("x").map(Number);
    const ps = PACKET_BY_GRID[gridStr];
    const packetSize = ps === undefined ? 200 : ps;
    const total = file.size;
    const blockCount = Math.max(1, Math.ceil(total / BLOCK_SIZE));

    const readBlock = async (off, len) =>
      new Uint8Array(await file.slice(off, off + len).arrayBuffer());

    // 各ブロックの OTI (full/last)。repair はブロックのシンボル数の 30% (アプリと同一)。
    const repairOf = (len) => Math.ceil(len / packetSize * 0.3);
    const lastLen = total - (blockCount - 1) * BLOCK_SIZE;
    const lastBytes = await readBlock((blockCount - 1) * BLOCK_SIZE, lastLen);
    const otiLast = new FountainEncoder(lastBytes, packetSize, repairOf(lastLen)).otiBytes();
    let otiFull = otiLast;
    if (blockCount > 1) {
      const fullBytes = await readBlock(0, BLOCK_SIZE);
      otiFull = new FountainEncoder(fullBytes, packetSize, repairOf(BLOCK_SIZE)).otiBytes();
    }

    this.manifest = new StreamManifest({
      name: file.name, type: file.type || "application/octet-stream",
      totalSize: total, blockSize: BLOCK_SIZE, blockCount, otiFull, otiLast,
    });
    this.readBlock = readBlock;
    this.packetSize = packetSize;
    this.repairOf = repairOf;
    this.rows = rows; this.cols = cols; this.ec = ec;
    this.blockIdx = -1;
    this.enc = null;
    this.packetInBlock = 0;
    this.frameCount = 0;
    this.pass = 0;
    this.running = true;
    const mySeq = ++this.seq;
    this._loop(mySeq, fps);
  }

  async _nextDataPayload() {
    const m = this.manifest;
    if (this.enc === null || this.packetInBlock >= this.enc.packetCount()) {
      this.blockIdx = (this.blockIdx + 1) % m.blockCount;
      if (this.blockIdx === 0) this.pass++;
      const len = m.blockLen(this.blockIdx);
      const bytes = await this.readBlock(this.blockIdx * m.blockSize, len);
      this.enc = new FountainEncoder(bytes, this.packetSize, this.repairOf(len));
      this.packetInBlock = 0;
    }
    const packet = this.enc.packet(this.packetInBlock++);
    return buildDataQr(this.blockIdx, packet);
  }

  async _loop(mySeq, fps) {
    const interval = 1000 / fps;
    while (this.running && mySeq === this.seq) {
      const t0 = performance.now();
      const n = this.rows * this.cols;
      const showManifest = this.frameCount % MANIFEST_EVERY_FRAMES === 0;
      const { ctx, canvas } = this;
      ctx.fillStyle = "#fff";
      ctx.fillRect(0, 0, canvas.width, canvas.height);
      const cellW = canvas.width / this.cols;
      const cellH = canvas.height / this.rows;
      for (let cell = 0; cell < n; cell++) {
        const payload = (showManifest && cell === 0)
          ? this.manifest.toQr()
          : await this._nextDataPayload();
        const qr = makeQr(payload, this.ec);
        const r = Math.floor(cell / this.cols), c = cell % this.cols;
        drawQRInCell(ctx, qr, c * cellW, r * cellH, cellW, cellH);
      }
      if (mySeq !== this.seq) break;
      this.frameCount++;
      this.onStatus(
        `送信中 · ブロック ${Math.max(0, this.blockIdx) + 1}/${this.manifest.blockCount}` +
        ` · ${this.pass} 巡目 · frame ${this.frameCount}`);
      const dt = performance.now() - t0;
      if (dt < interval) await new Promise((res) => setTimeout(res, interval - dt));
    }
  }

  stop() {
    this.seq++;
    this.running = false;
  }
}
