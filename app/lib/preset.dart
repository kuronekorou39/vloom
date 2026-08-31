// 送受信のプリセット。
//
// 格子・階調・fps・解像度を個別に合わせるのは現実的でない (送受信で食い違うと
// 何が原因か分からなくなる)。プリセットを 1 つ選べば両側の設定が決まるようにする。
//
// fps の上限は受信カメラの実効フレームレートで決まる。実測 (Pixel 9a) では
// 23〜25fps 出ているので送信 20fps までは 1 枚ずつ拾えるが、30fps にすると
// 1 枚の写真に 2 フレームが混ざってデータブロックが全滅する (ローリングシャッターで
// センサーは上から下へ順次読み出すため。コーナーとヘッダは画像の上下端なので
// 揃うことがあり、「追従中なのに blocks=0/42」になる)。
// 20fps を超えて稼ぐ道はないので、そこから先は 1 フレームあたりの容量で稼ぐ。

import 'package:camera/camera.dart';

/// bpc ごとの RaptorQ packet_size (Layout.block = 20 前提で block_payload_len - 4)
int packetSizeFor(int bpc) => bpc == 2 ? 92 : 42;

class VcodePreset {
  const VcodePreset({
    required this.name,
    required this.description,
    required this.grid,
    required this.bpc,
    required this.fps,
    required this.preset,
  });

  /// 表示名
  final String name;

  /// 何を狙う設定か
  final String description;

  /// ブロック格子 ('7x6' 等)。受信側もこの値に固定して探索を 1 候補に絞る
  final String grid;

  /// 1 セルあたりのビット数 (1=白黒, 2=輝度4値)
  final int bpc;

  /// 送信フレームレート
  final int fps;

  /// 受信カメラの解像度。密な格子ほど px/セル が要る
  final ResolutionPreset preset;

  /// 理論スループット (KB/s)。取りこぼしと符号化冗長は含まない
  double get theoreticalKbps {
    final p = grid.split('x');
    final blocks = int.parse(p[0]) * int.parse(p[1]);
    return blocks * packetSizeFor(bpc) * fps / 1024;
  }

  /// この格子に必要なセル幅 (grid_w × block)
  int get cellsWide => int.parse(grid.split('x')[0]) * 20;
}

/// 送信面は**平面ディスプレイであること**。曲面ディスプレイでは成立しない。
/// 位置合わせは四隅から射影変換を起こすので、四隅は定義上ぴったり合うが、面が
/// 曲がっていると内側のサンプリングがずれる。実測では曲面 (曲面 MSI) で
/// 「検出 133 回に対し回収パケット 5 個」まで落ち、同条件の平面 (BenQ) では
/// 「検出 96 回で回収ブロック 3854」と全く別の結果になった。
///
/// 選べるプリセット。上から順に「まず通す」→「速度を狙う」。
///
/// 輝度 4 値 (bpc=2) は復号できるが、同じ格子の 1bit より遅い。実測 (2026-08-28,
/// 全格子・画角を揃えた状態) では 4 値の最良が 7x6/10fps の 22.8 KB/s で、
/// 1bit の最良 11x10/20fps の 60 KB/s に遠く及ばない。レベルの分離自体は
/// できていて (輝度ヒストグラムは 4 峰がきれいに分かれる)、効いているのは
/// 1 セルあたりのマージンが 1bit の 1/3 しかないこと。ブロックは 400 セルが
/// 全部合って初めて回収できるので、セル単位のわずかな誤りが満点率を潰す
/// (4 値の満点ブロック率は 0-10%、1bit は 50-95%)。
/// したがって容量は階調ではなくセル数 (格子) で稼ぐ。QR がファインダ・EC 領域に
/// 面積を取られるのに対し、vcode は格子全部がデータなので、同じ画角でも
/// セル数を増やせる。
/// 解像度はすべて max に統一してある。ResolutionPreset を下げると画素数だけでなく
/// 画角が変わるため、プリセットを跨いだ比較が成立しなくなる。Pixel 9a では
/// veryHigh (1920x1080 / 16:9) がセンサー中央の切り出しになり、max (1600x1200 /
/// 4:3) より画角が狭い = 実質ズームされる。同じ三脚・同じ距離のまま切り替えると、
/// max で画面に収まっていたコードが veryHigh でははみ出し、四隅マーカーが写らず
/// 検出 0 になる (実測: 同一被写体で max は 2.67 秒で完走、veryHigh は 0/70)。
/// 画素数は 192 万 vs 207 万でほぼ同じなので、下げる利点がない。
///
/// 送信 fps は 20 が上限。カメラの実効フレームレートが 23-25 fps なので、
/// 20 までは 1 枚ずつ拾えるが、30 にすると混ざって満点ブロック率が落ちる
/// (11x10 で 43% -> 3%、スループットも 60 -> 33 KB/s に下がる)。
///
const kPresets = <VcodePreset>[
  VcodePreset(
    name: '確実',
    description: 'まずここから。100 セル幅で確実に通す。実測 9-10 KB/s',
    grid: '5x4',
    bpc: 1,
    fps: 20,
    preset: ResolutionPreset.max,
  ),
  VcodePreset(
    name: '標準',
    description: '140 セル幅。実測 22 KB/s',
    grid: '7x6',
    bpc: 1,
    fps: 20,
    preset: ResolutionPreset.max,
  ),
  VcodePreset(
    name: '高速',
    description: '密度で稼ぐ。180 セル幅。実測 38-41 KB/s',
    grid: '9x8',
    bpc: 1,
    fps: 20,
    preset: ResolutionPreset.max,
  ),
  VcodePreset(
    name: '限界',
    description: '220 セル幅。実測 60 KB/s。縦長のほうが速い',
    grid: '11x10',
    bpc: 1,
    fps: 20,
    preset: ResolutionPreset.max,
  ),
  VcodePreset(
    // 受信はスマホ縦持ちなので、カメラ視野は 3:4 (1200x1600) になる。正方形に
    // 近い格子だと幅を合わせた時点で縦が 700px ほど余る。幅を変えなければ
    // px/セル (= 読み取りマージン) は 11x10 と同じままなので、余った縦を
    // 使い切るだけでブロック数を 110 -> 154 にできる。
    name: '縦長',
    description: '幅は 11x10 のまま縦を使い切る。1MB 定常 約 65 KB/s',
    grid: '11x14',
    bpc: 1,
    fps: 20,
    preset: ResolutionPreset.max,
  ),
  VcodePreset(
    // 234 ブロック。マーカー直接検出で 3.1〜3.5 px/セル でも掴めるようになり、
    // 容量で押し切る。満点率は数 % しかないが部分回収と fountain 符号で積み上がる。
    // 受信を別 isolate にして毎秒 22 枚処理できるようになってからは 20fps が最良
    // (1MB の定常で 131〜147 KB/s。15fps は 105〜115)。それ以前は受信が 15 枚/秒で
    // 頭打ちだったため 15fps のほうが安定していた。
    // 13x20 (3.0 px/セル) は満点率がほぼ 0 になり、密度の床はここ。
    name: '超密',
    description: '260×416 セル、234 ブロック。OLED 送信 30fps で 1MB 200 KB/s。'
        'LCD 画面から送るなら 縦長 (20fps) を',
    grid: '13x18',
    bpc: 1,
    fps: 30,
    preset: ResolutionPreset.max,
  ),
  VcodePreset(
    name: '4値(実験)',
    description: '輝度4値。復号はできるが同じ格子の 1bit より遅い (実測 22.8 KB/s)',
    grid: '7x6',
    bpc: 2,
    fps: 10,
    preset: ResolutionPreset.max,
  ),
];

/// 既定は「超密」(縦長 13×18 / 1bit)。スマホ縦持ちの視野を使い切る現行の最速構成。
/// 送受信とも既定で揃うので、双方がそのまま起動すれば掴める
const kDefaultPresetIndex = 5;
