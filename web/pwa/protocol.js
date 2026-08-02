// チャンク・ストリーミング転送プロトコル (スマホアプリの lib/protocol.dart とバイト互換)。
//
// ファイルを固定 BLOCK_SIZE のブロックに分割し、各ブロックを独立に Fountain 符号化する。
// QR には 1 バイトの種別プレフィックスを付ける:
//   - 0x01 マニフェスト: [0x01][utf8(JSON {n,t,s,bs,bc,of,ol})]
//   - 0x02 データ:       [0x02][blockIndex u32LE][fountain packet]
// 全 full ブロックの OTI は共通 (of)、最後のブロックだけ別 (ol)。base64 で JSON に載せる。

export const BLOCK_SIZE = 512 * 1024;
export const FRAME_MANIFEST = 0x01;
export const FRAME_DATA = 0x02;

const te = new TextEncoder();
const td = new TextDecoder();

function b64encode(bytes) {
  let s = "";
  for (let i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i]);
  return btoa(s);
}
function b64decode(str) {
  const s = atob(str);
  const out = new Uint8Array(s.length);
  for (let i = 0; i < s.length; i++) out[i] = s.charCodeAt(i);
  return out;
}

export class StreamManifest {
  constructor({ name, type, totalSize, blockSize, blockCount, otiFull, otiLast }) {
    this.name = name;
    this.type = type;
    this.totalSize = totalSize;
    this.blockSize = blockSize;
    this.blockCount = blockCount;
    this.otiFull = otiFull;
    this.otiLast = otiLast;
  }

  get lastBlockSize() { return this.totalSize - (this.blockCount - 1) * this.blockSize; }
  otiFor(index) { return index === this.blockCount - 1 ? this.otiLast : this.otiFull; }
  blockLen(index) { return index === this.blockCount - 1 ? this.lastBlockSize : this.blockSize; }

  toQr() {
    const json = JSON.stringify({
      n: this.name, t: this.type, s: this.totalSize,
      bs: this.blockSize, bc: this.blockCount,
      of: b64encode(this.otiFull), ol: b64encode(this.otiLast),
    });
    const body = te.encode(json);
    const out = new Uint8Array(1 + body.length);
    out[0] = FRAME_MANIFEST;
    out.set(body, 1);
    return out;
  }

  static tryParse(bytes) {
    if (!bytes || bytes.length < 2 || bytes[0] !== FRAME_MANIFEST) return null;
    try {
      const j = JSON.parse(td.decode(bytes.subarray(1)));
      return new StreamManifest({
        name: j.n !== undefined && j.n !== null ? j.n : "data",
        type: j.t !== undefined && j.t !== null ? j.t : "application/octet-stream",
        totalSize: j.s, blockSize: j.bs, blockCount: j.bc,
        otiFull: b64decode(j.of), otiLast: b64decode(j.ol),
      });
    } catch (_) {
      return null;
    }
  }
}

/** データ QR を組み立てる: [0x02][blockIndex u32LE][packet]。 */
export function buildDataQr(blockIndex, packet) {
  const out = new Uint8Array(1 + 4 + packet.length);
  out[0] = FRAME_DATA;
  new DataView(out.buffer).setUint32(1, blockIndex, true);
  out.set(packet, 5);
  return out;
}

/** データ QR を解析。マニフェスト等なら null。 */
export function parseDataQr(bytes) {
  if (!bytes || bytes.length < 5 || bytes[0] !== FRAME_DATA) return null;
  const idx = new DataView(bytes.buffer, bytes.byteOffset).getUint32(1, true);
  return { blockIndex: idx, packet: bytes.subarray(5) };
}

/** grid ごとの実測ベスト packetSize (アプリと同一。EC=M / 版=自動 前提)。 */
export const PACKET_BY_GRID = { "1x1": 540, "1x2": 300, "2x2": 180, "2x3": 160, "3x3": 140 };
