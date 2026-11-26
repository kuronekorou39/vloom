import 'dart:math' as math;

import 'package:flutter/material.dart';

import '../probe_stats.dart';
import '../ui_common.dart';
import 'packet_coverage.dart';
import 'rx_speed.dart';

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

/// 受信の進み具合のバー。
///
/// 受信が必要数に届いても、最後のブロックの復元計算が残る。そのまま 100% を出すと
/// 「満タンなのに終わらない」になって待たされる感じが強い (2026-11-25 の指摘)。
/// 受信は 97% までの目盛りで進め、届いたあとは同じバーが残りをゆっくり伸び続ける。
/// 満タンは完了画面に切り替わるときだけ。
///
/// 以前は残り 3% を別の色の区間にして流れる表示にしていたが、97% の位置に切れ目が見え、
/// 「ここから別の処理をしている」と分かってしまった。見た目は 1 本のまま、途切れずに進める。
class ReceiveGauge extends StatefulWidget {
  const ReceiveGauge({super.key, required this.received});

  /// 必要数に対する受信の割合 (0〜1)
  final double received;

  @override
  State<ReceiveGauge> createState() => _ReceiveGaugeState();
}

class _ReceiveGaugeState extends State<ReceiveGauge> with SingleTickerProviderStateMixin {
  /// 受信に割り当てる目盛り。残りは、届いたあとの復元計算の間に伸びるぶん
  static const _receiveShare = 0.97;

  /// 復元計算の間に伸びる上限 (残りの区間に対する割合)。満タンには届かせない
  static const _creepLimit = 0.9;

  /// 残りの区間を伸びきるまでの時間。復元計算は数秒〜十数秒で、それより十分長く取る
  /// (先に伸びきると、また「止まったまま待たされる」になる)
  static const _creepDuration = Duration(seconds: 40);

  late final AnimationController _creep =
      AnimationController(vsync: this, duration: _creepDuration);

  @override
  void initState() {
    super.initState();
    if (widget.received >= 1.0) _creep.forward();
  }

  @override
  void didUpdateWidget(ReceiveGauge old) {
    super.didUpdateWidget(old);
    final finishing = widget.received >= 1.0;
    if (finishing && _creep.status == AnimationStatus.dismissed) {
      _creep.forward();
    } else if (!finishing && _creep.value > 0) {
      _creep.value = 0; // 受信のやり直し
    }
  }

  @override
  void dispose() {
    _creep.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return AnimatedBuilder(
      animation: _creep,
      builder: (context, _) {
        // 最初は速く、だんだん遅く。どこまで行っても少しずつ動いて見える
        final crept = Curves.easeOutCubic.transform(_creep.value) * _creepLimit;
        final value = widget.received < 1.0
            ? widget.received * _receiveShare
            : _receiveShare + (1 - _receiveShare) * crept;
        return LinearProgressIndicator(
          value: value,
          minHeight: 6,
          backgroundColor: const Color(0x55FFFFFF),
        );
      },
    );
  }
}

/// 受信中の 1 行: 受信済みの量・速さ・残り時間。進捗バーのすぐ下に置く。
///
/// 以前は「カメラ 30 枚/s · 走査 … · 投入 …」という走査の内訳しか出しておらず、
/// あとどれくらいで終わるのかが分からなかった。
class ReceiveProgressLine extends StatelessWidget {
  const ReceiveProgressLine({
    super.key,
    required this.receivedBytes,
    required this.totalBytes,
    required this.bytesPerSec,
    required this.eta,
    required this.finishing,
  });

  final int receivedBytes;
  final int totalBytes;

  /// 直近の速さ。まだ測れていなければ null
  final double? bytesPerSec;

  /// 残り時間の目安。止まっている・測れないときは null
  final Duration? eta;

  /// 必要な量は届いていて、復元の計算を待っている
  final bool finishing;

  @override
  Widget build(BuildContext context) {
    final speed = bytesPerSec;
    final parts = <String>[
      '${fmtSize(receivedBytes)} / ${fmtSize(totalBytes)}',
      if (finishing)
        '仕上げ中…'
      else ...[
        if (speed != null) '${(speed / 1024).round()} KB/s',
        if (eta != null) '残り ${fmtEta(eta!)}',
      ],
    ];
    return Text(
      parts.join('  ·  '),
      style: const TextStyle(
        fontSize: 15,
        fontWeight: FontWeight.w600,
        color: Colors.white,
        fontFeatures: [FontFeature.tabularFigures()], // 数字の幅を揃えて、行がばたつかないように
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
