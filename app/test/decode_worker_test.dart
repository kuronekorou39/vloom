// 復元ワーカー (別 isolate) の往復テスト。Rust の実体を呼ぶので、先に PC 用の
// ライブラリを作っておくこと: cd app/rust && cargo build --release
import 'dart:async';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:vloom/receive/decode_worker.dart';
import 'package:vloom/src/rust/api/fountain.dart';
import 'package:vloom/src/rust/frb_generated.dart';

/// vcode::wrap_payload と同じ: 先頭に CRC-32 (ISO-HDLC, big-endian)
Uint8List wrapPayload(Uint8List data) {
  var c = 0xFFFFFFFF;
  for (final b in data) {
    c ^= b;
    for (var k = 0; k < 8; k++) {
      c = (c & 1) != 0 ? (c >> 1) ^ 0xEDB88320 : c >> 1;
    }
  }
  c ^= 0xFFFFFFFF;
  return Uint8List.fromList([c >> 24 & 255, c >> 16 & 255, c >> 8 & 255, c & 255, ...data]);
}

void main() {
  setUpAll(() async => await RustLib.init());

  Future<(Uint8List?, bool)> run(Uint8List wire, {int chunk = 300}) async {
    final enc = FountainEncoder(payload: wire, packetSize: 42, extraRepair: wire.length ~/ 84);
    final done = Completer<(Uint8List?, bool)>();
    final w = await DecodeWorker.spawn(
      enc.otiBytes(),
      onDone: (p) => done.complete((p, false)),
      onCorrupt: () => done.complete((null, true)),
    );
    expect(w, isNotNull);
    // 1 巡目を半分落とし、2 巡目以降 (新しいリペア) で補う。1 枚ぶんずつ投げっぱなしで渡す
    var batch = <Uint8List>[];
    for (var i = 0; i < enc.packetCount() * 3 && !done.isCompleted; i++) {
      if (i < enc.packetCount() && i.isOdd) continue;
      batch.add(enc.packet(i: i));
      if (batch.length == chunk) {
        w!.feed(batch);
        batch = [];
        await Future<void>.delayed(Duration.zero);
      }
    }
    w!.feed(batch);
    final r = await done.future.timeout(const Duration(seconds: 60));
    w.dispose();
    return r;
  }

  test('別 isolate で復元し、CRC を外した中身を返す (複数ブロック)', () async {
    // 2MB 超 = 複数ソースブロック
    final data = Uint8List.fromList(List.generate(3 << 20, (i) => (i * 2654435761) >> 13 & 255));
    final (got, corrupt) = await run(wrapPayload(data));
    expect(corrupt, isFalse);
    expect(got, equals(data));
  });

  test('CRC が合わなければ onCorrupt', () async {
    final data = Uint8List.fromList(List.generate(5000, (i) => i & 255));
    final wire = wrapPayload(data)..[0] ^= 0xFF; // CRC を壊す
    final (_, corrupt) = await run(wire);
    expect(corrupt, isTrue);
  });
}
