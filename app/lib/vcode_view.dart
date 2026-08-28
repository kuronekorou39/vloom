import 'dart:math' as math;

import 'package:camera/camera.dart';
import 'package:flutter/material.dart';

/// vcode 受信の共有パーツ。本番受信 (VcodeReceiveScreen) と校正受信 (_VCalReceive)
/// で「カメラ描画」「緑のガイド枠」「スキャン範囲 (guideFrac)」を完全に共有し、
/// 校正で合わせた位置がそのまま本番でも成立するようにする。
///
/// ここを唯一の真実 (source of truth) とし、両画面はこの値/ウィジェットを使う。
/// UI のガイド枠と Rust 側のスキャン範囲計算はこの比率で一致させること。
const double kVcodeGuideFrac = 0.8;

/// カメラプレビュー + 緑ガイド枠。受信系はすべてこれを使う。
///
/// **カメラ画像の全体を映す (contain)。** 以前は幅いっぱいに拡げて縦をクリップして
/// いたが、それだと「写っているのに画面に出ていない」領域ができる。スキャンは
/// 画像全体を見るので、そこでコードに当たると利用者には「枠の外で検出された」
/// としか見えない。表示範囲とスキャン範囲は一致していなければならない。
/// 縦長の画面ではカメラ画像の上下に余りが出るので、そこに UI を重ねる。
///
/// [overlay] はカメラ画像の矩形そのものに重なる。ガイド枠や検出枠のように
/// 「画像座標に対応させたい描画」はここに渡すこと。表示領域全体に重ねると
/// 上下の余白ぶんだけ画像とずれる。
class VcodeCameraView extends StatelessWidget {
  const VcodeCameraView(this.controller, {super.key, this.overlay});
  final CameraController controller;
  final Widget? overlay;

  @override
  Widget build(BuildContext context) {
    return LayoutBuilder(builder: (ctx, c) {
      // CameraPreview は縦表示で 1/aspectRatio の縦横比なので、
      // 幅 w のとき高さ w*aspectRatio (歪みなし)。表示領域に収まる最大の w を取る。
      final ar = controller.value.aspectRatio;
      final w = math.min(c.maxWidth, c.maxHeight / ar);
      return Center(
        child: SizedBox(
          width: w,
          height: w * ar,
          child: Stack(
            fit: StackFit.expand,
            children: [
              CameraPreview(controller),
              // vcode の枠 (guideFrac 準拠, 縦横比 0.92 = ブロック格子形状)
              const ScanGuideOverlay(widthFrac: kVcodeGuideFrac, aspect: 0.92),
              ?overlay,
            ],
          ),
        ),
      );
    });
  }
}

/// スキャンの照準となる緑の枠 (四隅強調)。受信・校正で共用し、
/// 「枠に収める」という操作感を統一する。
class ScanGuideOverlay extends StatelessWidget {
  const ScanGuideOverlay({super.key, this.widthFrac = 0.8, this.aspect = 1.0});

  /// 表示幅に対する枠の幅の比率
  final double widthFrac;

  /// 枠の縦横比 (高さ / 幅)
  final double aspect;

  @override
  Widget build(BuildContext context) {
    return IgnorePointer(
      child: CustomPaint(
          painter: _ScanGuidePainter(widthFrac: widthFrac, aspect: aspect)),
    );
  }
}

class _ScanGuidePainter extends CustomPainter {
  _ScanGuidePainter({required this.widthFrac, required this.aspect});
  final double widthFrac;
  final double aspect;

  @override
  void paint(Canvas canvas, Size size) {
    final paint = Paint()
      ..color = Colors.greenAccent
      ..style = PaintingStyle.stroke
      ..strokeWidth = 2;
    final gw = size.width * widthFrac;
    final gh = gw * aspect;
    final rect = Rect.fromCenter(
        center: Offset(size.width / 2, size.height / 2), width: gw, height: gh);
    canvas.drawRect(rect, paint);
    // 四隅を強調
    const l = 24.0;
    final corner = Paint()
      ..color = Colors.greenAccent
      ..style = PaintingStyle.stroke
      ..strokeWidth = 5;
    for (final (dx, dy) in [
      (0.0, 0.0),
      (rect.width, 0.0),
      (0.0, rect.height),
      (rect.width, rect.height)
    ]) {
      final p = rect.topLeft + Offset(dx, dy);
      final sx = dx == 0 ? 1.0 : -1.0;
      final sy = dy == 0 ? 1.0 : -1.0;
      canvas.drawLine(p, p + Offset(sx * l, 0), corner);
      canvas.drawLine(p, p + Offset(0, sy * l), corner);
    }
  }

  @override
  bool shouldRepaint(covariant _ScanGuidePainter old) =>
      old.widthFrac != widthFrac || old.aspect != aspect;
}
