/// 受信 1 回ぶんの集計値。画面の統計表・計測ログ (`[vloom-stats]` / timing) の元になる。
///
/// 受信のやり直しやカメラの作り直しでは、インスタンスごと作り直して初期化する
/// (以前は 20 近いフィールドを 2 か所で個別にゼロへ戻していて、片方だけ漏れやすかった)。
class RxStats {
  /// カメラフレームを走査に回した枚数 (並行上限で捨てた枚は含まない)
  int framesSeen = 0;
  int framesDetected = 0;
  int framesTracked = 0;

  /// 回収ブロック数の累計と、検出フレームの blocksTotal の累計 (平均回収率 = 前者 / 後者)
  int blocksOk = 0;
  int blocksTotalSum = 0;

  /// デコーダに投入したパケット数 (重複込み)
  int packetsAdded = 0;

  /// エンドツーエンド CRC 不一致で受信をやり直した回数
  int integrityFails = 0;

  /// 直近 1 枚の走査時間 (ms)
  int lastScanMs = 0;

  int scanCount = 0;
  int scanMsSum = 0;

  // スキャン内訳の累計 (Y プレーン回転コピー / 探索・デコード)。
  // カメラのフレーム間隔に追従できないとき、どちらが効いているかの切り分けに使う。
  int rotateUsSum = 0;
  int decodeUsSum = 0;

  DateTime? firstDetected;

  /// 検出フレーム 1 枚あたりの回収ブロック数の分布 (0 / ~25% / ~50% / ~75% / ~99% / 100%)。
  /// 二極化 (0 と満点だけ) ならローリングシャッターや残像による時間方向の混入、
  /// 中間に散るなら px/セル・ピント・露出といった空間方向の問題を示す。
  /// どちらの壁に当たっているかで打ち手が変わるので、切り分けの一次情報として出す。
  final List<int> blockHist = List<int>.filled(6, 0);

  /// 失敗理由の内訳 (CornerMismatch / HeaderNotFound / ...)。
  /// 「たまに読める」ときに、読めない側が どの工程で落ちているか を切り分ける。
  final Map<String, int> errorKinds = {};

  // 処理時間の内訳 (30 枚ごとに平均してログに出す)
  int _tTotal = 0, _tScan = 0, _tDec = 0, _tFrames = 0;
  int _tMissMs = 0, _tMissN = 0; // 検出できなかったフレームの scan 時間と枚数

  static const _histLabels = ['0', '~25%', '~50%', '~75%', '~99%', '満点'];

  /// 走査 1 枚の結果を数える (検出の成否によらず)。
  void addScan({
    required int scanMs,
    required bool detected,
    required int rotateUs,
    required int decodeUs,
  }) {
    framesSeen++;
    lastScanMs = scanMs;
    scanMsSum += scanMs;
    _tScan += scanMs;
    if (!detected) {
      _tMissMs += scanMs;
      _tMissN++;
    }
    rotateUsSum += rotateUs;
    decodeUsSum += decodeUs;
    scanCount++;
  }

  /// 検出できたフレームを数える。
  void addDetected({required bool tracked, required int ok, required int total}) {
    framesDetected++;
    if (tracked) framesTracked++;
    firstDetected ??= DateTime.now();
    blocksOk += ok;
    blocksTotalSum += total;
    blockHist[histBucket(ok, total)]++;
  }

  /// 検出できなかった理由を数える。理由は "rot90/7x6:CornerMismatch" のような形で、末尾の種別だけ数える。
  void addMiss(String? error) {
    final kind = (error ?? '?').split(':').last.split(' ').first;
    errorKinds[kind] = (errorKinds[kind] ?? 0) + 1;
  }

  /// デコーダ投入にかかった時間を足す。
  void addDecodeMs(int ms) => _tDec += ms;

  /// 1 枚の処理全体にかかった時間を足す。30 枚たまったら内訳の平均を 1 行で返し、
  /// 区間をリセットする (それ以外は null)。受信の処理速度が天井になったとき、Rust の
  /// スキャンか、FRB の Y 面コピーか、デコーダ投入か、を切り分けるためのログ。
  String? addFrameTime(int ms) {
    _tTotal += ms;
    _tFrames++;
    if (_tFrames < 30) return null;
    final line =
        'total=${(_tTotal / _tFrames).toStringAsFixed(1)}ms '
        'scan(frb)=${(_tScan / _tFrames).toStringAsFixed(1)}ms '
        'decoder=${(_tDec / _tFrames).toStringAsFixed(1)}ms '
        'miss=$_tMissN/$_tFrames (${_tMissN == 0 ? 0 : (_tMissMs / _tMissN).toStringAsFixed(0)}ms each) '
        'rust(rot+dec)=${avgRustMs.toStringAsFixed(1)}ms';
    _tTotal = _tScan = _tDec = _tFrames = _tMissMs = _tMissN = 0;
    return line;
  }

  /// 初検出からの経過時間 (未検出なら 0)
  Duration elapsedSinceFirstDetect() => firstDetected == null
      ? Duration.zero
      : DateTime.now().difference(firstDetected!);

  /// 平均回収率 (0〜1)。検出がなければ null
  double? get recoveryRatio =>
      blocksTotalSum > 0 ? blocksOk / blocksTotalSum : null;

  int get avgScanMs => scanCount > 0 ? (scanMsSum / scanCount).round() : 0;
  double get avgRotateMs => scanCount > 0 ? rotateUsSum / scanCount / 1000 : 0;
  double get avgDecodeMs => scanCount > 0 ? decodeUsSum / scanCount / 1000 : 0;
  double get avgRustMs => avgRotateMs + avgDecodeMs;

  /// 回収ブロック数 → ヒストグラムのバケット番号
  static int histBucket(int ok, int total) {
    if (total <= 0 || ok <= 0) return 0;
    if (ok >= total) return 5;
    final r = ok / total;
    if (r < 0.25) return 1;
    if (r < 0.50) return 2;
    if (r < 0.75) return 3;
    return 4;
  }

  /// ヒストグラムを "0:12 ~25%:3 ... 満点:80" の 1 行にする
  String histText() {
    final total = blockHist.fold<int>(0, (a, b) => a + b);
    if (total == 0) return '-';
    final parts = <String>[];
    for (var i = 0; i < blockHist.length; i++) {
      if (blockHist[i] > 0) {
        final pct = (blockHist[i] * 100 / total).round();
        parts.add('${_histLabels[i]}:${blockHist[i]}($pct%)');
      }
    }
    return parts.join('  ');
  }

  /// 失敗理由の内訳を "CornerMismatch:120(80%) HeaderNotFound:30(20%)" の 1 行に
  String errorKindsText() {
    final total = errorKinds.values.fold<int>(0, (a, b) => a + b);
    if (total == 0) return '-';
    final e = errorKinds.entries.toList()
      ..sort((a, b) => b.value.compareTo(a.value));
    return e
        .map((x) => '${x.key}:${x.value}(${(x.value * 100 / total).round()}%)')
        .join('  ');
  }
}
