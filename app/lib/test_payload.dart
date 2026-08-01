// 計測用のテストペイロード。
//
// 条件 (格子・階調・fps・解像度) を振って比べるとき、毎回ファイルを選び直すと
// 手間な上にデータが変わって比較にならない。決定論的に生成した固定データを使う。
//
// 中身は擬似乱数で埋める。実ファイル (JPEG など) は既に圧縮されていて
// エントロピーが高いので、それに近い条件で測るため。ゼロ埋めだと現実と乖離する。

import 'dart:typed_data';

/// テストデータであることを示すマジック。受信側が判別して表示に使う。
const _kMagic = [0x56, 0x4c, 0x4d, 0x54]; // "VLMT"

/// 選べるサイズ (表示ラベル, バイト数)
const kTestSizes = <(String, int)>[
  ('100KB', 100 * 1024),
  ('500KB', 500 * 1024),
  ('1MB', 1024 * 1024),
  ('2MB', 2 * 1024 * 1024),
];

/// 指定サイズの決定論的テストデータを作る。
/// 先頭 8 byte = マジック(4) + 全長(4, big-endian)、以降は擬似乱数。
Uint8List makeTestPayload(int bytes) {
  final out = Uint8List(bytes);
  if (bytes >= 8) {
    out.setRange(0, 4, _kMagic);
    out[4] = (bytes >> 24) & 0xff;
    out[5] = (bytes >> 16) & 0xff;
    out[6] = (bytes >> 8) & 0xff;
    out[7] = bytes & 0xff;
  }
  // 線形合同法。同じサイズなら常に同じ列になる (再現性)
  var x = 0x12345678;
  for (var i = bytes >= 8 ? 8 : 0; i < bytes; i++) {
    x = (x * 1103515245 + 12345) & 0x7fffffff;
    out[i] = (x >> 16) & 0xff;
  }
  return out;
}

/// テストデータなら宣言サイズを返す。違えば null。
/// 中身の正しさは vcode の E2E CRC-32 が既に保証しているので、ここでは判別だけ行う。
int? testPayloadSize(Uint8List data) {
  if (data.length < 8) return null;
  for (var i = 0; i < 4; i++) {
    if (data[i] != _kMagic[i]) return null;
  }
  final declared =
      (data[4] << 24) | (data[5] << 16) | (data[6] << 8) | data[7];
  return declared == data.length ? declared : null;
}

/// 計測結果と紐づけやすい固定のファイル名
String testPayloadName(int bytes) {
  final label = kTestSizes.firstWhere((e) => e.$2 == bytes,
      orElse: () => ('${bytes}B', bytes));
  return 'vloom-test-${label.$1}.bin';
}
