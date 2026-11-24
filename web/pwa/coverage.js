// 受信したパケットの「被覆」を数える (進捗・停滞判定・被覆帯の描画に使う)。
//
// RaptorQ は 2MB 前後を超えるとオブジェクトを複数のソースブロックに割り、パケット ID は
// SBN (ブロック番号 1 byte) + ESI (ブロック内番号 3 byte)。ESI だけで数えると別ブロックの
// 同じ番号が重なって、大きいファイルで進捗が途中から伸びなくなる。
//
// 進捗はブロックごとに「必要数 K_b を超えた分は数えない」で合計する。復元にはブロックごとに
// K_b 個 (+ わずか) が要るので、これが本当に残りを表す。
//
// 描画用の位置は、ソースは全体の通し番号 (ブロック b の先頭位置 + ESI)、リペアは Kt 以降に
// ブロックをまたいで交互に置く。単一ブロックなら従来どおり ESI そのものになる。
// Flutter 版 (app/lib/packet_coverage.dart) と同じ計算。

export class PacketCoverage {
  /** @param {Uint8Array} oti 12 byte の OTI */
  constructor(oti) {
    // OTI: 転送長 40bit (0-4) / 予約 (5) / シンボル長 u16 (6-7) / Z (8) / N u16 (9-10) / Al (11)
    const len = oti[0] * 2 ** 32 + ((oti[1] << 24) >>> 0) + (oti[2] << 16) + (oti[3] << 8) + oti[4];
    const symbol = (oti[6] << 8) | oti[7];
    const z = Math.max(1, oti[8]);
    this.kt = symbol > 0 ? Math.ceil(len / symbol) : 0;
    // RFC 6330 の分割: 先頭 ZL 個のブロックが KL シンボル、残りが KS シンボル
    const kl = Math.ceil(this.kt / z);
    const ks = Math.floor(this.kt / z);
    const zl = this.kt - ks * z;
    this.z = z;
    this.k = new Uint32Array(z);
    this.offset = new Float64Array(z);
    let off = 0;
    for (let b = 0; b < z; b++) {
      this.k[b] = b < zl ? kl : ks;
      this.offset[b] = off;
      off += this.k[b];
    }
    this.count = new Uint32Array(z);
    /** 重複判定用の位置の集合 */
    this.positions = new Set();
    // 描画用の度数分布。数十万パケットを毎回なめないよう、位置を固定幅でまとめて数える
    this.bucketW = Math.max(1, Math.ceil(this.kt / 1024));
    this.hist = [];
    /** 届いた中で最大の位置 (+1)。描画範囲に使う */
    this.cap = this.kt;
    /** ブロックごとに K_b で頭打ちした受信数の合計。これが kt に届くと復元できる見込み */
    this.useful = 0;
  }

  /** パケット 1 つを数える。新しい位置なら true。 */
  add(pkt) {
    if (pkt.length < 4) return false;
    const b = pkt[0];
    if (b >= this.z) return false;
    const esi = (pkt[1] << 16) | (pkt[2] << 8) | pkt[3];
    const kb = this.k[b];
    const pos = esi < kb ? this.offset[b] + esi : this.kt + (esi - kb) * this.z + b;
    if (this.positions.has(pos)) return false;
    this.positions.add(pos);
    const bi = Math.floor(pos / this.bucketW);
    this.hist[bi] = (this.hist[bi] || 0) + 1;
    if (pos + 1 > this.cap) this.cap = pos + 1;
    if (this.count[b]++ < kb) this.useful++;
    return true;
  }
}
