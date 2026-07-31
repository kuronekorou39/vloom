// Fountain コア (Rust FFI) の encode→decode 往復テスト。
// M1 の疎通確認: Dart から Rust の FountainEncoder/Decoder を呼び、payload が復元できること。
import 'dart:typed_data';
import 'package:flutter_test/flutter_test.dart';
import 'package:vloom/src/rust/frb_generated.dart';
import 'package:vloom/src/rust/api/fountain.dart';

void main() {
  setUpAll(() async => await RustLib.init());

  test('fountain encode → decode round-trip', () {
    final payload =
        Uint8List.fromList(List.generate(5000, (i) => (i * 37 + 11) & 0xff));

    final enc = FountainEncoder(payload: payload, packetSize: 300, extraRepair: 20);
    final oti = enc.otiBytes();
    final count = enc.packetCount();
    expect(oti.length, 12);
    expect(count, greaterThan(0));

    final dec = FountainDecoder(otiBytes: oti);
    Uint8List? recovered;
    for (int i = 0; i < count && recovered == null; i++) {
      if (dec.addPacket(packet: enc.packet(i: i))) {
        recovered = dec.payload();
      }
    }

    expect(recovered, isNotNull);
    expect(recovered, equals(payload));
  });
}
