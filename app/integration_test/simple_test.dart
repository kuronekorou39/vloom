// アプリが起動し、Rust コアの初期化まで通ることを確認する smoke test。
// (元は flutter_rust_bridge のテンプレートのまま残っており、存在しない MyApp と
//  "Hello, Tom!" を参照して壊れていた)

import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:beyond_qr/main.dart';
import 'package:beyond_qr/src/rust/api/fountain.dart';
import 'package:beyond_qr/src/rust/frb_generated.dart';
import 'package:integration_test/integration_test.dart';

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  setUpAll(() async => await RustLib.init());

  testWidgets('アプリが起動してタイトルが出る', (WidgetTester tester) async {
    await tester.pumpWidget(const VloomApp());
    await tester.pump();
    expect(find.text('Vloom'), findsWidgets);
  });

  test('Fountain コアが往復する', () {
    final payload = Uint8List.fromList(
        List<int>.generate(3000, (i) => (i * 37 + 11) & 0xff));
    final enc = FountainEncoder(payload: payload, packetSize: 300, extraRepair: 10);
    final dec = FountainDecoder(otiBytes: enc.otiBytes());
    Uint8List? recovered;
    for (var i = 0; i < enc.packetCount() && recovered == null; i++) {
      if (dec.addPacket(packet: enc.packet(i: i))) recovered = dec.payload();
    }
    expect(recovered, isNotNull);
    expect(recovered!.length, payload.length);
    expect(recovered.first, payload.first);
    expect(recovered.last, payload.last);
  });
}
