import 'dart:math' as math;

import 'package:flutter/material.dart';

import '../probe_stats.dart';
import 'packet_coverage.dart';

/// 映像の上に重ねる半透明の角丸バッジ (状態・誘導・格子チップ共通の見た目)。
BoxDecoration overlayBadgeDecoration({Color? border, double borderWidth = 1.5}) =>
    BoxDecoration(
      color: Colors.black.withValues(alpha: 0.55),
      borderRadius: BorderRadius.circular(16),
      border: border == null ? null : Border.all(color: border, width: borderWidth),
    );

/// 構図や格子の誘導を出す黄色い文字のバッジ。onTap があればタップで対処できる。
class HintBadge extends StatelessWidget {
  const HintBadge(this.text, {super.key, this.onTap});
  final String text;
  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) {
    final badge = Container(
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
      decoration: overlayBadgeDecoration(),
      child: Text(
        text,
        textAlign: TextAlign.center,
        style: const TextStyle(
          color: Colors.amberAccent,
          fontSize: 13,
          fontWeight: FontWeight.w600,
        ),
      ),
    );
    return onTap == null ? badge : GestureDetector(onTap: onTap, child: badge);
  }
}

/// 処理中に映像を暗くしてスピナーと一言を出す (復元中・位置を検出中)。
class BusyOverlay extends StatelessWidget {
  const BusyOverlay(this.label, {super.key});
  final String label;

  @override
  Widget build(BuildContext context) {
    return Container(
      color: Colors.black54,
      child: Center(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            const CircularProgressIndicator(),
            const SizedBox(height: 12),
            Text(label, style: const TextStyle(color: Colors.white, fontSize: 15)),
          ],
        ),
      ),
    );
  }
}

/// 「さがす」モードの一覧。設定ごとの実効スループットを出し続ける。
/// 構図の読み上げ ([footer]) もこの中にまとめる (映像の上端に別に出すとコードの隅に被る)。
class ProbePanel extends StatelessWidget {
  const ProbePanel({super.key, required this.probe, required this.footer});
  final ProbeStats probe;
  final Widget footer;

  @override
  Widget build(BuildContext context) {
    final best = probe.best;
    const head = TextStyle(color: Colors.white70, fontSize: 11);
    TextStyle cell(ProbeSetting s) => TextStyle(
      color: identical(s, best) ? const Color(0xFF7CFFB2) : Colors.white,
      fontSize: 13,
      fontWeight: identical(s, best) ? FontWeight.w700 : FontWeight.w500,
      fontFeatures: const [FontFeature.tabularFigures()],
    );
    Text num(String v, ProbeSetting s) =>
        Text(v, style: cell(s), textAlign: TextAlign.right);
    return Container(
      padding: const EdgeInsets.fromLTRB(12, 8, 12, 10),
      decoration: BoxDecoration(
        color: Colors.black.withValues(alpha: 0.7),
        borderRadius: BorderRadius.circular(10),
      ),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(
            best == null
                ? '速度テスト: 計測中… (${probe.settingCount} 通り × '
                      '${probe.dwellSec.toStringAsFixed(0)} 秒)'
                : 'おすすめ: ${best.grid} / ${best.fps}fps '
                      '(悪いときで ${best.minKbps!.toStringAsFixed(0)} KB/s)',
            style: const TextStyle(
              color: Colors.white,
              fontSize: 13,
              fontWeight: FontWeight.w600,
            ),
          ),
          const SizedBox(height: 6),
          Table(
            columnWidths: const {
              0: FixedColumnWidth(14),
              1: FlexColumnWidth(1.25),
              2: FlexColumnWidth(0.7),
              3: FlexColumnWidth(0.95),
              4: FlexColumnWidth(1.0),
              5: FlexColumnWidth(1.9),
            },
            defaultVerticalAlignment: TableCellVerticalAlignment.middle,
            children: [
              const TableRow(
                children: [
                  SizedBox.shrink(),
                  Text('格子', style: head),
                  Text('fps', style: head),
                  Text('KB/s', style: head, textAlign: TextAlign.right),
                  Text('読めた', style: head, textAlign: TextAlign.right),
                  Text('悪い〜良い', style: head, textAlign: TextAlign.right),
                ],
              ),
              for (final s in probe.settings)
                TableRow(
                  children: [
                    Text(
                      identical(s, probe.current) ? '▶' : '',
                      style: const TextStyle(color: Colors.white, fontSize: 11),
                    ),
                    Text(s.grid, style: cell(s)),
                    Text('${s.fps}', style: cell(s)),
                    num(s.kbps.toStringAsFixed(0), s),
                    num('${(s.recovery * 100).toStringAsFixed(0)}%', s),
                    num(
                      s.visits == 0
                          ? '-'
                          : '${s.minKbps!.toStringAsFixed(0)}〜'
                                '${s.maxKbps!.toStringAsFixed(0)}',
                      s,
                    ),
                  ],
                ),
            ],
          ),
          const SizedBox(height: 6),
          footer,
        ],
      ),
    );
  }
}

/// ラベルと値の 2 列表 (受信完了後の統計)。
class StatsTable extends StatelessWidget {
  const StatsTable(this.rows, {super.key});
  final List<(String, String)> rows;

  @override
  Widget build(BuildContext context) {
    const pad = EdgeInsets.symmetric(vertical: 3, horizontal: 8);
    final labelStyle = TextStyle(
      fontSize: 12,
      color: Theme.of(context).colorScheme.onSurfaceVariant,
    );
    return Table(
      columnWidths: const {0: IntrinsicColumnWidth(), 1: FlexColumnWidth()},
      defaultVerticalAlignment: TableCellVerticalAlignment.middle,
      children: [
        for (final (label, value) in rows)
          TableRow(
            children: [
              Padding(padding: pad, child: Text(label, style: labelStyle)),
              Padding(
                padding: pad,
                child: Text(
                  value,
                  style: const TextStyle(fontSize: 13, fontWeight: FontWeight.w600),
                ),
              ),
            ],
          ),
      ],
    );
  }
}

/// 受信データのカバレッジ格子 (パケット番号順の被覆)。受信済み=緑(source)/水色(repair)、
/// 未受信=灰。埋まらない穴 = まだ取れていないパケット (= 復元が完了しない原因) が一目でわかる。
/// 高さは固定で、一定サイズ内に収める (カメラ表示位置がずれないように)。
class CoverageGrid extends StatelessWidget {
  const CoverageGrid(this.cov, {super.key});
  final PacketCoverage cov;

  @override
  Widget build(BuildContext context) {
    return SizedBox(
      height: 72,
      width: double.infinity,
      child: CustomPaint(
        painter: _CoverageGridPainter(cov: cov, distinct: cov.distinct),
      ),
    );
  }
}

class _CoverageGridPainter extends CustomPainter {
  _CoverageGridPainter({required this.cov, required this.distinct});
  final PacketCoverage cov;
  // 描いた時点の受信数。cov は同じインスタンスが更新され続けるので、比較用に控える
  final int distinct;

  @override
  void paint(Canvas canvas, Size size) {
    final cap = cov.cap;
    if (cap <= 0 || size.width <= 0 || size.height <= 0) return;
    // 1 マス 1 パケットだと 10MB 超で数十万マスになり、毎フレームの描画で UI が詰まる。
    // マス数を上限で抑え、1 マスに入るパケットの充足率を濃淡で出す (PWA と同じ)。
    const s = 6.0;
    final cols = math.max(1, (size.width / s).floor());
    final rows = math.max(1, (size.height / s).floor());
    // 1 マス = 度数分布のバケット perB 個 (= per パケット)
    final perB = math.max(1, (cap / cov.bucketW / (cols * rows)).ceil());
    final per = perB * cov.bucketW;
    final cells = math.min(cols * rows, (cap / per).ceil());
    final unseen = Paint()..color = const Color(0xFF37474F);
    final paint = Paint();
    for (var i = 0; i < cells; i++) {
      var got = 0;
      for (var j = i * perB; j < (i + 1) * perB && j < cov.hist.length; j++) {
        got += cov.hist[j];
      }
      final rect = Rect.fromLTWH((i % cols) * s, (i ~/ cols) * s, s - 1, s - 1);
      if (got == 0) {
        canvas.drawRect(rect, unseen);
        continue;
      }
      final a = 0.25 + 0.75 * math.min(1.0, got / per);
      // source と repair で色を分ける (マスの中心がどちら側かで判定)
      final src = (i + 0.5) * per < cov.kt;
      paint.color = (src ? const Color(0xFF4CAF50) : const Color(0xFF29B6F6))
          .withValues(alpha: a);
      canvas.drawRect(rect, paint);
    }
  }

  @override
  bool shouldRepaint(covariant _CoverageGridPainter old) =>
      old.distinct != distinct || !identical(old.cov, cov);
}
