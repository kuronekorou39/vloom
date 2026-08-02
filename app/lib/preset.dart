// 送受信のプリセット。
//
// 格子・階調・fps・解像度を個別に合わせるのは現実的でない (送受信で食い違うと
// 何が原因か分からなくなる)。プリセットを 1 つ選べば両側の設定が決まるようにする。
//
// fps の上限はカメラのローリングシャッターで決まる。センサーは画像を上から下へ
// 順次読み出すので 1 枚あたり 25〜35ms かかり、その間に送信側の画面が切り替わると
// 1 枚の写真に 2 フレームが混ざってデータブロックが全滅する (コーナーとヘッダは
// 画像の上下端なので揃うことがあり、「追従中なのに blocks=0/42」になる)。
// したがって fps を上げても稼げない。スループットは 1 フレームあたりの容量で稼ぐ。

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

/// 選べるプリセット。上から順に「まず通す」→「速度を狙う」。
const kPresets = <VcodePreset>[
  VcodePreset(
    name: '確実',
    description: 'まずここから。低密度・低速で確実に通す',
    grid: '5x4',
    bpc: 1,
    fps: 10,
    preset: ResolutionPreset.veryHigh,
  ),
  VcodePreset(
    name: '標準',
    description: '実績のある設定。7×6 の輝度4値',
    grid: '7x6',
    bpc: 2,
    fps: 10,
    preset: ResolutionPreset.veryHigh,
  ),
  VcodePreset(
    name: '高速',
    description: '密度で稼ぐ。2160p 受信が要る (6px/セルに 1080px)',
    grid: '9x8',
    bpc: 2,
    fps: 10,
    preset: ResolutionPreset.ultraHigh,
  ),
  VcodePreset(
    name: '限界',
    description: '4K 受信・近距離・固定が前提',
    grid: '11x10',
    bpc: 2,
    fps: 10,
    preset: ResolutionPreset.max,
  ),
];

/// 既定は「標準」(実績のある 7×6)
const kDefaultPresetIndex = 1;
