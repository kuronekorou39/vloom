import 'dart:typed_data';

/// 受信したパケットの「被覆」を数える (進捗・カバレッジ格子に使う)。
///
/// RaptorQ は 2MB 前後を超えるとオブジェクトを複数のソースブロックに割り、パケット ID は
/// SBN (ブロック番号 1 byte) + ESI (ブロック内番号 3 byte)。ESI だけで数えると別ブロックの
/// 同じ番号が重なって、大きいファイルで進捗が途中から伸びなくなる。
///
/// 進捗はブロックごとに「必要数 K_b を超えた分は数えない」で合計する。復元にはブロックごとに
/// K_b 個 (+ わずか) が要るので、これが本当に残りを表す。
///
/// 描画用の位置は、ソースは全体の通し番号 (ブロック b の先頭位置 + ESI)、リペアは Kt 以降に
/// ブロックをまたいで交互に置く。単一ブロックなら従来どおり ESI そのものになる。
/// PWA 版 (web/pwa/coverage.js) と同じ計算。
class PacketCoverage {
  PacketCoverage._(this.kt, this.z, this._k, this._offset, this.totalBytes, this.symbolSize)
      : _count = List<int>.filled(z, 0),
        bucketW = kt <= 1024 ? 1 : (kt + 1023) ~/ 1024,
        cap = kt;

  /// 12 byte の OTI から作る。
  factory PacketCoverage.fromOti(Uint8List oti) {
    // OTI: 転送長 40bit (0-4) / 予約 (5) / シンボル長 u16 (6-7) / Z (8) / N u16 (9-10) / Al (11)
    var len = 0;
    for (var i = 0; i < 5; i++) {
      len = len * 256 + oti[i];
    }
    final symbol = (oti[6] << 8) | oti[7];
    final z = oti[8] < 1 ? 1 : oti[8];
    final kt = symbol > 0 ? (len + symbol - 1) ~/ symbol : 0;
    // RFC 6330 の分割: 先頭 ZL 個のブロックが KL シンボル、残りが KS シンボル
    final kl = (kt + z - 1) ~/ z;
    final ks = kt ~/ z;
    final zl = kt - ks * z;
    final k = List<int>.generate(z, (b) => b < zl ? kl : ks);
    final offset = List<int>.filled(z, 0);
    for (var b = 1; b < z; b++) {
      offset[b] = offset[b - 1] + k[b - 1];
    }
    return PacketCoverage._(kt, z, k, offset, len, symbol);
  }

  /// 復元に要るソースシンボル数 (全ブロックの合計)。
  final int kt;

  /// 送られてくるデータ全体の長さ (バイト) と、パケット 1 つが運ぶ長さ
  final int totalBytes;
  final int symbolSize;

  /// 受信できた量 (バイト)。進捗の表示用で、[useful] をバイトに直したもの
  int get receivedBytes {
    final b = useful * symbolSize;
    return b > totalBytes ? totalBytes : b;
  }
  final int z;
  final List<int> _k;
  final List<int> _offset;
  final List<int> _count;
  final Set<int> _positions = {};

  /// 描画用の度数分布。数十万パケットを毎回なめないよう、位置を固定幅でまとめて数える。
  final int bucketW;
  final List<int> hist = [];

  /// 届いた中で最大の位置 (+1)。描画範囲に使う。
  int cap;

  /// ブロックごとに K_b で頭打ちした受信数の合計。これが [kt] に届くと復元できる見込み。
  int useful = 0;

  /// 重複を除いた受信数 (リペアの余剰も含む)。
  int get distinct => _positions.length;

  /// パケット 1 つを数える。新しい位置なら true。
  bool add(Uint8List p) {
    if (p.length < 4) return false;
    final b = p[0];
    if (b >= z) return false;
    final esi = (p[1] << 16) | (p[2] << 8) | p[3];
    final kb = _k[b];
    final pos = esi < kb ? _offset[b] + esi : kt + (esi - kb) * z + b;
    if (!_positions.add(pos)) return false;
    final bi = pos ~/ bucketW;
    while (hist.length <= bi) {
      hist.add(0);
    }
    hist[bi]++;
    if (pos + 1 > cap) cap = pos + 1;
    if (_count[b]++ < kb) useful++;
    return true;
  }
}
