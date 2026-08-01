// 計測用のテスト画像。
//
// 条件 (格子・階調・fps・解像度) を振って比べるとき、毎回ファイルを選び直すと
// 手間な上にデータが変わって比較にならない。同梱の固定画像を使う。
//
// バイト列ではなく画像なのは、受信側で開いた瞬間に「壊れているか正常か」が
// 目で分かるため。画像内にサイズを焼き込んであるので、どの条件のデータが
// 届いたのかも一目で判別できる。


import 'package:flutter/services.dart';

/// 選べる計測データ (表示ラベル, アセットパス, 実バイト数)。
/// 実バイト数は生成時の実測値で、UI の表示に使う。
const kTestImages = <(String, String, int)>[
  ('100KB', 'assets/testdata/test-100KB.jpg', 102431),
  ('500KB', 'assets/testdata/test-500KB.jpg', 512781),
  ('1MB', 'assets/testdata/test-1MB.jpg', 1048716),
  ('2MB', 'assets/testdata/test-2MB.jpg', 2091447),
];

/// 同梱の計測用画像を読む
Future<Uint8List> loadTestImage(String asset) async {
  final data = await rootBundle.load(asset);
  return data.buffer.asUint8List(data.offsetInBytes, data.lengthInBytes);
}

/// 受信側に渡すファイル名 (履歴と統計で条件を識別できるようにする)
String testImageName(String asset) => asset.split('/').last;

/// 計測用画像として送られたものか (ファイル名で判別する)
bool isTestImage(String name) =>
    name.startsWith('test-') && name.endsWith('.jpg');
