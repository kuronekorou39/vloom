import 'dart:async';
import 'dart:convert' show utf8;
import 'dart:isolate';
import 'dart:io';

import 'package:camera/camera.dart';
import 'package:camera_android_camerax/camera_android_camerax.dart';
import 'package:camera_android_camerax/src/camerax_library.g.dart'
    show PigeonInstanceManager;
import 'package:camera_platform_interface/camera_platform_interface.dart'
    show CameraPlatform;
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:path_provider/path_provider.dart';
import 'package:wakelock_plus/wakelock_plus.dart';

import 'history_screen.dart' show shareReceived, saveReceivedToFile;
import 'history_store.dart';
import 'preset.dart';
import 'probe_stats.dart';
import 'receive/detected_code.dart';
import 'receive/file_sniff.dart';
import 'receive/focus_hunter.dart';
import 'receive/packet_coverage.dart';
import 'receive/rx_stats.dart';
import 'receive/rx_widgets.dart';
import 'rx_prefs.dart';
import 'launch_args.dart';
import 'scan_worker.dart';
import 'src/rust/api/fountain.dart';
import 'src/rust/api/vcode.dart';
import 'test_payload.dart';
import 'ui_common.dart';
import 'vcode_view.dart';

/// vcode 受信画面。camera パッケージで生 YUV フレームを取得し、
/// Y プレーンを Rust の vcode スキャナに渡す (mobile_scanner/MLKit 不使用)。
class VcodeReceiveScreen extends StatefulWidget {
  /// このタブが表示中で校正も開いていない = カメラを動かしてよいとき true。
  /// false の間は背面カメラを解放し、他画面 (校正など) と奪い合わないようにする。
  const VcodeReceiveScreen({super.key, this.active = true});
  final bool active;
  @override
  State<VcodeReceiveScreen> createState() => _VcodeReceiveScreenState();
}

/// 格子を固定せずスキャナの候補を総当たりさせる指定
const kGridAuto = 'auto';

/// 連続してこのフレーム数だけ検出できなければ、広域 sweep (acquire) に切り替える。
/// 短すぎると単発のフレーム落ちで重い処理が走り、長すぎると待たされる。
const kAutoAcquireMissFrames = 20;

/// 状態バッジを「追従中」のまま保つ猶予 (連続ミス数)。追従は数フレームの取り逃しで
/// 頻繁に途切れるが、そのたびに表示が「探しています」へ戻ると使う人が不安になる。
/// 内部状態はそのまま、表示だけ粘る。
const kBadgeHoldMissFrames = 30;

/// 自動 acquire の再試行間隔 (フレーム)。acquire は 300 回超の探索を伴うので連発させない。
const kAutoAcquireCooldownFrames = 45;

// 追従が何枚続いたらカメラ (露出) を固定するか。
//
// カメラのモード切替は何であれフレーム供給を 1〜5 秒止めることがある (フレーム
// ログで確認: AF+AE で最大 5.2 秒、露出だけでも 1.65 秒)。300 枚 (約 15 秒) に
// 置いてあるのは、1MB の転送 (120 枚前後) の途中で発動させないため。数十秒以上の
// 転送だけが 1 回の停止と引き換えに露出の安定 (白飛び防止) を得る形にする。
const kCamLockStreak = 300;

// AF の探り直し検出の定数と判断は receive/focus_hunter.dart。

// --- 密すぎる格子から降りる誘導 ---
//
// ブロックは 400 セル全部が合って初めて回収できるので、1 セルあたりの画素が
// 足りないと満点率が落ちる。合成計測 (track_motion) では、ボケ 0.80px のとき
// 13x18 (3.4px/セル) が 35 KB/s まで崩れるのに対し、11x14 (4.2px/セル) は
// 101 KB/s を保った。読めていないときは、密度を落とすほうが速い。
// 既定は変えない (据え置きで合っていれば 13x18 が最速のため) — 誘導だけ出す。

/// 誘導を出すか判断し始める検出フレーム数
const kSparseHintFrames = 40;

/// 平均の回収率がこれを下回っていたら、粗い格子を勧める
const kSparseHintRatio = 0.35;

/// 勧める格子と、その格子のセル幅 (これ以下なら勧めない)
const kSparseHintGrid = '11x14';
const kSparseHintCellsW = 220;

// 既定の露出補正 (EV)。理由は _initCameraInner の適用箇所を参照。
// -2 は最初の三脚位置では最良だったが、据え直した位置では白が 70 まで沈んで
// ヘッダが読めなくなった (0 EV なら 157)。カメラの自動露出が何を見るかで
// 絶対的な白の明るさが変わるので、固定値は控えめに -1 にしておく。
// 本来は白の実測 (輝度の最大値) を見て補正を追従させるべき。
const kDefaultExposureOffsetEv = -1.0;

/// この回数連続で見失ったら AF/AE ロックを解除する。モード切替はカメラの
/// フレーム供給を止めるため、acquire (20) より長く粘って発振を防ぐ。
const kCamUnlockMissFrames = 60;

/// 前回選んだプリセット (範囲外になっていたら捨てる)
int? get _persistedPresetIndex {
  final i = RxPrefs.presetIndex;
  return (i != null && i >= 0 && i < kPresets.length) ? i : null;
}

class _VcodeReceiveScreenState extends State<VcodeReceiveScreen>
    with WidgetsBindingObserver {
  CameraController? _cam;

  /// 選択中のプリセット。送信側と同じものを選べば格子も解像度も揃う
  // 起動 Intent でプリセットを指定できる (計測の自動化用)。カメラの解像度を
  // 決めるので、初期化より前に確定していなければならない。
  int _presetIndex = LaunchArgs.cached.grid != null
      ? -1 // 格子を直接指定したときはカスタム扱い
      : (LaunchArgs.cached.preset ??
            _persistedPresetIndex ??
            kDefaultPresetIndex);

  /// 探索する格子 (kGridAuto か '9x8' 等)。送信側と揃えるほど初回検出が速い
  // 格子の直接指定があればそれを使う (プリセットに無い格子を測るため)
  String _forcedGrid =
      LaunchArgs.cached.grid ??
      kPresets[LaunchArgs.cached.preset ??
              _persistedPresetIndex ??
              kDefaultPresetIndex]
          .grid;

  /// カメラ解像度。9x8 以上は 1080p では px/セル が足りない
  ResolutionPreset _preset =
      kPresets[LaunchArgs.cached.preset ??
              _persistedPresetIndex ??
              kDefaultPresetIndex]
          .preset;

  /// 実際に得られたプレビュー寸法 (完了後もカメラ停止後に残すので統計に出せる)
  Size? _lastPreviewSize;

  // --- 検出できないときの切り分け用の実測値 ---
  /// スキャナに渡した回転 (端末間で sensorOrientation が異なると検出 0 になりうる)
  int _lastRot = -1;

  /// Y プレーンの寸法と行バイト数 (stride != width の端末がある)
  int _lastImgW = 0, _lastImgH = 0, _lastStride = 0;

  /// Y の値域。iOS の 420v は video range (16〜235) に制限される
  int _lumaMin = 255, _lumaMax = 0;

  /// 直近の検出失敗理由 (Rust 側が返す FrameError)
  String? _lastError;

  /// 成功例と失敗例のダンプをそれぞれ 1 枚ずつ残したか。
  /// 追従できる/できないの差を PC で直接比べるために、両方が要る。
  bool _needOkDump = true;
  bool _needNgDump = true;
  int _lowDumps = 0;
  int _allDumps = 0;

  /// 「さがす」モードの集計。送信側が試験フレームを流していると気付いた時点で作る
  /// (受信側に切り替えの操作は無い。試験フレームかどうかはヘッダで分かる)
  ProbeStats? _probe;

  /// いま処理中の枚数 (走査の本数が上限)
  int _inFlight = 0;
  bool _active = false;
  bool _camBusy = false; // カメラ初期化/再初期化の多重実行ガード
  bool _acquireRequested = false; // 次フレームで acquire (位置検出) を実行する
  bool _acquiring = false; // acquire 実行中 (UI スピナー表示)
  bool _autoAcquire = true; // 検出できないとき自動で acquire を走らせる
  bool _acquireIsAuto = false; // 実行中の acquire が自動起動か (自動なら確認ダイアログを出さない)
  int _autoAcquireAt = 0; // 次に自動 acquire してよい _stats.framesSeen (連発を防ぐクールダウン)
  int _autoAcquireCount = 0; // 自動 acquire の実行回数 (統計・UI 表示用)
  int _missStreak = 0; // 連続して検出できなかったフレーム数
  int _detectStreak = 0; // 連続して検出できたフレーム数 (AF/AE ロックの判断に使う)
  bool _restoring = false; // 全パケット到達後の復元・検証中 (画面が固まって見えないように出す)
  bool _camLocked = false; // フォーカス・露出をロック済みか
  FocusHunter _focus = FocusHunter(); // AF の探り直し検出 (receive/focus_hunter.dart)
  bool _seeded = false; // acquire 結果で受信位置を確定済み (中央ガイド枠に頼らず追従)
  DetectedCode? _det; // 検出したコードの位置 — 枠のハイライト・構図の誘導・読み上げ用
  // 最後に検出したコードのセル幅。見失って _det を消した後も「密すぎる格子」の誘導に使う
  int _lastCellsW = 0;
  Timer? _watchdog; // プレビューが灰色 (フレーム途絶) になったら作り直す
  // スキャンは別 isolate (ScanWorker) で回す。理由は scan_worker.dart を参照
  ScanPool? _rx;
  // ワーカーが処理中に届いたフレーム。捨てずに 1 枚だけ持ち、処理が終わり次第すぐ回す
  // (受け取りと処理を重ねてカメラの供給速度まで処理するため)
  CameraImage? _pendingImg;

  FountainDecoder? _dec;
  Uint8List? _payload;
  HistoryItem? _savedItem;

  // 統計
  int _camCallbacks = 0; // カメラが配信した全フレーム (busy スキップ含む)
  DateTime? _camStarted;
  /// 受信 1 回ぶんの集計 (やり直しでインスタンスごと作り直す)
  RxStats _stats = RxStats();
  // 受信できたパケットの被覆 (重複除く)。ブロックごとに必要数で頭打ちした数が全体の必要数に
  // 届くと復元できる。進捗・カバレッジ格子・distinct 数の表示に使う。OTI が分かった時点で作る
  PacketCoverage? _cov;
  Duration? _elapsed;
  String _status = 'カメラ起動待ち';

  /// 操作パネルを画面上部に出すか (既定は下部)。三脚に据えたときや持ち方で
  /// 押しやすい側が変わり、コードが画面のどちら寄りに写るかで隠したくない
  /// 場所も変わるため、その場で入れ替えられるようにしている。
  bool _panelAtTop = false;

  /// 操作パネルを開いているか。既定は畳む: カバレッジ格子や設定まで出すと映像の
  /// 下 1/4 を隠し、コードの下側の四隅が目視できなくなる (構図合わせの読み上げを
  /// 作った意味がない)。畳んだ状態はボタン 1 行 + 細い進捗 + 統計 1 行だけ。
  bool _panelExpanded = false;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    if (widget.active) _initCamera();
    // フレームが一定時間途絶えたらカメラを作り直す (校正からの復帰レースや
    // 一時的なカメラ喪失で灰色のまま固まるのを自己修復する)。
    _watchdog = Timer.periodic(
      const Duration(seconds: 1),
      (_) => _checkStale(),
    );
  }

  void _checkStale() {
    if (!mounted || !widget.active || _payload != null || _camBusy) return;
    final cam = _cam;
    if (cam == null) {
      _initCamera(); // active なのにカメラが無い → 再取得
      return;
    }
    if (!cam.value.isInitialized) return;
    final ref = _lastCallbackAt ?? _camStarted;
    if (ref != null && DateTime.now().difference(ref).inMilliseconds > 2000) {
      _reinit(); // フレームが 2 秒途絶 = 灰色 → 作り直す
    }
  }

  Future<void> _reinit() async {
    if (_camBusy) return;
    await _stopCamera();
    if (mounted && widget.active && _payload == null) await _initCamera();
  }

  @override
  void didUpdateWidget(VcodeReceiveScreen old) {
    super.didUpdateWidget(old);
    if (widget.active == old.active) return;
    if (widget.active) {
      // 再表示: 未完了ならカメラを再取得してスキャン再開
      if (_payload == null && _cam == null) _initCamera();
    } else {
      // 非表示 / 校正表示中: カメラを解放
      _stopCamera();
    }
  }

  Future<void> _initCamera() async {
    if (_camBusy) return;
    _camBusy = true;
    try {
      await _initCameraInner();
    } finally {
      _camBusy = false;
    }
  }

  Future<void> _initCameraInner() async {
    // 直前まで校正/別タブがカメラを掴んでいると初回 initialize が失敗しうるので、
    // 解放待ちのため数回リトライする。
    for (var attempt = 0; attempt < 6; attempt++) {
      if (!mounted || !widget.active) return;
      CameraController? cam;
      try {
        final cams = await availableCameras();
        final back = cams.firstWhere(
          (c) => c.lensDirection == CameraLensDirection.back,
          orElse: () => cams.first,
        );
        cam = CameraController(
          back,
          // セル解像度が密度の上限を決める。7x6 (140 セル幅) は 1080p で足りるが、
          // 9x8 (180) / 11x10 (220) は 6px/セル に 1080/1320px 要るので 2160p 以上が要る。
          // iOS では max がセンサー最大になるので読み替える (preset.dart)
          cameraResolutionFor(_preset),
          enableAudio: false,
          // 30fps を要求する。以前は 60 を要求していて、UI isolate が空くと実際に
          // 約 40fps 届いたが、処理できるのは毎秒 25 枚前後で、捨てるフレームの
          // 受け取り (1 枚 20ms 前後) だけで UI isolate の 8 割を使い、ワーカーの
          // スキャンまで遅くしていた (30 → 40ms)。供給を処理速度に合わせる。
          fps: 30,
          imageFormatGroup: ImageFormatGroup.yuv420,
        );
        await cam.initialize();
        if (!mounted || !widget.active) {
          await cam.dispose();
          return;
        }
        // 露出補正。既定は -1 EV (kDefaultExposureOffsetEv)。露光が短いほど、表示の切り替わりがローリング
        // シャッターの 1 枚に混ざる帯が狭くなる。実測 (11x14 / 20fps、BenQ 白 221/255):
        // 0 EV 平均 75 (57〜97) / -1 EV 81 (60〜102) / -2 EV 86 (78〜96) / -3 EV は
        // 4 本中 1 本が 5.4 KB/s に落ちた (暗すぎて余裕がない)。効果の本体は平均より
        // ばらつきが縮むこと。起動 Intent の ev で上書きできる (計測用)。
        final ev = LaunchArgs.cached.ev ?? kDefaultExposureOffsetEv;
        {
          try {
            // 戻り値は EV ではなく補正インデックス (camerax は 1/6 EV 刻みなので
            // -1 EV → -6)。混同しやすいのでログに単位を書く
            final index = await cam.setExposureOffset(ev);
            debugPrint('[vcode-rx] exposure offset ${ev}EV -> index $index');
          } catch (e) {
            debugPrint('[vcode-rx] exposure offset failed: $e');
          }
        }
        // AE/AF の測光点を画角の中心 (ガイド枠 = コード) に置く。送信画面の周りが黒いと
        // AE が画面全体の平均を目標まで上げようとして、コードの白が飽和したまま露光が
        // 伸びる (iPhone 送信で -2.5 EV でも切り替えの帯が画角の 1/4 あった)。
        // 起動 Intent の aepoint=none で外せる (比較用)
        if ((LaunchArgs.cached.aePoint ?? 'center') != 'none') {
          try {
            await cam.setExposurePoint(const Offset(0.5, 0.5));
            await cam.setFocusPoint(const Offset(0.5, 0.5));
            debugPrint('[vcode-rx] ae/af point -> center');
          } catch (e) {
            debugPrint('[vcode-rx] ae/af point failed: $e');
          }
        }
        // 露光時間の直接指定 (Intent exp=µs [iso=n])。MainActivity.setManualExposure を参照
        final expUs = LaunchArgs.cached.exposureUs;
        if (expUs != null && expUs > 0) {
          final iso =
              LaunchArgs.cached.iso ?? (860000 / expUs).round().clamp(50, 6400);
          try {
            final platform = CameraPlatform.instance;
            if (platform is! AndroidCameraCameraX) {
              throw StateError('camerax platform unavailable');
            }
            final id = PigeonInstanceManager.instance.getIdentifier(
              platform.cameraControl,
            );
            await const MethodChannel('app.vloom/launch').invokeMethod(
              'setExposure',
              {'control': id, 'us': expUs, 'iso': iso},
            );
            debugPrint('[vcode-rx] manual exposure ${expUs}us iso $iso');
          } catch (e) {
            debugPrint('[vcode-rx] manual exposure failed: $e');
          }
        }
        _rx?.dispose();
        _rx = await ScanPool.spawn(LaunchArgs.cached.workers);
        _applyForcedGrid();
        _lastPreviewSize = cam.value.previewSize;
        _camStarted = DateTime.now();
        _camCallbacks = 0;
        await cam.startImageStream(_onFrame);
        await WakelockPlus.enable();
        setState(() {
          _cam = cam;
          _active = true;
          _status = 'スキャン中';
        });
        return;
      } catch (e) {
        try {
          await cam?.dispose();
        } catch (_) {}
        if (attempt == 5) {
          if (mounted) setState(() => _status = 'カメラ初期化失敗: $e');
        } else {
          await Future.delayed(const Duration(milliseconds: 300));
        }
      }
    }
  }

  /// カメラの実配信フレームレート (要求 60fps がどこまで通ったかの検証用)。
  /// 最後のコールバック時刻までで計測する (完了後に表示しても値が減衰しない)。
  double get _camFps {
    final started = _camStarted;
    final last = _lastCallbackAt;
    if (started == null || last == null || _camCallbacks < 2) return 0;
    final sec = last.difference(started).inMilliseconds / 1000.0;
    return sec > 0 ? _camCallbacks / sec : 0;
  }

  DateTime? _lastCallbackAt;

  Future<void> _onFrame(CameraImage img) async {
    _camCallbacks++;
    _lastCallbackAt = DateTime.now();
    if (!_active || _payload != null) return;
    // 走査の本数ぶんまでは並行に処理する。それを超えたら最新の 1 枚だけ持つ
    if (_inFlight >= (_rx?.size ?? 1)) {
      _pendingImg = img; // 古いものは捨てる
      return;
    }
    _inFlight++;
    try {
      final sw = Stopwatch()..start();
      final y = img.planes[0];
      final rotation = _cam?.description.sensorOrientation ?? 90;
      _lastRot = rotation;
      _lastImgW = img.width;
      _lastImgH = img.height;
      _lastStride = y.bytesPerRow;
      // 輝度の値域を間引きで測る。16〜235 に収まっていれば video range で、
      // フル 0〜255 を前提にした閾値判定が効きにくい端末だと分かる。
      // 検出できている間は不要なので、未検出のときだけ測る。
      if (_stats.framesDetected == 0 && _stats.framesSeen % 30 == 0) {
        var lo = 255, hi = 0;
        for (var i = 0; i < y.bytes.length; i += 64) {
          final v = y.bytes[i];
          if (v < lo) lo = v;
          if (v > hi) hi = v;
        }
        _lumaMin = lo;
        _lumaMax = hi;
      }
      // 未検出のあいだ 150 フレームごとに処理済み画像を上書き保存 (PC 解析用)
      // 成功例・失敗例を 1 枚ずつ確保するまでは定期的にダンプを要求する。
      // どちらが返ってくるかはスキャンしてみないと分からないので、結果を見て振り分ける。
      final wantDump =
          ((_needOkDump || _needNgDump) &&
              _stats.framesSeen > 0 &&
              _stats.framesSeen % 20 == 0) ||
          _lowDumps < LaunchArgs.cached.dumpLow;
      // 成績に関係なく等間隔で保存する標本。間隔は 1MB の受信 (約 200 枚) から
      // 20 枚ほど取れる幅
      final wantAllDump =
          _allDumps < LaunchArgs.cached.dumpAll &&
          _stats.framesSeen % LaunchArgs.cached.dumpEvery == 0;
      final rx = _rx;
      if (rx == null) return;
      // 位置検出 (acquire): 画面全体を sweep して実際の 4 隅を取得し、ポップアップで確認 →
      // seed でトラッキングの種にする。以降 scan() は最初からその位置にロックして始まる。
      if (_acquireRequested) {
        _acquireRequested = false;
        final wasAuto = _acquireIsAuto;
        final rep = await rx.acquire(
          y: TransferableTypedData.fromList([y.bytes]),
          width: img.width,
          height: img.height,
          stride: y.bytesPerRow,
          rotationDeg: rotation,
          // 自動起動は軽量版 (locate のみ)。全 sweep は見つからないとき数秒
          // フレーム処理を止めるので、手動ボタンのときだけ使う
          thorough: !wasAuto,
        );
        if (!mounted || !_active) return;
        setState(() {
          _acquiring = false;
          if (rep.detected) {
            // 検出 4 隅をハイライト表示用に保持 (確認ダイアログの背後に見える)
            _setDetected(rep.corners, rep.imgW, rep.imgH, rep.rot, rep.cellsW, rep.cellsH);
          }
        });
        if (_acquireIsAuto) {
          // 自動起動: 確認を挟まずそのままロックする。位置がずれていても、
          // 検出が続かなければクールダウン後にまた自動取得が走って直る。
          _acquireIsAuto = false;
          if (rep.detected) {
            _adoptDetectedGrid(rep);
            rx.seed(
              rot: rep.rot,
              gridW: rep.gridW,
              gridH: rep.gridH,
              corners: rep.corners.toList(),
            );
            if (mounted) setState(() => _seeded = true);
          }
          return;
        }
        await _showAcquireDialog(rep, rx);
        return;
      }
      // 同期版 (実験): 非同期だと完了通知が UI の描画待ちに掛かり、Rust の 31ms に対して
      // 往復 55ms になっていた。プレビューはネイティブのテクスチャなので止まらない。
      final report = await rx.scan(
        y: TransferableTypedData.fromList([y.bytes]),
        width: img.width,
        height: img.height,
        stride: y.bytesPerRow,
        rotationDeg: rotation,
        guideFrac: kVcodeGuideFrac,
        debugDump: wantDump || wantAllDump,
      );
      sw.stop();
      if (!_active || _payload != null) return;
      if (report.debugGray != null) {
        if (wantAllDump) {
          _allDumps++;
          _saveDump(report, 'all$_allDumps');
        } else if (report.detected &&
            report.blocksTotal > 0 &&
            report.blocksOk * 100 < report.blocksTotal * 70 &&
            _lowDumps < LaunchArgs.cached.dumpLow) {
          _lowDumps++;
          _saveDump(report, 'low$_lowDumps');
        } else if (report.detected && _needOkDump) {
          _needOkDump = false;
          _saveDump(report, 'ok');
        } else if (!report.detected && _needNgDump) {
          _needNgDump = false;
          _saveDump(report, 'ng');
        }
      }

      _stats.addScan(
        scanMs: sw.elapsedMilliseconds,
        detected: report.detected,
        rotateUs: report.rotateUs,
        decodeUs: report.decodeUs,
      );
      if (report.detected) {
        _stats.addDetected(
          tracked: report.tracked,
          ok: report.blocksOk,
          total: report.blocksTotal,
        );
        // 試験フレーム (「さがす」モード) はファイルを運んでいない。デコーダには入れず、
        // 設定ごとの速さを数えるだけにする
        final probeTag = ProbeTag.parse(report.oti);
        if (probeTag != null) _onProbeFrame(probeTag, report);
        final packets = probeTag == null ? report.packets : const <Uint8List>[];
        if (probeTag == null) {
          _dec ??= FountainDecoder(otiBytes: report.oti);
          _cov ??= PacketCoverage.fromOti(report.oti);
        }
        var done = false;
        final swDec = Stopwatch()..start();
        for (final p in packets) {
          _stats.packetsAdded++;
          _cov!.add(p);
          if (_dec!.addPacket(packet: p)) {
            done = true;
            break;
          }
        }
        _stats.addDecodeMs(swDec.elapsedMilliseconds);
        debugPrint(
          '[vcode-rx] seq=${report.frameSeq} '
          'blocks=${report.blocksOk}/${report.blocksTotal} '
          'pkts=${_stats.packetsAdded} scan=${_stats.lastScanMs}ms '
          'tracked=${report.tracked} done=$done',
        );
        // 半分程度しか取れないフレームは、どの行が落ちたかを出す。散らばって
        // いれば露出やピント、片側や帯なら位置合わせのずれ、と切り分けられる。
        if (report.blocksTotal > 0 &&
            report.blocksOk * 100 < report.blocksTotal * 60 &&
            report.blockOk.isNotEmpty) {
          final gw = report.cellsW ~/ 20;
          final rows = <String>[];
          for (var i = 0; gw > 0 && i + gw <= report.blockOk.length; i += gw) {
            rows.add(
              report.blockOk
                  .sublist(i, i + gw)
                  .map((b) => b ? '#' : '.')
                  .join(),
            );
          }
          debugPrint('[vcode-rx] blockmap ${rows.join("|")}');
        }
        if (done) {
          // 復元 (CRC 検証 + 履歴保存) は UI を数百 ms 塞ぐことがある。固まって
          // 見えないよう先に表示を切り替え、1 フレーム描かせてから進める
          setState(() {
            _restoring = true;
            _status = '復元中…';
          });
          await Future<void>.delayed(Duration.zero);
          // エンドツーエンド CRC-32 検証。不一致 = ゴミパケットが RaptorQ を
          // 汚染して復元結果が破損 → デコーダを捨てて受信をやり直す
          final payload = vcodeUnwrapPayload(payload: _dec!.payload()!);
          if (payload == null) {
            setState(() => _restoring = false);
            _stats.integrityFails++;
            debugPrint(
              '[vcode-rx] 整合性エラー: 復元結果が破損 '
              '(${_stats.integrityFails} 回目)。デコーダを再作成して受信続行',
            );
            _dec = null;
            _cov = null;
            return;
          }
          _onComplete(payload);
          return;
        }
        _missStreak = 0;
        // 追従が安定したら露出をロックする。数十秒以上の転送だけが、1 回の停止と
        // 引き換えに露出の安定 (白飛び防止) を得る形にしている。
        _detectStreak++;
        if (!_camLocked && _detectStreak >= kCamLockStreak) {
          _camLocked = true;
          _lockCamera(true);
        }
        // ピントの探り直しを見つけて、合っている瞬間に AF を固定する
        _updateFocusLock(report.blocksOk, report.blocksTotal);
        // 「今どこを読んでいるか」を毎フレーム更新する。追従中も枠が動くので、
        // ロックできているかが画面を見れば分かる。
        if (report.corners.length >= 8) {
          _setDetected(report.corners, report.imgW, report.imgH, report.rot,
              report.cellsW, report.cellsH);
        }
      } else {
        // 中央ガイド枠での探索が「連続して」失敗するなら、広域 sweep に切り替えて
        // 位置を取りに行く。acquire は重いが一度きりで、成功後はトラッキングに
        // 移るので定常コストは増えない (= 手で位置を合わせる必要がなくなる)。
        // 単発のフレーム落ちで走らせないよう、連続失敗数で判定する。
        _missStreak++;
        _detectStreak = 0;
        // 見失ったら枠を消す (古い位置を出したままにしない)
        if (_missStreak > kAutoAcquireMissFrames ~/ 2) _det = null;
        // ロングロスト: ロックしたピントが合わなくなった (距離が変わった等) 可能性が
        // あるので AF に戻す。短いロストでは解除しない。モード切替はカメラの
        // フレーム供給を 0.5〜2 秒止めるので、acquire (20) より長く粘る
        if (_camLocked && _missStreak >= kCamUnlockMissFrames) {
          _camLocked = false;
          _lockCamera(false);
        }
        if (_autoAcquire &&
            _missStreak >= kAutoAcquireMissFrames &&
            _stats.framesSeen >= _autoAcquireAt) {
          _autoAcquireAt = _stats.framesSeen + kAutoAcquireCooldownFrames;
          _missStreak = 0;
          _autoAcquireCount++;
          _startAcquire(auto: true);
        }
      }
      if (!report.detected) _stats.addMiss(report.error);
      if (!report.detected && _stats.framesSeen % 30 == 0) {
        _lastError = report.error;
        debugPrint(
          '[vcode-rx] not detected (${report.error}) '
          'scan=${_stats.lastScanMs}ms seen=${_stats.framesSeen} '
          'detected=${_stats.framesDetected} '
          'camFps=${_camFps.toStringAsFixed(1)}',
        );
      }
      if (mounted && _stats.framesSeen % 5 == 0) setState(() {});
      // 1 枚あたりの処理時間の内訳 (30 枚ごとの平均)。受信の処理速度が天井になった
      // とき、Rust のスキャンか、FRB の Y 面コピーか、デコーダ投入か、を切り分ける
      final timing = _stats.addFrameTime(sw.elapsedMilliseconds);
      if (timing != null) debugPrint('[vcode-rx] timing avg/frame: $timing');
    } finally {
      _inFlight--;
      final next = _pendingImg;
      _pendingImg = null;
      if (next != null && _active && _payload == null) {
        // 待たせていたフレームをすぐ処理する (await しない: この関数の呼び出し元は
        // カメラのストリームで、戻りを待たれていない)
        unawaited(_onFrame(next));
      }
    }
  }

  void _setDetected(List<double> corners, int imgW, int imgH, int rot,
      int cellsW, int cellsH) {
    _det = DetectedCode(
      corners: corners.toList(),
      imgW: imgW,
      imgH: imgH,
      rot: rot,
      cellsW: cellsW,
      cellsH: cellsH,
    );
    _lastCellsW = cellsW;
  }

  int get _sensorOrientation => _cam?.description.sensorOrientation ?? 90;

  Future<void> _saveDump(VcodeScanReport report, String tag) async {
    try {
      // 外部ストレージを優先する。内部 (getApplicationDocumentsDirectory) だと
      // release ビルドでは run-as が使えず adb で取り出せない。ここは PC で
      // scan_file にかけるためのダンプなので、取り出せないと意味がない。
      //   adb shell ls /sdcard/Android/data/<pkg>/files/
      final dir =
          await getExternalStorageDirectory() ??
          await getApplicationDocumentsDirectory();
      final path =
          '${dir.path}/vcode_${tag}_${report.debugW}x${report.debugH}.gray';
      await File(path).writeAsBytes(report.debugGray!);
      debugPrint('[vcode-rx] DUMP saved: $path (err=${report.error})');
    } catch (e) {
      debugPrint('[vcode-rx] DUMP 保存失敗: $e');
    }
  }

  Future<void> _onComplete(Uint8List rawPayload) async {
    // ファイル名/MIME ヘッダがあれば元の名前・種別で保存。無ければ推測+タイムスタンプ名。
    final meta = vcodeUnwrapFile(buf: rawPayload);
    final payload = meta?.data ?? rawPayload;
    final (name, mime) = resolveReceivedName(
      payload,
      headerName: meta?.name ?? '',
      headerMime: meta?.mime ?? '',
    );
    final elapsed = _stats.elapsedSinceFirstDetect();
    setState(() {
      _payload = payload;
      _elapsed = elapsed;
      _restoring = false;
      _status = '受信完了';
    });
    final ms = elapsed.inMilliseconds;
    final kbps = ms > 0 ? (payload.length / 1024) / (ms / 1000) : 0.0;
    final note =
        '${(ms / 1000).toStringAsFixed(2)}s'
        ' · ${kbps.toStringAsFixed(1)}KB/s'
        ' · cam${_camFps.toStringAsFixed(0)}fps'
        ' · 検出${_stats.framesDetected}/${_stats.framesSeen}(追従${_stats.framesTracked})'
        ' · blk${_stats.blocksOk} · pkt${_stats.packetsAdded}'
        ' · scan${_stats.avgScanMs}ms'
        ' (rot${_stats.avgRotateMs.toStringAsFixed(1)}/dec${_stats.avgDecodeMs.toStringAsFixed(1)}ms)';
    debugPrint(
      '[vcode-rx] COMPLETE: $name ${payload.length} bytes in ${ms}ms, $note',
    );
    // 条件を振って自動計測するとき、結果を画面から目で読むのは現実的でない。
    // 画面と同じ内容を 1 行で吐いておけば adb logcat でそのまま集計できる。
    debugPrint(
      '[vloom-stats] '
      '${_statsRows().map((r) => "${r.$1.trim()}=${r.$2}").join(" | ")}',
    );
    // 履歴に保存。名前/種別はヘッダ優先、無ければ内容推定。
    try {
      final slot = HistoryStore.instance.reserveReceivedPath();
      await File(slot.path).writeAsBytes(payload);
      await HistoryStore.instance.registerReceived(
        slot.id,
        name,
        mime,
        payload.length,
        note: note,
      );
      setState(() {
        _savedItem = HistoryStore.instance.received.first;
      });
    } catch (e) {
      debugPrint('[vcode-rx] 履歴保存失敗: $e');
    }
    await _stopCamera();
  }

  /// 試験フレームが写った 1 枚ぶんを集計に足す。
  void _onProbeFrame(ProbeTag tag, VcodeScanReport report) {
    final probe = _probe ??= ProbeStats();
    // 送信側は格子を切り替えていくので、固定したままだと最初の格子しか読めない。
    // 格子はスキャナに写りから判別させる
    if (_forcedGrid != kGridAuto) {
      _presetIndex = -1;
      _forcedGrid = kGridAuto;
      _applyForcedGrid();
    }
    probe.add(
      tag,
      report.packets,
      report.cellsW ~/ 20,
      (report.cellsH - 56) ~/ 20, // 上下のストリップ (28 セル × 2) を除いた段数
    );
    if (_stats.framesSeen % 30 == 0) {
      debugPrint('[vloom-probe] ${probe.summary()}');
    }
  }

  /// 探索する格子をスキャナに反映する。送信側の格子が分かっているなら固定するほど
  /// 初回検出と acquire が速い (候補数に比例したコストが 1 候補分になる)。
  void _applyForcedGrid() {
    final rx = _rx;
    if (rx == null) return;
    if (_forcedGrid == kGridAuto) {
      rx.setLayout(gridW: 0, gridH: 0);
    } else {
      final p = _forcedGrid.split('x');
      rx.setLayout(gridW: int.parse(p[0]), gridH: int.parse(p[1]));
    }
  }

  /// acquire が検出した格子を受信設定に取り込む。格子はフレームのヘッダが自己記述して
  /// いるので、送信側と食い違っていてもユーザーに合わせ直させるのではなく、こちらが
  /// 検出結果に追従する (プリセット違いで「見えているのに掴めない」を防ぐ)。
  void _adoptDetectedGrid(VcodeAcquireReport rep) {
    final detected = '${rep.gridW}x${rep.gridH}';
    if (_forcedGrid == detected) return;
    final matched = kPresets.indexWhere((p) => p.grid == detected);
    setState(() {
      _forcedGrid = detected;
      _presetIndex = matched; // 一致するプリセットが無ければ -1 (カスタム)
    });
    _applyForcedGrid();
    showToast(
      context,
      '送信側の格子は $detected でした。受信設定を合わせました'
      '${matched >= 0 ? ' (${kPresets[matched].name})' : ''}',
    );
  }

  /// フォーカスと露出のロック/解除。据え置きの AF ハンチング (数秒おきのピント
  /// 探り直しで 1〜2 秒検出が全滅する) を止める。ブラウザにはできないネイティブの強み。
  /// 未対応端末では例外になるだけなので握りつぶす。
  /// 満点ブロック率の推移から AF の探り直しを見つけ、合っている瞬間に固定する。
  /// 判断の根拠と定数は kFocusGoodRatio 付近を参照。
  void _updateFocusLock(int ok, int total) {
    // 露出だけをロックする設定 (既定) でも、AF の固定はこちらで別に判断する。
    // camlock=none は「カメラに一切触らない」、both は「_lockCamera が AF も握る」
    // 指定なので、どちらもこちらでは触らない。afhunt=0 で比較計測用に切れる。
    final mode = LaunchArgs.cached.camLock ?? 'ae';
    if (mode == 'none' || mode == 'both') return;
    if (LaunchArgs.cached.afHunt == 0) return;
    final huntsBefore = _focus.hunts;
    final action = _focus.update(ok, total, framesDetected: _stats.framesDetected);
    if (_focus.hunts != huntsBefore) {
      debugPrint('[vcode-rx] focus hunt ${_focus.hunts}/$kFocusHuntCycles');
    }
    switch (action) {
      case FocusAction.lock:
        debugPrint('[vcode-rx] focus lock (回収率 ${(ok * 100 / total).round()}%, '
            '${_focus.attempts}/$kFocusLockAttempts 回目)');
        _setFocusLocked(true);
      case FocusAction.unlock:
        debugPrint('[vcode-rx] focus unlock (ピントが外れた)');
        _setFocusLocked(false);
      case null:
        break;
    }
  }

  /// AF の判定状態を初期に戻す (受信のやり直し・カメラの作り直し)。
  /// 固定していたら AF に返してから捨てる。
  void _resetFocusState() {
    if (_focus.locked) _setFocusLocked(false);
    _focus = FocusHunter();
  }

  /// AF だけを固定/解除する (露出は _lockCamera が別に扱う)。
  /// FocusMode の切替はフレーム供給を 0.5〜5 秒止めることがあるので、
  /// 呼ぶのは _updateFocusLock が「探り直しが起きている」と判断したときだけ。
  Future<void> _setFocusLocked(bool lock) async {
    final cam = _cam;
    if (cam == null || !cam.value.isInitialized) return;
    try {
      await cam.setFocusMode(lock ? FocusMode.locked : FocusMode.auto);
    } catch (e) {
      debugPrint('[vcode-rx] setFocusMode failed: $e');
    }
  }

  Future<void> _lockCamera(bool lock) async {
    final cam = _cam;
    if (cam == null || !cam.value.isInitialized) return;
    // 既定は露出だけ固定する。ピント (FocusMode) の切替はカメラのフレーム供給を
    // 0.5〜5 秒止めることがあり (フレームログで確認: lock 直後に 3 秒間 1 枚も
    // 来なかった)、100KB の転送が 1.5 秒で終わる状況では致命的だった。露出の
    // 固定は止まらず、白飛びを防ぐ効果だけが残る。実測 (11x14 / 20fps 各 4 本):
    // none 平均 71 / ae 76 / both 49 KB/s。both だけ 1〜5 秒の空白が出る。
    // 起動引数 camlock=none|ae|both で切り替えられる (計測用)。
    final mode = LaunchArgs.cached.camLock ?? 'ae';
    debugPrint('[vcode-rx] camera ${lock ? "lock" : "unlock"} ($mode)');
    if (mode == 'none') return;
    if (mode != 'ae') {
      try {
        await cam.setFocusMode(lock ? FocusMode.locked : FocusMode.auto);
      } catch (_) {}
    }
    try {
      await cam.setExposureMode(lock ? ExposureMode.locked : ExposureMode.auto);
    } catch (_) {}
  }

  /// 受信中のやり直し。集めたパケットと追従状態を捨てて最初から始める。
  /// カメラは開いたままなので即座に再開する。
  void _restartReceive() {
    _resetFocusState();
    setState(() {
      _dec = null;
      _probe = null;
      _cov = null;
      _stats = RxStats();
      _elapsed = null;
      _missStreak = 0;
      _detectStreak = 0;
      _autoAcquireAt = 0;
      _autoAcquireCount = 0;
      _seeded = false;
      _det = null;
      _lastError = null;
    });
    // スキャナの追従状態も捨てる (古い位置に引きずられないように)
    _rx?.reset();
    _applyForcedGrid();
    if (_camLocked) {
      _camLocked = false;
      _lockCamera(false);
    }
    showToast(context, '受信をやり直します');
  }

  /// カメラ解像度を変えて開き直す (プレビュー中でも即反映)
  /// プリセットを選ぶ。画面のチップから呼ぶ。
  /// 起動時の指定 (LaunchArgs) は _presetIndex の初期値として効くので、
  /// ここを通らずにカメラが正しい解像度で立ち上がる。
  void _selectPreset(int i) {
    if (i < 0 || i >= kPresets.length) return;
    setState(() {
      _presetIndex = i;
      _forcedGrid = kPresets[i].grid;
    });
    RxPrefs.savePreset(i); // 次回の手起動でも同じ格子で立ち上がる
    _applyForcedGrid();
    _changePreset(kPresets[i].preset);
  }

  Future<void> _changePreset(ResolutionPreset p) async {
    if (p == _preset) return;
    setState(() => _preset = p);
    await _stopCamera();
    await _initCamera();
  }

  Future<void> _stopCamera() async {
    _active = false;
    final cam = _cam;
    _cam = null;
    if (cam != null) {
      try {
        await cam.stopImageStream();
      } catch (_) {}
      await cam.dispose();
    }
    await WakelockPlus.disable();
  }

  Future<void> _reset() async {
    await _stopCamera();
    setState(() {
      _dec = null;
      _probe = null;
      _payload = null;
      _savedItem = null;
      _cov = null;
      _stats = RxStats();
      _elapsed = null;
      _acquireRequested = false;
      _acquiring = false;
      _acquireIsAuto = false;
      _autoAcquireAt = 0;
      _autoAcquireCount = 0;
      _missStreak = 0;
      _detectStreak = 0;
      _camLocked = false; // カメラは作り直すので AF/AE は自動に戻る
      // カメラごと作り直すため、AF の固定も判定の履歴も無効になる
      _focus = FocusHunter();
      _seeded = false;
      _det = null;
      _status = 'カメラ起動待ち';
    });
    await _initCamera();
  }

  /// 次フレームで acquire (位置検出) を走らせる。固定後の一回きりの重い処理なので
  /// スピナーを出して待つ (その間カメラフレームは _busy でスキップされる)。
  /// 広域 sweep で 4 隅を取得する。auto=true は自動起動で、確認ダイアログを出さず
  /// 検出できたらそのまま seed する。オーバーレイも出さない (頻繁な暗転を避ける)。
  void _startAcquire({bool auto = false}) {
    if (!_active || _payload != null || _acquireRequested || _acquiring) return;
    _acquireIsAuto = auto;
    setState(() {
      _acquiring = !auto;
      _acquireRequested = true;
      if (!auto) _det = null; // 手動時は前回のハイライトを消す
    });
  }

  /// acquire 結果を中央ポップアップで確認。確定なら seed して受信継続、やり直しなら再取得。
  Future<void> _showAcquireDialog(VcodeAcquireReport rep, ScanPool rx) async {
    if (!mounted) return;
    if (!rep.detected) {
      await showDialog<void>(
        context: context,
        builder: (ctx) => AlertDialog(
          title: const Text('位置を検出できませんでした'),
          content: const Text('コードが画面に写っているか、ピントが合っているか確認して、もう一度お試しください。'),
          actions: [
            TextButton(
              onPressed: () => Navigator.pop(ctx),
              child: const Text('閉じる'),
            ),
          ],
        ),
      );
      return;
    }
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: const Text('位置を検出しました'),
        content: Text(
          '格子 ${rep.gridW}×${rep.gridH} · 直近 ${rep.blocksOk}/${rep.blocksTotal} ブロック\n'
          'この位置に固定したまま受信を開始しますか?',
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(ctx, false),
            child: const Text('やり直す'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(ctx, true),
            child: const Text('この位置で受信開始'),
          ),
        ],
      ),
    );
    if (!mounted) return;
    if (confirmed == true) {
      _adoptDetectedGrid(rep);
      // 検出した 4 隅・回転・格子をトラッキングの種にする。中央ガイド枠に頼らず即ロック。
      rx.seed(
        rot: rep.rot,
        gridW: rep.gridW,
        gridH: rep.gridH,
        corners: rep.corners.toList(),
      );
      setState(() => _seeded = true);
    } else {
      _startAcquire();
    }
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state == AppLifecycleState.paused) _stopCamera();
  }

  @override
  void dispose() {
    _rx?.dispose();
    _rx = null;
    _watchdog?.cancel();
    WidgetsBinding.instance.removeObserver(this);
    _stopCamera();
    super.dispose();
  }

  /// 受信ファイルを端末の任意の場所へ保存 (SAF ダイアログ)。結果をスナックバーで通知。
  Future<void> _saveToFile(HistoryItem item) async {
    final ok = await saveReceivedToFile(item);
    if (!mounted) return;
    if (!ok) {
      showToast(context, '保存をキャンセルしました');
      return;
    }
    // 保存先は OS のダイアログでユーザーが選ぶためアプリには返らない。
    // パスを出す代わりに、その場で開ける導線を添える。
    final file = HistoryStore.instance.receivedFile(item);
    showToast(
      context,
      '端末に保存しました',
      kind: ToastKind.success,
      action: file == null
          ? null
          : SnackBarAction(
              label: '開く',
              onPressed: () => openWithDefaultApp(context, file),
            ),
    );
  }

  /// 受信完了時の統計。テーブル表示とクリップボードコピーで同じデータを使う
  /// (条件を振って比べるとき、目視で書き写すと必ず取りこぼすため)。
  List<(String, String)> _statsRows() {
    final p = _payload!;
    final ms = _elapsed?.inMilliseconds ?? 0;
    final kbps = ms > 0 ? (p.length / 1024) / (ms / 1000) : 0.0;
    // 条件を一緒に残す: 同じ数字でも解像度と格子が違えば比較にならない。
    final preview = _lastPreviewSize;
    final rows = <(String, String)>[
      if (isTestImage(_savedItem?.name ?? '')) ('計測データ', _savedItem!.name),
      ('サイズ', '${p.length} B'),
      (
        '所要時間 (初検出→完了)',
        ms >= 1000 ? '${(ms / 1000).toStringAsFixed(2)} 秒' : '$ms ms',
      ),
      ('実効スループット', '${kbps.toStringAsFixed(1)} KB/s'),
      (
        'カメラ解像度',
        preview == null
            ? '-'
            : '${preview.width.round()}×${preview.height.round()}',
      ),
      ('格子指定', _forcedGrid == kGridAuto ? '自動 (候補総当たり)' : _forcedGrid),
      ('カメラフレーム数',
          '${_stats.framesSeen} (検出 ${_stats.framesDetected} / 追従 ${_stats.framesTracked})'),
      ('カメラ実効fps', _camFps.toStringAsFixed(1)),
      ('回収ブロック', '${_stats.blocksOk} (部分回収込み)'),
      ('　1 枚あたり分布', _stats.histText()),
      ('未検出の内訳', _stats.errorKindsText()),
      // AF の探り直しをいくつ観測し、固定できたか。手持ちの遅さがピント由来かを
      // 実機ログで確かめるための一次情報 (三脚では 0 回のままになるはず)
      ('AF 探り直し', _focus.summary()),
      ('投入パケット', '${_stats.packetsAdded}'),
      ('distinct パケット', '${_cov?.distinct ?? 0}'),
      if (_stats.integrityFails > 0)
        ('整合性エラー再試行', '${_stats.integrityFails} 回'),
      (
        '平均スキャン時間',
        _stats.scanCount > 0 ? '${_stats.avgScanMs} ms' : '-',
      ),
      (
        '　うち回転コピー',
        _stats.scanCount > 0 ? '${_stats.avgRotateMs.toStringAsFixed(1)} ms' : '-',
      ),
      (
        '　うち探索・デコード',
        _stats.scanCount > 0 ? '${_stats.avgDecodeMs.toStringAsFixed(1)} ms' : '-',
      ),
    ];
    return rows;
  }

  /// 計測ログ用にタブ区切りで書き出す。表計算にそのまま貼れる。
  Future<void> _copyStats() async {
    // 受信側の設定がプリセットのどれとも一致しない場合、_presetIndex は -1 になる
    final preset = _presetIndex >= 0 ? kPresets[_presetIndex] : null;
    final header = <(String, String)>[
      ('プリセット', preset?.name ?? 'カスタム'),
      ('格子(受信設定)', _forcedGrid),
      ('bit/セル', preset != null ? '${preset.bpc}' : '-'),
      ('送信fps(想定)', preset != null ? '${preset.fps}' : '-'),
      (
        '理論スループット',
        preset != null
            ? '${preset.theoreticalKbps.toStringAsFixed(1)} KB/s'
            : '-',
      ),
    ];
    final text = [
      ...header,
      ..._statsRows(),
    ].map((r) => '${r.$1.trim()}\t${r.$2}').join('\n');
    await Clipboard.setData(ClipboardData(text: text));
    if (mounted) {
      ScaffoldMessenger.of(
        context,
      ).showSnackBar(const SnackBar(content: Text('統計をコピーしました')));
    }
  }

  /// 密度を落としたほうが速い状況なら、その誘導文を返す。
  /// 検出はできているのに満点ブロックが集まらない = 1 セルあたりの画素が足りない。
  String? _sparseHint() {
    final ratio = _stats.recoveryRatio;
    if (_stats.framesDetected < kSparseHintFrames || ratio == null) return null;
    if (_lastCellsW <= kSparseHintCellsW) return null; // すでに十分粗い
    if (_forcedGrid == kSparseHintGrid) return null;
    if (ratio >= kSparseHintRatio) return null;
    return '読めているのは ${(ratio * 100).round()}% です。'
        'タップして $kSparseHintGrid にすると速いことがあります';
  }

  List<Widget> _hintWidgets() {
    final sparse = _sparseHint();
    final guide = _det?.guideHint(_sensorOrientation);
    return [
      if (sparse != null)
        HintBadge(sparse, onTap: () {
          // 送信側も同じ格子にしないと読めないので、切り替えたことが分かるように
          // 通常の格子選択と同じ経路 (_selectPreset) を通す
          final i = kPresets.indexWhere((p) => p.grid == kSparseHintGrid);
          if (i >= 0) _selectPreset(i);
        }),
      if (guide != null) HintBadge(guide),
    ];
  }

  /// プレビュー上の状態バッジ。「探索中 / 位置検出中 / 追従中」を明示する。
  /// 内部でロックしていても画面に出ないと分からないため、状態を必ず可視化する。
  Widget _statusBadge() {
    final (String label, Color color, IconData icon) = switch (this) {
      _ when _acquireRequested || _acquiring => (
        '位置を検出中…',
        Colors.amber,
        Icons.travel_explore,
      ),
      _ when _stats.framesDetected > 0 && _missStreak < kBadgeHoldMissFrames => (
        _camLocked
            ? '追従中 (露出固定)'
            : _seeded
            ? '追従中 (位置固定)'
            : '追従中',
        Colors.cyanAccent,
        Icons.center_focus_strong,
      ),
      _ when _autoAcquire => ('位置を探しています…', Colors.orangeAccent, Icons.search),
      _ => ('枠にコードを合わせてください', Colors.orangeAccent, Icons.crop_free),
    };
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
      decoration: overlayBadgeDecoration(border: color),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(icon, size: 16, color: color),
          const SizedBox(width: 6),
          Text(
            label,
            style: TextStyle(
              color: color,
              fontSize: 13,
              fontWeight: FontWeight.w600,
            ),
          ),
        ],
      ),
    );
  }

  /// いま探している格子のチップ。タップでプリセット選択シートを開く。
  Widget _gridChip() {
    final label = _presetIndex >= 0
        ? '${kPresets[_presetIndex].grid} ${kPresets[_presetIndex].name}'
        : _forcedGrid == kGridAuto
        ? '格子: 自動'
        : _forcedGrid;
    return InkWell(
      onTap: _showGridPicker,
      borderRadius: BorderRadius.circular(16),
      child: Container(
        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
        decoration: overlayBadgeDecoration(border: Colors.white70, borderWidth: 1.2),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            const Icon(Icons.grid_on, size: 14, color: Colors.white70),
            const SizedBox(width: 5),
            Text(
              label,
              style: const TextStyle(
                color: Colors.white,
                fontSize: 13,
                fontWeight: FontWeight.w600,
              ),
            ),
            const Icon(Icons.arrow_drop_down, size: 18, color: Colors.white70),
          ],
        ),
      ),
    );
  }

  void _showGridPicker() {
    showModalBottomSheet<void>(
      context: context,
      showDragHandle: true,
      builder: (ctx) => SafeArea(
        child: ListView(
          shrinkWrap: true,
          children: [
            const Padding(
              padding: EdgeInsets.fromLTRB(16, 0, 16, 8),
              child: Text(
                '格子 — 送信側と同じものを選ぶ',
                style: TextStyle(fontWeight: FontWeight.w600),
              ),
            ),
            for (var i = 0; i < kPresets.length; i++)
              ListTile(
                dense: true,
                leading: Icon(
                  _presetIndex == i
                      ? Icons.radio_button_checked
                      : Icons.radio_button_off,
                  color: _presetIndex == i
                      ? Theme.of(ctx).colorScheme.primary
                      : null,
                ),
                title: Text('${kPresets[i].grid}  ${kPresets[i].name}'),
                subtitle: Text(
                  kPresets[i].description,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                ),
                onTap: () {
                  Navigator.pop(ctx);
                  _selectPreset(i);
                },
              ),
          ],
        ),
      ),
    );
  }

  /// 計測用の設定 (常用しないので折りたたむ)。カメラ解像度は px/セル の上限を、
  /// 格子固定は初回検出/acquire の速さを決める。
  Widget _measureSettings() {
    final small = Theme.of(context).textTheme.bodySmall;
    return ExpansionTile(
      title: Text('計測設定', style: small),
      tilePadding: EdgeInsets.zero,
      childrenPadding: const EdgeInsets.only(bottom: 8),
      children: [
        SwitchListTile(
          dense: true,
          contentPadding: EdgeInsets.zero,
          title: Text('自動追従', style: small),
          subtitle: Text(
            '検出できないとき自動で広域探索して位置を掴む (手で合わせる必要がない)',
            style: TextStyle(fontSize: 11, color: Theme.of(context).hintColor),
          ),
          value: _autoAcquire,
          onChanged: (v) => setState(() {
            _autoAcquire = v;
            _missStreak = 0;
          }),
        ),
        // 送信側と同じプリセットを選ぶ。格子と解像度が自動で揃うので、
        // 「どれを選べばいいか分からない」状態にならない。
        Row(
          children: [
            Text('プリセット', style: small),
            const SizedBox(width: 12),
            Expanded(
              child: Wrap(
                spacing: 6,
                children: [
                  for (var i = 0; i < kPresets.length; i++)
                    ChoiceChip(
                      label: Text(kPresets[i].name),
                      selected: _presetIndex == i,
                      onSelected: (_) => _selectPreset(i),
                    ),
                ],
              ),
            ),
          ],
        ),
        Text(
          _presetIndex >= 0
              ? '格子 ${kPresets[_presetIndex].grid} · '
                    '${kPresets[_presetIndex].cellsWide} セル幅 · '
                    '送信側も同じものを選ぶこと'
              : 'カスタム',
          style: TextStyle(fontSize: 11, color: Theme.of(context).hintColor),
        ),
      ],
    );
  }

  @override
  Widget build(BuildContext context) {
    final cam = _cam;
    final cov = _cov;
    final total = cov?.kt; // 必要 source パケット数

    if (_payload != null) return _resultView();
    if (cam == null || !cam.value.isInitialized) {
      return Center(child: Text(_status));
    }
    // カメラを画面いっぱいに敷き、操作系はその上に重ねる。カメラ画像は全体が
    // 見える大きさで中央に置かれる (VcodeCameraView) ので、縦長の画面では上下に
    // 余りが出る。UI は主にその余りに乗るため、映像を隠す量が最小で済む。
    return Stack(
      fit: StackFit.expand,
      children: [
        ColoredBox(
          color: Colors.black,
          child: VcodeCameraView(
            cam,
            guideAspect: _guideAspect(),
            // 操作パネルの反対側へ寄せて、パネルが映像 (とくにコードの下側の四隅) に
            // 重ならないようにする
            alignment: _panelAtTop
                ? const Alignment(0, 0.3)
                : const Alignment(0, -0.3),
            // 検出領域のハイライト (シアン)。緑のガイド枠と区別できる色。
            // カメラ画像の矩形に重ねて描く = 画像座標との対応がそのまま保たれる。
            overlay: _det == null
                ? null
                : CustomPaint(
                    painter: DetectedQuadPainter(
                      _det!,
                      sensorOrientation: cam.description.sensorOrientation,
                    ),
                  ),
          ),
        ),
        // 状態バッジは操作パネルと反対側に置く (重なると読めなくなる)
        Positioned(
          top: _panelAtTop ? null : 10,
          bottom: _panelAtTop ? 10 : null,
          left: 0,
          right: 0,
          child: SafeArea(
            top: !_panelAtTop,
            bottom: _panelAtTop,
            // 状態バッジと格子チップ。格子は送信側と合っていないと永遠に掴めないので、
            // 折りたたみパネルの中ではなく常に見せて、その場で切り替えられるようにする
            child: Center(
              child: Wrap(
                spacing: 6,
                runSpacing: 6,
                alignment: WrapAlignment.center,
                crossAxisAlignment: WrapCrossAlignment.center,
                children: [_statusBadge(), _gridChip(), ..._hintWidgets()],
              ),
            ),
          ),
        ),
        // 構図合わせの読み上げ。バッジと同じ側 (操作パネルの反対側) に出す
        Positioned(
          top: _panelAtTop ? null : 62,
          bottom: _panelAtTop ? 62 : null,
          left: 12,
          right: 12,
          child: SafeArea(
            top: !_panelAtTop,
            bottom: _panelAtTop,
            // 「さがす」の間は一覧の中に出す (ここは映像の上端で、コードの隅に被る)
            child: _probe != null ? const SizedBox.shrink() : _alignPanel(),
          ),
        ),
        // 「さがす」の一覧はカメラ映像の中央に、幅を絞って重ねる。コードの四隅は
        // はみ出していないかを目で確かめる場所なので、そこには被せない
        if (_probe != null)
          IgnorePointer(
            child: Align(
              // カメラ映像は操作パネルの反対側へ 0.3 寄せてある。一覧は縦に長いので、
              // 同じ 0.3 だと上端がコードの上の隅に掛かる (実機で確認)。寄せを半分にして
              // 映像の中ほどに置く
              alignment: _panelAtTop
                  ? const Alignment(0, 0.12)
                  : const Alignment(0, -0.12),
              child: FractionallySizedBox(
                widthFactor: 0.74,
                child: ProbePanel(probe: _probe!, footer: _alignPanel()),
              ),
            ),
          ),
        if (_restoring) const BusyOverlay('復元中…'),
        if (_acquiring) const BusyOverlay('位置を検出中…'),
        Positioned(
          left: 0,
          right: 0,
          top: _panelAtTop ? 0 : null,
          bottom: _panelAtTop ? null : 0,
          child: _controlPanel(total),
        ),
      ],
    );
  }

  /// ガイド枠の縦横比。選択中の格子のセル数から求める (自動のときは既定格子)。
  double _guideAspect() {
    final g = _forcedGrid == kGridAuto
        ? kPresets[kDefaultPresetIndex].grid
        : _forcedGrid;
    final p = g.split('x');
    final c = vcodeLayoutCells(gridW: int.parse(p[0]), gridH: int.parse(p[1]));
    return c[0] == 0 ? 0.92 : c[1] / c[0];
  }

  /// 構図合わせ用の読み上げ (充填率・余白・px/セル)。
  Widget _alignPanel() =>
      AlignPanel(code: _det, sensorOrientation: _sensorOrientation);

  /// 受信中の操作パネル。カメラ映像の上に重なるので、画面の端へ向かって濃くなる
  /// 黒のグラデーションを敷き、文字は読めるが映像側の端は透ける形にする。
  /// 中身が伸びても映像を潰しきらないよう高さを画面の 6 割で頭打ちにしてスクロールさせる。
  Widget _controlPanel(int? total) {
    final cov = _cov;
    // 端 (画面の外側) を濃く、内側 (映像側) を透明にする
    final edge = _panelAtTop ? Alignment.topCenter : Alignment.bottomCenter;
    final inner = _panelAtTop ? Alignment.bottomCenter : Alignment.topCenter;
    return Container(
      decoration: BoxDecoration(
        gradient: LinearGradient(
          begin: inner,
          end: edge,
          colors: const [
            Color(0x00000000),
            Color(0x59000000),
            Color(0xA6000000),
          ],
          stops: const [0.0, 0.30, 1.0],
        ),
      ),
      child: SafeArea(
        top: _panelAtTop,
        bottom: !_panelAtTop,
        child: ConstrainedBox(
          constraints: BoxConstraints(
            maxHeight: MediaQuery.of(context).size.height * 0.6,
          ),
          child: SingleChildScrollView(
            padding: _panelAtTop
                ? const EdgeInsets.fromLTRB(8, 8, 8, 28)
                : const EdgeInsets.fromLTRB(8, 28, 8, 8),
            // ボタンも進捗も透かす (Opacity は当たり判定を変えないので押せる)
            child: Opacity(
              opacity: 0.82,
              child: Column(
                mainAxisSize: MainAxisSize.min,
                children: [
                  Row(
                    children: [
                      Expanded(
                        child: FilledButton.icon(
                          onPressed: _acquiring ? null : () => _startAcquire(),
                          icon: Icon(
                            _seeded ? Icons.refresh : Icons.center_focus_strong,
                          ),
                          label: Text(_seeded ? '位置を再検出' : '位置を検出'),
                        ),
                      ),
                      const SizedBox(width: 8),
                      // 受信中でもやり直せるようにする。集めたパケットを捨てて最初から始める。
                      // 途中で条件を変えたときや、間違ったデータを掴んだときに要る。
                      FilledButton.tonalIcon(
                        onPressed: _restartReceive,
                        icon: const Icon(Icons.restart_alt),
                        label: const Text('リセット'),
                      ),
                      IconButton(
                        tooltip: _panelExpanded ? '詳細を畳む' : '詳細を開く (カバレッジ・設定)',
                        color: Colors.white,
                        icon: Icon(
                          _panelExpanded
                              ? Icons.expand_more
                              : Icons.expand_less,
                        ),
                        onPressed: () =>
                            setState(() => _panelExpanded = !_panelExpanded),
                      ),
                      IconButton(
                        tooltip: _panelAtTop ? '操作パネルを下へ' : '操作パネルを上へ',
                        color: Colors.white,
                        icon: Icon(
                          _panelAtTop
                              ? Icons.vertical_align_bottom
                              : Icons.vertical_align_top,
                        ),
                        onPressed: () =>
                            setState(() => _panelAtTop = !_panelAtTop),
                      ),
                    ],
                  ),
                  const SizedBox(height: 6),
                  // 畳んでいる間は進捗を細いバーだけで示す (格子は映像の下 1/4 を隠す)
                  if (total != null && !_panelExpanded)
                    Padding(
                      padding: const EdgeInsets.only(bottom: 6),
                      child: LinearProgressIndicator(
                        value: (cov!.useful / total).clamp(0.0, 1.0),
                        minHeight: 6,
                        backgroundColor: const Color(0x55FFFFFF),
                      ),
                    ),
                  // 受信データのカバレッジ格子: ESI ごとのマスを、受信済み=緑(source)/水色(repair)、
                  // 未受信=灰で塗る。埋まらない穴が「取れていないフレームのデータ」= 未完了の原因。
                  if (cov != null && _panelExpanded) ...[
                    CoverageGrid(cov),
                    const SizedBox(height: 4),
                  ],
                  // 一度も検出できていないときだけ、切り分けに要る実測値を出す。
                  // 回転はスキャナが rot と rot+180 しか試さないため端末差が出やすく、
                  // 輝度レンジは iOS の video range (16〜235) を見分けるために要る。
                  if (_stats.framesDetected == 0 && _stats.framesSeen > 10)
                    Padding(
                      padding: const EdgeInsets.only(bottom: 4),
                      child: Text(
                        '未検出の診断: rot=$_lastRot · ${_lastImgW}x$_lastImgH '
                        '(stride $_lastStride) · 輝度 $_lumaMin–$_lumaMax'
                        '${_lumaMax <= 240 && _lumaMin >= 10 ? " (video range?)" : ""}'
                        '${_lastError != null ? "\n$_lastError" : ""}',
                        style: TextStyle(
                          fontSize: 11,
                          color: Theme.of(context).colorScheme.error,
                        ),
                      ),
                    ),
                  if (_panelExpanded) _measureSettings(),
                  Text(
                    '検出 ${_stats.framesDetected}/${_stats.framesSeen} · '
                    // distinct (重複除く) が必要数 K に届くと復元される。投入は重複込みの累計。
                    '受信 ${cov?.useful ?? 0}${total != null ? "/$total 必要" : ""} '
                    '(投入 ${_stats.packetsAdded} 重複込) · scan ${_stats.lastScanMs}ms'
                    '${_seeded ? " · [位置固定]" : ""}'
                    '${_autoAcquireCount > 0 ? " · 自動取得 $_autoAcquireCount 回" : ""}'
                    '${_stats.integrityFails > 0 ? " · 整合性エラー ${_stats.integrityFails}" : ""}',
                    style: const TextStyle(fontSize: 12, color: Colors.white),
                  ),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }

  /// 受信完了後の画面。カメラは止まっているので通常のスクロール表示にする。
  /// アプリ全体が extendBody (映像を全画面にするため) なので、この画面は自前で
  /// SafeArea に収め、下に浮いているナビバーの高さぶん余白を取る。
  Widget _resultView() {
    return SafeArea(
      child: Center(
        child: SingleChildScrollView(
          padding: const EdgeInsets.fromLTRB(16, 16, 16, 96),
          child: Column(
            mainAxisAlignment: MainAxisAlignment.center,
            children: [
              const Icon(Icons.check_circle, size: 64, color: Colors.green),
              const SizedBox(height: 12),
              Text(_status, style: Theme.of(context).textTheme.titleMedium),
              const SizedBox(height: 12),
              // 受け取ったものをその場で確認できるようにする
              // (画像・テキストは内蔵表示、それ以外は「アプリで開く」へ)
              ReceivedPreview(
                bytes: _payload!,
                mime: _savedItem?.type ?? '',
                name: _savedItem?.name ?? '受信データ',
              ),
              const SizedBox(height: 12),
              // 詳細の数字は普段は見ないので畳んでおく (計測時だけ開く)
              ExpansionTile(
                title: const Text('詳細な統計', style: TextStyle(fontSize: 13)),
                tilePadding: const EdgeInsets.symmetric(horizontal: 8),
                childrenPadding: const EdgeInsets.only(bottom: 8),
                children: [StatsTable(_statsRows())],
              ),
              const SizedBox(height: 12),
              Wrap(
                alignment: WrapAlignment.center,
                spacing: 12,
                runSpacing: 8,
                children: [
                  // テキストなら保存せずその場でコピーできる (短文をわざわざ保存させない)
                  if ((_savedItem?.type ?? '').startsWith('text/'))
                    FilledButton.icon(
                      onPressed: () {
                        Clipboard.setData(
                          ClipboardData(
                            text: utf8.decode(_payload!, allowMalformed: true),
                          ),
                        );
                        ScaffoldMessenger.of(context).showSnackBar(
                          const SnackBar(content: Text('テキストをコピーしました')),
                        );
                      },
                      icon: const Icon(Icons.copy),
                      label: const Text('テキストをコピー'),
                    ),
                  if (_savedItem != null)
                    FilledButton.icon(
                      onPressed: () => _saveToFile(_savedItem!),
                      icon: const Icon(Icons.save_alt),
                      label: const Text('端末に保存'),
                    ),
                  if (_savedItem != null)
                    FilledButton.tonalIcon(
                      onPressed: () => shareReceived(_savedItem!),
                      icon: const Icon(Icons.share),
                      label: const Text('共有'),
                    ),
                  // 条件を振って比べるとき、統計を手で書き写すと必ず抜けるので
                  // タブ区切りで丸ごとコピーできるようにする
                  FilledButton.tonalIcon(
                    onPressed: _copyStats,
                    icon: const Icon(Icons.copy_all),
                    label: const Text('統計をコピー'),
                  ),
                  // 端末に保存する前でも中身を確認できるようにする
                  // (動画・PDF・Office など内蔵表示できない形式向け)
                  FilledButton.tonalIcon(
                    onPressed: () => openBytesWithDefaultApp(
                      context,
                      _payload!,
                      _savedItem?.name ?? 'received.bin',
                    ),
                    icon: const Icon(Icons.open_in_new),
                    label: const Text('アプリで開く'),
                  ),
                  FilledButton(onPressed: _reset, child: const Text('もう一度受信')),
                ],
              ),
            ],
          ),
        ),
      ),
    );
  }
}
