import 'dart:math' as math;

import 'package:flutter/material.dart';

/// 回転後画像座標 (imgW×imgH) の点を、プレビュー表示の正規化座標 (0..1) へ写す。
/// delta = (sensorOrientation - 検出時 rot + 360) % 360。プレビューは sensorOrientation
/// 空間で表示されるので、検出時の回転との差分だけ回す。検出枠の描画と構図合わせの
/// 読み上げで同じ変換を使う。
Offset previewNorm(double x, double y, int imgW, int imgH, int delta) {
  double ix, iy;
  int pw, ph;
  if (delta == 90) {
    ix = imgH - 1 - y;
    iy = x;
    pw = imgH;
    ph = imgW;
  } else if (delta == 180) {
    ix = imgW - 1 - x;
    iy = imgH - 1 - y;
    pw = imgW;
    ph = imgH;
  } else if (delta == 270) {
    ix = y;
    iy = imgW - 1 - x;
    pw = imgH;
    ph = imgW;
  } else {
    ix = x;
    iy = y;
    pw = imgW;
    ph = imgH;
  }
  return Offset(ix / pw, iy / ph);
}

/// スキャナが検出したコードの位置 (acquire / 毎フレームの追従結果)。
/// 検出枠のハイライト・構図の誘導・充填率の読み上げが、すべてここから計算する。
class DetectedCode {
  DetectedCode({
    required this.corners,
    required this.imgW,
    required this.imgH,
    required this.rot,
    required this.cellsW,
    required this.cellsH,
  });

  /// 回転後画像座標の 4 隅 [tl.x, tl.y, tr.x, tr.y, br.x, br.y, bl.x, bl.y]
  final List<double> corners;

  /// 検出時の回転後画像寸法と回転
  final int imgW, imgH, rot;

  /// 検出したコードのセル数 (充填率・px/セル の計算用)
  final int cellsW, cellsH;

  bool get valid => corners.length >= 8 && imgW > 0 && imgH > 0;

  /// プレビューに対する回転差。sensorOrientation はカメラ固有の値
  int deltaFor(int sensorOrientation) => (sensorOrientation - rot + 360) % 360;

  /// プレビュー上の外接矩形 (正規化座標 0..1)
  Rect previewBounds(int sensorOrientation) {
    final delta = deltaFor(sensorOrientation);
    var minX = 1.0, maxX = 0.0, minY = 1.0, maxY = 0.0;
    for (var i = 0; i < 4; i++) {
      final p = previewNorm(corners[i * 2], corners[i * 2 + 1], imgW, imgH, delta);
      minX = math.min(minX, p.dx);
      maxX = math.max(maxX, p.dx);
      minY = math.min(minY, p.dy);
      maxY = math.max(maxY, p.dy);
    }
    return Rect.fromLTRB(minX, minY, maxX, maxY);
  }

  /// px/セル。回転後画像座標での辺の長さをセル数で割る (カメラ画像の実寸)。
  /// 縦横で違えば小さいほう = 読み取りが厳しいほうを返す
  double get pxPerCell {
    final c = corners;
    final topLen = math.sqrt(math.pow(c[2] - c[0], 2) + math.pow(c[3] - c[1], 2));
    final leftLen = math.sqrt(math.pow(c[6] - c[0], 2) + math.pow(c[7] - c[1], 2));
    final pxW = cellsW > 0 ? topLen / cellsW : 0.0;
    final pxH = cellsH > 0 ? leftLen / cellsH : 0.0;
    return math.min(pxW, pxH);
  }

  /// 構図の誘導 (人向けの一言)。数字の読み上げと違い、どう動けばいいかだけを短く出す。
  /// 問題がなければ null
  String? guideHint(int sensorOrientation) {
    if (!valid) return null;
    final b = previewBounds(sensorOrientation);
    // はみ出しかけ → まず離れる (寄せの指示より優先)
    if (b.left < 0.02 || b.right > 0.98 || b.top < 0.02 || b.bottom > 0.98) {
      return '少し離してください';
    }
    final px = pxPerCell;
    if (px > 0 && px < 2.2) return 'もう少し近づけてください';
    final cx = b.center.dx - 0.5, cy = b.center.dy - 0.5;
    if (cx.abs() > 0.16 || cy.abs() > 0.16) {
      final dirs = <String>[
        if (cx < -0.16) '左',
        if (cx > 0.16) '右',
        if (cy < -0.16) '上',
        if (cy > 0.16) '下',
      ];
      return 'コードが${dirs.join("と")}に寄っています';
    }
    return null;
  }
}

/// 検出した 4 隅をプレビュー上にハイライトする。VcodeCameraView の overlay として、
/// カメラ画像の矩形そのものに重ねて描く (正規化座標を比率で伸ばすだけで済む)。
class DetectedQuadPainter extends CustomPainter {
  DetectedQuadPainter(this.code, {required this.sensorOrientation});
  final DetectedCode code;
  final int sensorOrientation;

  @override
  void paint(Canvas canvas, Size size) {
    if (!code.valid) return;
    final delta = code.deltaFor(sensorOrientation);
    final c = code.corners;
    Offset map(int i) {
      final n = previewNorm(c[i * 2], c[i * 2 + 1], code.imgW, code.imgH, delta);
      return Offset(n.dx * size.width, n.dy * size.height);
    }

    final pts = [map(0), map(1), map(2), map(3)];
    final path = Path()..addPolygon(pts, true);
    canvas.drawPath(path, Paint()..color = const Color(0x3300E5FF));
    canvas.drawPath(
      path,
      Paint()
        ..color = const Color(0xFF00E5FF)
        ..style = PaintingStyle.stroke
        ..strokeWidth = 3,
    );
    final dot = Paint()..color = const Color(0xFF00E5FF);
    for (final p in pts) {
      canvas.drawCircle(p, 6, dot);
    }
  }

  @override
  bool shouldRepaint(covariant DetectedQuadPainter old) =>
      !identical(old.code, code) || old.sensorOrientation != sensorOrientation;
}

/// 構図合わせ用の読み上げ。検出した四隅から、カメラ画像に対する充填率と四辺の
/// 余白、px/セル を出す。三脚やウィンドウをどちらへどれだけ動かせばよいかを
/// 数字で判断できるようにする (緑の枠は目安にしかならないため)。
class AlignPanel extends StatelessWidget {
  const AlignPanel({super.key, required this.code, required this.sensorOrientation});
  final DetectedCode? code;
  final int sensorOrientation;

  @override
  Widget build(BuildContext context) {
    final c = code;
    if (c == null || !c.valid) return const SizedBox.shrink();
    final b = c.previewBounds(sensorOrientation);
    final fillW = b.width * 100, fillH = b.height * 100;
    final top = b.top * 100, bottom = (1 - b.bottom) * 100;
    final left = b.left * 100, right = (1 - b.right) * 100;
    // 余白が 2% を切った辺は、少しの揺れで四隅マーカーが画角から出る
    final tight = <String>[
      if (top < 2) '上',
      if (bottom < 2) '下',
      if (left < 2) '左',
      if (right < 2) '右',
    ];
    // いちばん余っている辺を示す。そちらへ寄せる/大きくする余地がある
    final slack = {
      '上': top,
      '下': bottom,
      '左': left,
      '右': right,
    }.entries.reduce((a, b) => a.value >= b.value ? a : b);
    return Align(
      alignment: Alignment.centerLeft,
      child: Container(
        padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 6),
        decoration: BoxDecoration(
          color: const Color(0x99000000),
          borderRadius: BorderRadius.circular(8),
        ),
        child: Text(
          '充填 幅 ${fillW.toStringAsFixed(0)}% · 高 ${fillH.toStringAsFixed(0)}%'
          '   ${c.pxPerCell.toStringAsFixed(1)} px/セル\n'
          '余白 上 ${top.toStringAsFixed(0)}%  下 ${bottom.toStringAsFixed(0)}%  '
          '左 ${left.toStringAsFixed(0)}%  右 ${right.toStringAsFixed(0)}%'
          '${tight.isNotEmpty ? "\nはみ出し注意: ${tight.join("/")}" : "   (${slack.key}に余裕)"}',
          style: TextStyle(
            color: tight.isEmpty ? Colors.white : Colors.redAccent,
            fontSize: 13,
            fontFeatures: const [FontFeature.tabularFigures()],
            height: 1.35,
          ),
        ),
      ),
    );
  }
}
