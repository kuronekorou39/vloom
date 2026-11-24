import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:vloom/receive/packet_coverage.dart';

/// 転送長 len・シンボル長 t・ブロック数 z の OTI を作る。
Uint8List oti(int len, int t, int z) {
  final b = Uint8List(12);
  for (var i = 4; i >= 0; i--) {
    b[i] = len & 0xff;
    len >>= 8;
  }
  b[6] = t >> 8;
  b[7] = t & 0xff;
  b[8] = z;
  b[10] = 1;
  b[11] = 1;
  return b;
}

Uint8List pkt(int sbn, int esi) =>
    Uint8List.fromList([sbn, esi >> 16, (esi >> 8) & 0xff, esi & 0xff, 0]);

void main() {
  test('複数ブロックでは SBN を区別して数え、ブロックごとに必要数で頭打ちする', () {
    // Kt = 10, Z = 3 → K = [4, 3, 3]
    final c = PacketCoverage.fromOti(oti(100, 10, 3));
    expect(c.kt, 10);
    // 同じ ESI でもブロックが違えば別物
    expect(c.add(pkt(0, 0)), isTrue);
    expect(c.add(pkt(1, 0)), isTrue);
    expect(c.add(pkt(1, 0)), isFalse);
    expect(c.useful, 2);
    // ブロック 1 を必要数 (3) を超えて受け取っても進捗は 3 で止まる
    for (var e = 1; e < 6; e++) {
      c.add(pkt(1, e));
    }
    expect(c.useful, 1 + 3);
    expect(c.distinct, 7);
  });

  test('単一ブロックなら位置は ESI そのもの', () {
    final c = PacketCoverage.fromOti(oti(100, 10, 1));
    c.add(pkt(0, 12));
    expect(c.cap, 13);
    expect(c.useful, 1);
  });
}
