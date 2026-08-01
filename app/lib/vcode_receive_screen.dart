import 'dart:async';
import 'dart:io';

import 'package:camera/camera.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:path_provider/path_provider.dart';
import 'package:wakelock_plus/wakelock_plus.dart';

import 'history_screen.dart' show shareReceived, saveReceivedToFile;
import 'history_store.dart';
import 'src/rust/api/fountain.dart';
import 'src/rust/api/vcode.dart';
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

/// 自動 acquire の再試行間隔 (フレーム)。acquire は 300 回超の探索を伴うので連発させない。
const kAutoAcquireCooldownFrames = 45;

/// 受信で選べるカメラ解像度。密な格子ほど px/セル が要るので上げる必要がある。
const kPresets = <(String, ResolutionPreset)>[
  ('1080p', ResolutionPreset.veryHigh),
  ('2160p', ResolutionPreset.ultraHigh),
  ('最大', ResolutionPreset.max),
];

class _VcodeReceiveScreenState extends State<VcodeReceiveScreen>
    with WidgetsBindingObserver {
  CameraController? _cam;

  /// 探索する格子 (kGridAuto か '9x8' 等)。送信側の格子が分かっているなら固定するほど速い
  String _forcedGrid = kGridAuto;

  /// カメラ解像度。9x8 以上は 1080p では px/セル が足りない
  ResolutionPreset _preset = ResolutionPreset.veryHigh;

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

  bool _busy = false;
  bool _active = false;
  bool _camBusy = false; // カメラ初期化/再初期化の多重実行ガード
  bool _acquireRequested = false; // 次フレームで acquire (位置検出) を実行する
  bool _acquiring = false; // acquire 実行中 (UI スピナー表示)
  bool _autoAcquire = true; // 検出できないとき自動で acquire を走らせる
  bool _acquireIsAuto = false; // 実行中の acquire が自動起動か (自動なら確認ダイアログを出さない)
  int _autoAcquireAt = 0; // 次に自動 acquire してよい _framesSeen (連発を防ぐクールダウン)
  int _autoAcquireCount = 0; // 自動 acquire の実行回数 (統計・UI 表示用)
  int _missStreak = 0; // 連続して検出できなかったフレーム数
  bool _seeded = false; // acquire 結果で受信位置を確定済み (中央ガイド枠に頼らず追従)
  List<double>? _detCorners; // acquire で検出した 4 隅 (回転後画像座標, 8 値) — ハイライト表示用
  int _detImgW = 0, _detImgH = 0, _detRot = 0; // 検出時の回転後画像寸法と回転 (表示座標への変換用)
  Timer? _watchdog; // プレビューが灰色 (フレーム途絶) になったら作り直す
  VcodeRx? _rx;

  FountainDecoder? _dec;
  int? _packetSize; // 最初の回収パケットから推定 (シリアライズ長 - 4)
  Uint8List? _payload;
  String? _savedPath;
  HistoryItem? _savedItem;

  // 統計
  int _camCallbacks = 0; // カメラが配信した全フレーム (busy スキップ含む)
  DateTime? _camStarted;
  int _framesSeen = 0;
  int _framesDetected = 0;
  int _framesTracked = 0;
  int _blocksOk = 0;
  int _packetsAdded = 0;
  // 受信できた ESI (Encoding Symbol ID) の集合。重複を除いた"実データ被覆"。
  // RaptorQ は distinct が必要数 K に届くと復元できる。カバレッジ格子と distinct 数の表示に使う。
  final Set<int> _seenEsi = {};
  int _integrityFails = 0; // エンドツーエンド CRC 不一致で受信をやり直した回数
  int _lastScanMs = 0;
  int _scanMsSum = 0;
  int _scanCount = 0;
  DateTime? _firstDetected;
  Duration? _elapsed;
  String _status = 'カメラ起動待ち';

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    if (widget.active) _initCamera();
    // フレームが一定時間途絶えたらカメラを作り直す (校正からの復帰レースや
    // 一時的なカメラ喪失で灰色のまま固まるのを自己修復する)。
    _watchdog =
        Timer.periodic(const Duration(seconds: 1), (_) => _checkStale());
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
            orElse: () => cams.first);
        cam = CameraController(
          back,
          // セル解像度が密度の上限を決める。7x6 (140 セル幅) は 1080p で足りるが、
          // 9x8 (180) / 11x10 (220) は 6px/セル に 1080/1320px 要るので 2160p 以上が要る。
          _preset,
          enableAudio: false,
          // 60fps 要求 (対応外の端末では無視される。実配信レートは統計で確認)
          fps: 60,
          imageFormatGroup: ImageFormatGroup.yuv420,
        );
        await cam.initialize();
        if (!mounted || !widget.active) {
          await cam.dispose();
          return;
        }
        _rx = VcodeRx();
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
    if (_busy || !_active || _payload != null) return;
    _busy = true;
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
      if (_framesDetected == 0 && _framesSeen % 30 == 0) {
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
      final wantDump = _framesDetected == 0 && _framesSeen > 0 && _framesSeen % 150 == 0;
      final rx = _rx;
      if (rx == null) return;
      // 位置検出 (acquire): 画面全体を sweep して実際の 4 隅を取得し、ポップアップで確認 →
      // seed でトラッキングの種にする。以降 scan() は最初からその位置にロックして始まる。
      if (_acquireRequested) {
        _acquireRequested = false;
        final rep = await rx.acquire(
          y: y.bytes,
          width: img.width,
          height: img.height,
          stride: y.bytesPerRow,
          rotationDeg: rotation,
        );
        if (!mounted || !_active) return;
        setState(() {
          _acquiring = false;
          if (rep.detected) {
            // 検出 4 隅をハイライト表示用に保持 (確認ダイアログの背後に見える)
            _detCorners = rep.corners.toList();
            _detImgW = rep.imgW;
            _detImgH = rep.imgH;
            _detRot = rep.rot;
          }
        });
        if (_acquireIsAuto) {
          // 自動起動: 確認を挟まずそのままロックする。位置がずれていても、
          // 検出が続かなければクールダウン後にまた自動取得が走って直る。
          _acquireIsAuto = false;
          if (rep.detected) {
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
      final report = await rx.scan(
        y: y.bytes,
        width: img.width,
        height: img.height,
        stride: y.bytesPerRow,
        rotationDeg: rotation,
        guideFrac: kVcodeGuideFrac,
        debugDump: wantDump,
      );
      sw.stop();
      if (!_active || _payload != null) return;
      if (report.debugGray != null) _saveDump(report);

      _framesSeen++;
      _lastScanMs = sw.elapsedMilliseconds;
      _scanMsSum += _lastScanMs;
      _scanCount++;
      if (report.detected) {
        _framesDetected++;
        if (report.tracked) _framesTracked++;
        _firstDetected ??= DateTime.now();
        _blocksOk += report.blocksOk;
        _dec ??= FountainDecoder(otiBytes: report.oti);
        if (_packetSize == null && report.packets.isNotEmpty) {
          _packetSize = report.packets.first.length - 4;
        }
        var done = false;
        for (final p in report.packets) {
          _packetsAdded++;
          if (p.length >= 4) {
            // RaptorQ payload ID = SBN(1 byte) + ESI(3 byte, big-endian)。
            // 単一ソースブロック前提 (SBN=0) で ESI をカバレッジ格子の座標に使う。
            final esi = (p[1] << 16) | (p[2] << 8) | p[3];
            _seenEsi.add(esi);
          }
          if (_dec!.addPacket(packet: p)) {
            done = true;
            break;
          }
        }
        debugPrint('[vcode-rx] seq=${report.frameSeq} '
            'blocks=${report.blocksOk}/${report.blocksTotal} '
            'pkts=$_packetsAdded scan=${_lastScanMs}ms '
            'tracked=${report.tracked} done=$done');
        if (done) {
          // エンドツーエンド CRC-32 検証。不一致 = ゴミパケットが RaptorQ を
          // 汚染して復元結果が破損 → デコーダを捨てて受信をやり直す
          final payload = vcodeUnwrapPayload(payload: _dec!.payload()!);
          if (payload == null) {
            _integrityFails++;
            debugPrint('[vcode-rx] 整合性エラー: 復元結果が破損 '
                '($_integrityFails 回目)。デコーダを再作成して受信続行');
            _dec = null;
            return;
          }
          _onComplete(payload);
          return;
        }
        _missStreak = 0;
        // 「今どこを読んでいるか」を毎フレーム更新する。追従中も枠が動くので、
        // ロックできているかが画面を見れば分かる。
        if (report.corners.length >= 8) {
          _detCorners = report.corners.toList();
          _detImgW = report.imgW;
          _detImgH = report.imgH;
          _detRot = report.rot;
        }
      } else {
        // 中央ガイド枠での探索が「連続して」失敗するなら、広域 sweep に切り替えて
        // 位置を取りに行く。acquire は重いが一度きりで、成功後はトラッキングに
        // 移るので定常コストは増えない (= 手で位置を合わせる必要がなくなる)。
        // 単発のフレーム落ちで走らせないよう、連続失敗数で判定する。
        _missStreak++;
        // 見失ったら枠を消す (古い位置を出したままにしない)
        if (_missStreak > kAutoAcquireMissFrames ~/ 2) _detCorners = null;
        if (_autoAcquire &&
            _missStreak >= kAutoAcquireMissFrames &&
            _framesSeen >= _autoAcquireAt) {
          _autoAcquireAt = _framesSeen + kAutoAcquireCooldownFrames;
          _missStreak = 0;
          _autoAcquireCount++;
          _startAcquire(auto: true);
        }
      }
      if (!report.detected && _framesSeen % 30 == 0) {
        _lastError = report.error;
        debugPrint('[vcode-rx] not detected (${report.error}) '
            'scan=${_lastScanMs}ms seen=$_framesSeen detected=$_framesDetected '
            'camFps=${_camFps.toStringAsFixed(1)}');
      }
      if (mounted && _framesSeen % 5 == 0) setState(() {});
    } finally {
      _busy = false;
    }
  }

  Future<void> _saveDump(VcodeScanReport report) async {
    try {
      final dir = await getApplicationDocumentsDirectory();
      final path = '${dir.path}/vcode_dump_${report.debugW}x${report.debugH}.gray';
      await File(path).writeAsBytes(report.debugGray!);
      debugPrint('[vcode-rx] DUMP saved: $path (err=${report.error})');
    } catch (e) {
      debugPrint('[vcode-rx] DUMP 保存失敗: $e');
    }
  }

  /// 先頭バイトからファイル種別を推定する (vcode はメタデータを運ばないため)
  (String, String) _sniffType(Uint8List b) {
    if (b.length > 3 && b[0] == 0xFF && b[1] == 0xD8) return ('jpg', 'image/jpeg');
    if (b.length > 7 && b[0] == 0x89 && b[1] == 0x50) return ('png', 'image/png');
    if (b.length > 11 && b[8] == 0x57 && b[9] == 0x45 && b[10] == 0x42 && b[11] == 0x50) {
      return ('webp', 'image/webp');
    }
    // ISO-BMFF (オフセット 4 に 'ftyp'): HEIC/AVIF (iOS 写真の既定形式)
    if (b.length > 11 && b[4] == 0x66 && b[5] == 0x74 && b[6] == 0x79 && b[7] == 0x70) {
      final brand = String.fromCharCodes(b.sublist(8, 12));
      if (const {'heic', 'heix', 'hevc', 'heim', 'heis', 'mif1', 'msf1'}.contains(brand)) {
        return ('heic', 'image/heic');
      }
      if (brand == 'avif' || brand == 'avis') return ('avif', 'image/avif');
    }
    if (b.length > 3 && b[0] == 0x25 && b[1] == 0x50 && b[2] == 0x44 && b[3] == 0x46) {
      return ('pdf', 'application/pdf');
    }
    if (b.length > 1 && b[0] == 0x50 && b[1] == 0x4B) return ('zip', 'application/zip');
    // 先頭 4KB の制御文字率でテキスト判定
    final probe = b.take(4096);
    final ctrl = probe.where((c) => c < 9 || (c > 13 && c < 32) || c == 127).length;
    if (probe.isNotEmpty && ctrl / probe.length < 0.02) {
      return ('txt', 'text/plain;charset=utf-8');
    }
    return ('bin', 'application/octet-stream');
  }

  Future<void> _onComplete(Uint8List rawPayload) async {
    // ファイル名/MIME ヘッダがあれば元の名前・種別で保存。無ければ従来どおり推測+タイムスタンプ名。
    final meta = vcodeUnwrapFile(buf: rawPayload);
    final Uint8List payload;
    final String name;
    final String mime;
    final ts = DateTime.now().toIso8601String().replaceAll(':', '-').substring(0, 19);
    if (meta != null) {
      payload = meta.data;
      final sniff = _sniffType(payload);
      name = meta.name.isNotEmpty ? meta.name : 'vcode_$ts.${sniff.$1}';
      mime = meta.mime.isNotEmpty ? meta.mime : sniff.$2;
    } else {
      payload = rawPayload;
      final sniff = _sniffType(payload);
      name = 'vcode_$ts.${sniff.$1}';
      mime = sniff.$2;
    }
    final elapsed = _firstDetected == null
        ? Duration.zero
        : DateTime.now().difference(_firstDetected!);
    setState(() {
      _payload = payload;
      _elapsed = elapsed;
      _status = '受信完了';
    });
    final ms = elapsed.inMilliseconds;
    final kbps = ms > 0 ? (payload.length / 1024) / (ms / 1000) : 0.0;
    final note = '${(ms / 1000).toStringAsFixed(2)}s'
        ' · ${kbps.toStringAsFixed(1)}KB/s'
        ' · cam${_camFps.toStringAsFixed(0)}fps'
        ' · 検出$_framesDetected/$_framesSeen(追従$_framesTracked)'
        ' · blk$_blocksOk · pkt$_packetsAdded'
        ' · scan${_scanCount > 0 ? (_scanMsSum / _scanCount).round() : 0}ms';
    debugPrint('[vcode-rx] COMPLETE: $name ${payload.length} bytes in ${ms}ms, $note');
    // 履歴に保存 (QR 受信と同じ HistoryStore)。名前/種別はヘッダ優先、無ければ内容推定。
    try {
      final slot = HistoryStore.instance.reserveReceivedPath();
      await File(slot.path).writeAsBytes(payload);
      await HistoryStore.instance
          .registerReceived(slot.id, name, mime, payload.length, note: note);
      setState(() {
        _savedPath = '履歴に保存: $name';
        _savedItem = HistoryStore.instance.received.first;
      });
    } catch (e) {
      debugPrint('[vcode-rx] 履歴保存失敗: $e');
    }
    await _stopCamera();
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

  /// カメラ解像度を変えて開き直す (プレビュー中でも即反映)
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
      _packetSize = null;
      _payload = null;
      _savedPath = null;
      _savedItem = null;
      _framesSeen = 0;
      _framesDetected = 0;
      _framesTracked = 0;
      _blocksOk = 0;
      _packetsAdded = 0;
      _seenEsi.clear();
      _integrityFails = 0;
      _scanMsSum = 0;
      _scanCount = 0;
      _firstDetected = null;
      _elapsed = null;
      _acquireRequested = false;
      _acquiring = false;
      _acquireIsAuto = false;
      _autoAcquireAt = 0;
      _autoAcquireCount = 0;
      _missStreak = 0;
      _seeded = false;
      _detCorners = null;
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
      if (!auto) _detCorners = null; // 手動時は前回のハイライトを消す
    });
  }

  /// acquire 結果を中央ポップアップで確認。確定なら seed して受信継続、やり直しなら再取得。
  Future<void> _showAcquireDialog(VcodeAcquireReport rep, VcodeRx rx) async {
    if (!mounted) return;
    if (!rep.detected) {
      await showDialog<void>(
        context: context,
        builder: (ctx) => AlertDialog(
          title: const Text('位置を検出できませんでした'),
          content: const Text(
              'コードが画面に写っているか、ピントが合っているか確認して、もう一度お試しください。'),
          actions: [
            TextButton(onPressed: () => Navigator.pop(ctx), child: const Text('閉じる')),
          ],
        ),
      );
      return;
    }
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: const Text('位置を検出しました'),
        content: Text('格子 ${rep.gridW}×${rep.gridH} · 直近 ${rep.blocksOk}/${rep.blocksTotal} ブロック\n'
            'この位置に固定したまま受信を開始しますか?'),
        actions: [
          TextButton(
              onPressed: () => Navigator.pop(ctx, false), child: const Text('やり直す')),
          FilledButton(
              onPressed: () => Navigator.pop(ctx, true),
              child: const Text('この位置で受信開始')),
        ],
      ),
    );
    if (!mounted) return;
    if (confirmed == true) {
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
    _watchdog?.cancel();
    WidgetsBinding.instance.removeObserver(this);
    _stopCamera();
    super.dispose();
  }

  /// 受信ファイルを端末の任意の場所へ保存 (SAF ダイアログ)。結果をスナックバーで通知。
  Future<void> _saveToFile(HistoryItem item) async {
    final ok = await saveReceivedToFile(item);
    if (!mounted) return;
    ScaffoldMessenger.of(context).showSnackBar(
      SnackBar(content: Text(ok ? '端末に保存しました' : '保存をキャンセルしました')),
    );
  }

  /// 受信データのカバレッジ格子 (ESI ごとの被覆)。高さは固定で、セル数が増えても
  /// マスを小さくして一定サイズ内に収める (カメラ表示位置がずれないように)。
  Widget _coverageGrid(int k) {
    return SizedBox(
      height: 72,
      width: double.infinity,
      child: CustomPaint(painter: _CoverageGridPainter(seen: _seenEsi, k: k)),
    );
  }

  /// 受信完了時の統計テーブル
  Widget _statsTable() {
    final p = _payload!;
    final ms = _elapsed?.inMilliseconds ?? 0;
    final kbps = ms > 0 ? (p.length / 1024) / (ms / 1000) : 0.0;
    // 条件を一緒に残す: 同じ数字でも解像度と格子が違えば比較にならない。
    final preview = _lastPreviewSize;
    final rows = <(String, String)>[
      ('サイズ', '${p.length} B'),
      ('所要時間 (初検出→完了)', ms >= 1000 ? '${(ms / 1000).toStringAsFixed(2)} 秒' : '$ms ms'),
      ('実効スループット', '${kbps.toStringAsFixed(1)} KB/s'),
      ('カメラ解像度', preview == null
          ? '-'
          : '${preview.width.round()}×${preview.height.round()}'),
      ('格子指定', _forcedGrid == kGridAuto ? '自動 (候補総当たり)' : _forcedGrid),
      ('カメラフレーム数', '$_framesSeen (検出 $_framesDetected / 追従 $_framesTracked)'),
      ('カメラ実効fps', _camFps.toStringAsFixed(1)),
      ('回収ブロック', '$_blocksOk (部分回収込み)'),
      ('投入パケット', '$_packetsAdded'),
      if (_integrityFails > 0) ('整合性エラー再試行', '$_integrityFails 回'),
      ('平均スキャン時間', _scanCount > 0 ? '${(_scanMsSum / _scanCount).round()} ms' : '-'),
    ];
    return Table(
      columnWidths: const {0: IntrinsicColumnWidth(), 1: FlexColumnWidth()},
      defaultVerticalAlignment: TableCellVerticalAlignment.middle,
      children: [
        for (final (label, value) in rows)
          TableRow(children: [
            Padding(
              padding: const EdgeInsets.symmetric(vertical: 3, horizontal: 8),
              child: Text(label,
                  style: TextStyle(
                      fontSize: 12, color: Theme.of(context).colorScheme.onSurfaceVariant)),
            ),
            Padding(
              padding: const EdgeInsets.symmetric(vertical: 3, horizontal: 8),
              child: Text(value,
                  style: const TextStyle(fontSize: 13, fontWeight: FontWeight.w600)),
            ),
          ]),
      ],
    );
  }

  /// プレビュー上の状態バッジ。「探索中 / 位置検出中 / 追従中」を明示する。
  /// 内部でロックしていても画面に出ないと分からないため、状態を必ず可視化する。
  Widget _statusBadge() {
    final (String label, Color color, IconData icon) = switch (this) {
      _ when _acquireRequested || _acquiring => ('位置を検出中…', Colors.amber, Icons.travel_explore),
      _ when _framesDetected > 0 && _missStreak == 0 =>
        (_seeded ? '追従中 (位置固定)' : '追従中', Colors.cyanAccent, Icons.center_focus_strong),
      _ when _autoAcquire => ('位置を探しています…', Colors.orangeAccent, Icons.search),
      _ => ('枠にコードを合わせてください', Colors.orangeAccent, Icons.crop_free),
    };
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
      decoration: BoxDecoration(
        color: Colors.black.withValues(alpha: 0.55),
        borderRadius: BorderRadius.circular(16),
        border: Border.all(color: color, width: 1.5),
      ),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(icon, size: 16, color: color),
          const SizedBox(width: 6),
          Text(label, style: TextStyle(color: color, fontSize: 13, fontWeight: FontWeight.w600)),
        ],
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
              style: TextStyle(fontSize: 11, color: Theme.of(context).hintColor)),
          value: _autoAcquire,
          onChanged: (v) => setState(() {
            _autoAcquire = v;
            _missStreak = 0;
          }),
        ),
        Row(
          children: [
            Text('解像度', style: small),
            const SizedBox(width: 12),
            SegmentedButton<ResolutionPreset>(
              segments: [
                for (final (label, p) in kPresets)
                  ButtonSegment(value: p, label: Text(label)),
              ],
              selected: {_preset},
              showSelectedIcon: false,
              onSelectionChanged: (s) => _changePreset(s.first),
            ),
          ],
        ),
        const SizedBox(height: 6),
        Row(
          children: [
            Text('格子', style: small),
            const SizedBox(width: 12),
            Expanded(
              child: SingleChildScrollView(
                scrollDirection: Axis.horizontal,
                child: SegmentedButton<String>(
                  segments: const [
                    ButtonSegment(value: kGridAuto, label: Text('自動')),
                    ButtonSegment(value: '5x4', label: Text('5x4')),
                    ButtonSegment(value: '7x6', label: Text('7x6')),
                    ButtonSegment(value: '9x8', label: Text('9x8')),
                    ButtonSegment(value: '11x10', label: Text('11x10')),
                  ],
                  selected: {_forcedGrid},
                  showSelectedIcon: false,
                  onSelectionChanged: (s) => setState(() {
                    _forcedGrid = s.first;
                    _applyForcedGrid();
                  }),
                ),
              ),
            ),
          ],
        ),
      ],
    );
  }

  @override
  Widget build(BuildContext context) {
    final cam = _cam;
    final ps = _packetSize ?? 42;
    final total = _dec == null
        ? null
        : (_dec!.payloadSize().toInt() + ps - 1) ~/ ps; // 必要 source パケット数

    return Column(
      children: [
        Expanded(
          child: _payload != null
              ? Center(
                  child: SingleChildScrollView(
                    padding: const EdgeInsets.all(16),
                    child: Column(
                      mainAxisAlignment: MainAxisAlignment.center,
                      children: [
                        const Icon(Icons.check_circle, size: 64, color: Colors.green),
                        const SizedBox(height: 12),
                        Text(_status, style: Theme.of(context).textTheme.titleMedium),
                        const SizedBox(height: 12),
                        _statsTable(),
                        if (_savedPath != null)
                          Padding(
                            padding: const EdgeInsets.all(8),
                            child: Text(_savedPath!,
                                style: const TextStyle(fontSize: 11)),
                          ),
                        const SizedBox(height: 12),
                        Wrap(
                          alignment: WrapAlignment.center,
                          spacing: 12,
                          runSpacing: 8,
                          children: [
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
                            FilledButton(
                                onPressed: _reset, child: const Text('もう一度受信')),
                          ],
                        ),
                      ],
                    ),
                  ),
                )
              : cam == null || !cam.value.isInitialized
                  ? Center(child: Text(_status))
                  : Stack(
                      fit: StackFit.expand,
                      children: [
                        VcodeCameraView(cam),
                        // 検出領域のハイライト (シアン)。緑のガイド枠と区別できる色。
                        if (_detCorners != null)
                          CustomPaint(
                            painter: _DetectedQuadPainter(
                              corners: _detCorners!,
                              imgW: _detImgW,
                              imgH: _detImgH,
                              delta: ((_cam?.description.sensorOrientation ?? 90) -
                                      _detRot +
                                      360) %
                                  360,
                              ar: cam.value.aspectRatio,
                            ),
                          ),
                        Positioned(
                          top: 10,
                          left: 0,
                          right: 0,
                          child: Center(child: _statusBadge()),
                        ),
                        if (_acquiring)
                          Container(
                            color: Colors.black54,
                            child: const Center(
                              child: Column(
                                mainAxisSize: MainAxisSize.min,
                                children: [
                                  CircularProgressIndicator(),
                                  SizedBox(height: 12),
                                  Text('位置を検出中…',
                                      style: TextStyle(color: Colors.white, fontSize: 15)),
                                ],
                              ),
                            ),
                          ),
                      ],
                    ),
        ),
        Padding(
          padding: const EdgeInsets.all(8),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              if (_payload == null && cam != null && cam.value.isInitialized) ...[
                SizedBox(
                  width: double.infinity,
                  child: FilledButton.icon(
                    onPressed: _acquiring ? null : () => _startAcquire(),
                    icon: Icon(_seeded ? Icons.refresh : Icons.center_focus_strong),
                    label: Text(_seeded
                        ? '位置を再検出'
                        : _autoAcquire
                            ? '今すぐ位置を検出 (自動取得は有効)'
                            : 'うまく取得できない時: 位置を検出'),
                  ),
                ),
                const SizedBox(height: 6),
              ],
              // 受信データのカバレッジ格子: ESI ごとのマスを、受信済み=緑(source)/水色(repair)、
              // 未受信=灰で塗る。埋まらない穴が「取れていないフレームのデータ」= 未完了の原因。
              if (_payload == null && total != null) ...[
                _coverageGrid(total),
                const SizedBox(height: 4),
              ],
              // 一度も検出できていないときだけ、切り分けに要る実測値を出す。
              // 回転はスキャナが rot と rot+180 しか試さないため端末差が出やすく、
              // 輝度レンジは iOS の video range (16〜235) を見分けるために要る。
              if (_payload == null && _framesDetected == 0 && _framesSeen > 10)
                Padding(
                  padding: const EdgeInsets.only(bottom: 4),
                  child: Text(
                    '未検出の診断: rot=$_lastRot · ${_lastImgW}x$_lastImgH '
                    '(stride $_lastStride) · 輝度 $_lumaMin–$_lumaMax'
                    '${_lumaMax <= 240 && _lumaMin >= 10 ? " (video range?)" : ""}'
                    '${_lastError != null ? "\n$_lastError" : ""}',
                    style: TextStyle(
                        fontSize: 11, color: Theme.of(context).colorScheme.error),
                  ),
                ),
              if (_payload == null) _measureSettings(),
              Text(
                _payload != null
                    ? _status
                    : '検出 $_framesDetected/$_framesSeen · '
                        // distinct (重複除く) が必要数 K に届くと復元される。投入は重複込みの累計。
                        '受信 ${_seenEsi.length}${total != null ? "/$total 必要" : ""} '
                        '(投入 $_packetsAdded 重複込) · scan ${_lastScanMs}ms'
                        '${_seeded ? " · [位置固定]" : ""}'
                        '${_autoAcquireCount > 0 ? " · 自動取得 $_autoAcquireCount 回" : ""}'
                        '${_integrityFails > 0 ? " · 整合性エラー $_integrityFails" : ""}',
                style: const TextStyle(fontSize: 12),
              ),
            ],
          ),
        ),
      ],
    );
  }
}

/// acquire で検出した 4 隅 (回転後画像座標) を、プレビュー表示座標へ写してハイライトする。
/// 回転差 delta = (sensorOrientation - 検出時 rot) を吸収してからプレビュー矩形に一様スケールする。
/// VcodeCameraView と同じ配置 (幅いっぱい・高さ = 幅×aspectRatio・上下中央) を前提にする。
class _DetectedQuadPainter extends CustomPainter {
  _DetectedQuadPainter({
    required this.corners,
    required this.imgW,
    required this.imgH,
    required this.delta,
    required this.ar,
  });

  /// 回転後画像座標の 4 隅 [tl.x, tl.y, tr.x, tr.y, br.x, br.y, bl.x, bl.y]
  final List<double> corners;
  final int imgW;
  final int imgH;

  /// (sensorOrientation - 検出時 rot + 360) % 360。プレビューは sensorOrientation 空間。
  final int delta;

  /// controller.value.aspectRatio (VcodeCameraView の高さ計算と一致させる)
  final double ar;

  Offset _map(double x, double y, Size size) {
    // 1) 回転後画像空間 (imgW×imgH) → プレビュー画像空間 (delta 回転)
    double ix, iy;
    int pwImg, phImg;
    if (delta == 90) {
      ix = imgH - 1 - y;
      iy = x;
      pwImg = imgH;
      phImg = imgW;
    } else if (delta == 180) {
      ix = imgW - 1 - x;
      iy = imgH - 1 - y;
      pwImg = imgW;
      phImg = imgH;
    } else if (delta == 270) {
      ix = y;
      iy = imgW - 1 - x;
      pwImg = imgH;
      phImg = imgW;
    } else {
      ix = x;
      iy = y;
      pwImg = imgW;
      phImg = imgH;
    }
    // 2) プレビュー画像空間 → ウィジェット座標 (幅いっぱい・高さ=幅×ar・上下中央)
    final pw = size.width;
    final ph = size.width * ar;
    final off = (size.height - ph) / 2;
    return Offset(ix / pwImg * pw, iy / phImg * ph + off);
  }

  @override
  void paint(Canvas canvas, Size size) {
    if (corners.length < 8 || imgW == 0 || imgH == 0) return;
    final tl = _map(corners[0], corners[1], size);
    final tr = _map(corners[2], corners[3], size);
    final br = _map(corners[4], corners[5], size);
    final bl = _map(corners[6], corners[7], size);
    final path = Path()
      ..moveTo(tl.dx, tl.dy)
      ..lineTo(tr.dx, tr.dy)
      ..lineTo(br.dx, br.dy)
      ..lineTo(bl.dx, bl.dy)
      ..close();
    canvas.drawPath(path, Paint()..color = const Color(0x3300E5FF));
    canvas.drawPath(
        path,
        Paint()
          ..color = const Color(0xFF00E5FF)
          ..style = PaintingStyle.stroke
          ..strokeWidth = 3);
    final dot = Paint()..color = const Color(0xFF00E5FF);
    for (final p in [tl, tr, br, bl]) {
      canvas.drawCircle(p, 6, dot);
    }
  }

  @override
  bool shouldRepaint(covariant _DetectedQuadPainter old) =>
      old.corners != corners ||
      old.delta != delta ||
      old.imgW != imgW ||
      old.imgH != imgH ||
      old.ar != ar;
}

/// 受信データのカバレッジ格子。ESI をマスに割り当て、受信済み=緑(source)/水色(repair)、
/// 未受信=灰で塗る。埋まらない穴 = まだ取れていないパケット (= 復元が完了しない原因) が
/// 一目でわかる。RaptorQ は distinct が必要数 K に届くと復元できる。
class _CoverageGridPainter extends CustomPainter {
  _CoverageGridPainter({required this.seen, required this.k});
  final Set<int> seen;
  final int k; // 必要 source パケット数 (ESI < k = source, >= k = repair)

  @override
  void paint(Canvas canvas, Size size) {
    var cap = k;
    for (final e in seen) {
      if (e + 1 > cap) cap = e + 1;
    }
    if (cap <= 0 || size.width <= 0 || size.height <= 0) return;
    // 固定領域 (size) に cap マスを正方セルで収める。増えるほどセルを小さくする。
    var s = 8.0; // 希望セルサイズ
    var cols = (size.width / s).floor();
    if (cols < 1) cols = 1;
    var rows = (cap / cols).ceil();
    while (rows * s > size.height && s > 1.0) {
      s -= 0.5;
      cols = (size.width / s).floor();
      if (cols < 1) cols = 1;
      rows = (cap / cols).ceil();
    }
    final gap = s > 3 ? 1.0 : 0.0;
    final unseen = Paint()..color = const Color(0xFF37474F);
    final src = Paint()..color = const Color(0xFF4CAF50);
    final rep = Paint()..color = const Color(0xFF29B6F6);
    for (var i = 0; i < cap; i++) {
      final r = i ~/ cols, c = i % cols;
      canvas.drawRect(
        Rect.fromLTWH(c * s, r * s, s - gap, s - gap),
        seen.contains(i) ? (i < k ? src : rep) : unseen,
      );
    }
  }

  @override
  bool shouldRepaint(covariant _CoverageGridPainter old) =>
      old.seen.length != seen.length || old.k != k;
}
