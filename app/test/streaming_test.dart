// ストリーミング・プロトコルの検証: マルチブロック符号化 → データQR → 解析 → ブロック復号 →
// 再構成が元データに一致することを (カメラ無しで) 確認する。
import 'dart:typed_data';
import 'package:flutter_test/flutter_test.dart';
import 'package:vloom/src/rust/frb_generated.dart';
import 'package:vloom/src/rust/api/fountain.dart';
import 'package:vloom/protocol.dart';

void main() {
  setUpAll(() async => await RustLib.init());

  test('streaming multi-block encode → frame → decode → reconstruct', () {
    final total = 1200 * 1024; // ~1.2MB → 3 ブロック (512KB)
    final src = Uint8List(total);
    for (var i = 0; i < total; i++) {
      src[i] = (i * 131 + 7) & 0xff;
    }
    const packetSize = 300;
    final blockCount = (total / kBlockSize).ceil();

    Uint8List otiOf(int off, int len) {
      final b = Uint8List.sublistView(src, off, off + len);
      return FountainEncoder(
              payload: b, packetSize: packetSize, extraRepair: (len / packetSize * 0.3).ceil())
          .otiBytes();
    }

    final lastLen = total - (blockCount - 1) * kBlockSize;
    final manifest = StreamManifest(
      name: 'x.bin',
      type: 'application/octet-stream',
      totalSize: total,
      blockSize: kBlockSize,
      blockCount: blockCount,
      otiFull: otiOf(0, kBlockSize),
      otiLast: otiOf((blockCount - 1) * kBlockSize, lastLen),
    );

    // マニフェスト QR round-trip
    final m = StreamManifest.tryParse(manifest.toQr())!;
    expect(m.blockCount, blockCount);
    expect(m.totalSize, total);

    // 受信側再構成
    final out = Uint8List(total);
    final decoders = <int, FountainDecoder>{};
    final done = <int>{};

    for (var bi = 0; bi < blockCount; bi++) {
      final off = bi * kBlockSize;
      final len = m.blockLen(bi);
      final blockBytes = Uint8List.sublistView(src, off, off + len);
      final enc = FountainEncoder(
          payload: blockBytes, packetSize: packetSize, extraRepair: (len / packetSize * 0.3).ceil());
      for (var pi = 0; pi < enc.packetCount() && !done.contains(bi); pi++) {
        final qr = buildDataQr(bi, enc.packet(i: pi));
        final d = parseDataQr(qr)!;
        final dec = decoders.putIfAbsent(
            d.blockIndex, () => FountainDecoder(otiBytes: m.otiFor(d.blockIndex)));
        if (dec.addPacket(packet: Uint8List.fromList(d.packet))) {
          final rb = dec.payload();
          if (rb != null) {
            out.setRange(off, off + len, rb);
            done.add(bi);
          }
        }
      }
    }

    expect(done.length, blockCount);
    expect(out, equals(src));
  });
}
