/// 受信中の速さ (直近の数秒) と残り時間の目安。
///
/// 受信済みのバイト数を時刻つきで覚え、直近 [window] の傾きを速さにする。始めからの平均だと、
/// 掴むまでの数秒や途中で見失った時間が効いて、いま置き直した効果が見えない。
class RxSpeedMeter {
  RxSpeedMeter({this.window = const Duration(seconds: 4)});

  /// 速さを測る幅。短いと 1 枚ごとの揺れで数字がばたつき、長いと置き直しへの反応が鈍い
  final Duration window;

  /// 標本を取る間隔 (これより詰まった呼び出しは捨てる)
  static const _minGap = Duration(milliseconds: 250);

  final List<(DateTime, int)> _samples = [];

  /// 受信済みのバイト数 [bytes] を記録する。走査 1 枚ごとに呼んでよい。
  void add(DateTime now, int bytes) {
    if (_samples.isNotEmpty && now.difference(_samples.last.$1) < _minGap) return;
    _samples.add((now, bytes));
    while (_samples.length > 2 && now.difference(_samples[1].$1) >= window) {
      _samples.removeAt(0);
    }
  }

  /// 直近の速さ (バイト/秒)。まだ測れなければ null
  double? get bytesPerSec {
    if (_samples.length < 2) return null;
    final (t0, b0) = _samples.first;
    final (t1, b1) = _samples.last;
    final ms = t1.difference(t0).inMilliseconds;
    if (ms <= 0) return null;
    return (b1 - b0) * 1000 / ms;
  }

  /// 残り [remainingBytes] にかかる時間の目安。止まっている・測れないときは null
  Duration? eta(int remainingBytes) {
    final v = bytesPerSec;
    if (v == null || v <= 0) return null;
    return Duration(milliseconds: (remainingBytes * 1000 / v).round());
  }
}

/// 残り時間を「約 40 秒」「約 2 分 10 秒」の形にする (1 分を超えたら 10 秒単位に丸める)。
String fmtEta(Duration d) {
  final s = d.inSeconds;
  if (s < 5) return 'まもなく';
  if (s < 60) return '約 $s 秒';
  final r = (s / 10).round() * 10;
  final sec = r % 60;
  return sec == 0 ? '約 ${r ~/ 60} 分' : '約 ${r ~/ 60} 分 $sec 秒';
}
