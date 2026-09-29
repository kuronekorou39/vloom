// 送信画面のレイアウトと、送るものの選択の確認。
//
// 狭い端末幅で描いてはみ出し (RenderFlex overflow) が出ないことを確かめる。
// テスト中のはみ出しは例外として上がるので、描けた時点で合格になる。
// Rust コアは送信開始まで呼ばれないので、初期化なしで描ける。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vloom/preset.dart';
import 'package:vloom/vcode_send_screen.dart';

Future<void> pumpSend(WidgetTester tester, Size size) async {
  tester.view.physicalSize = size;
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);
  await tester.pumpWidget(MaterialApp(
    theme: ThemeData(brightness: Brightness.dark, useMaterial3: true),
    home: const Scaffold(body: VcodeSendScreen()),
  ));
}

void main() {
  for (final size in const [Size(320, 568), Size(360, 740), Size(412, 915)]) {
    testWidgets('送信画面が ${size.width.toInt()} 幅ではみ出さない', (tester) async {
      await pumpSend(tester, size);
      for (final tab in ['テキスト', '計測データ', 'ファイル']) {
        await tester.tap(find.text(tab));
        await tester.pumpAndSettle();
      }
      await tester.scrollUntilVisible(find.text('詳細設定 (通常は触らない)'), 200);
      await tester.tap(find.text('詳細設定 (通常は触らない)'));
      await tester.pumpAndSettle();
    });
  }

  testWidgets('何も選んでいなければ送信できない', (tester) async {
    await pumpSend(tester, const Size(360, 740));
    expect(find.text('ファイルが選ばれていません'), findsOneWidget);
    await tester.scrollUntilVisible(find.text('送信開始'), 200);
    final button = tester.widget<FilledButton>(
        find.ancestor(of: find.text('送信開始'), matching: find.bySubtype<FilledButton>()));
    expect(button.onPressed, isNull);
  });

  testWidgets('テキストを入れたままファイルに切り替えると、送られないと表示する', (tester) async {
    await pumpSend(tester, const Size(360, 740));
    await tester.tap(find.text('テキスト'));
    await tester.pumpAndSettle();
    await tester.enterText(find.byType(TextField), 'hello');
    await tester.pumpAndSettle();
    expect(find.text('テキスト · 5B'), findsOneWidget);

    await tester.tap(find.text('ファイル'));
    await tester.pumpAndSettle();
    expect(find.text('入力済みのテキストは送られません (送るのは「ファイル」)'), findsOneWidget);
  });

  test('所要時間の目安は実測から出す', () {
    // 13x18 @20fps の実測は 151.7 KB/s (理論は 192)
    expect(measuredKbps('13x18', 1, 20), closeTo(151.7, 0.01));
    // fps が違えば最も近い実測から比例させる
    expect(measuredKbps('11x14', 1, 10), closeTo(40.25, 0.01));
    // 60Hz 画面の上限を超えるぶんは伸びに数えない
    expect(measuredKbps('13x18', 1, 60), closeTo(204, 0.01));
    // 実測の無い組み合わせは出さない
    expect(measuredKbps('18x13', 1, 20), isNull);
    expect(measuredKbps('13x18', 2, 20), isNull);

    expect(fmtEstimate(0.4), '1 秒未満');
    expect(fmtEstimate(6.91), '約 6.9 秒');
    expect(fmtEstimate(42.4), '約 42 秒');
    expect(fmtEstimate(84), '約 1 分 20 秒');
    expect(fmtEstimate(118), '約 2 分');
  });
}
