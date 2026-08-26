import 'dart:async';
import 'dart:convert';
import 'dart:typed_data';
import 'dart:ui' as ui;
import 'package:camera/camera.dart';
import 'package:flutter/material.dart';
import 'package:wakelock_plus/wakelock_plus.dart';
import 'src/rust/api/vcode.dart';
import 'vcode_view.dart';

/// 校正モード: 送信側が「ゆるい→きつい」レベルのテストフレームを表示し、
/// 受信側が「どの密度まで検出できるか」を確認する (オフラインの人アシスト校正)。
/// 検出できた最も密なレベルが、その環境で使える上限の目安になる。


class CalibrationScreen extends StatefulWidget {
  const CalibrationScreen({super.key});
  @override
  State<CalibrationScreen> createState() => _CalibrationScreenState();
}

class _CalibrationScreenState extends State<CalibrationScreen> {
  bool _send = true; // true=送信(表示), false=受信(確認)

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: const Text('校正'),
        bottom: PreferredSize(
          preferredSize: const Size.fromHeight(56),
          child: Padding(
            padding: const EdgeInsets.only(bottom: 8, left: 8, right: 8),
            child: SegmentedButton<bool>(
              segments: const [
                ButtonSegment(value: true, label: Text('送信(表示)'), icon: Icon(Icons.grid_on)),
                ButtonSegment(
                    value: false, label: Text('受信(確認)'), icon: Icon(Icons.center_focus_strong)),
              ],
              selected: {_send},
              onSelectionChanged: (s) => setState(() => _send = s.first),
            ),
          ),
        ),
      ),
      body: SafeArea(
          top: false, child: _send ? const _VCalSend() : const _VCalReceive()),
    );
  }
}

// ============ レベル定義 ============
class VCalLevel {
  final String label;
  final int gw;
  final int gh;
  const VCalLevel(this.label, this.gw, this.gh);
  int get blocks => gw * gh;
}

// vcode スキャナの CANDIDATES は 5×4 / 7×6 のみ検出できるため、校正もこの2択に絞る
// (他の格子はデコーダが認識できず「読めない」と誤判定されてしまう)。
const vCalLevels = <VCalLevel>[
  VCalLevel('Lv1  5×4 標準', 5, 4),
  VCalLevel('Lv2  7×6 高密度', 7, 6),
];

// ============ 送信 (テストフレーム表示) ============
class _VCalSend extends StatefulWidget {
  const _VCalSend();
  @override
  State<_VCalSend> createState() => _VCalSendState();
}

class _VCalSendState extends State<_VCalSend> {
  int _lv = 0; // 既定は 5×4 (ゆるい方から)
  ui.Image? _image;
  final _payload = Uint8List.fromList(utf8.encode('VCAL-CALIBRATION-PATTERN'));

  @override
  void initState() {
    super.initState();
    _rebuild();
  }

  Future<void> _rebuild() async {
    final lv = vCalLevels[_lv];
    // 本番 V送信 の既定と同じ 2bit (4値) で描く。校正のコードの見た目/密度を本番に一致させる。
    final tx = VcodeTx(payload: _payload, extraRepair: 4, gridW: lv.gw, gridH: lv.gh, bitsPerCell: 2);
    final f = tx.frameGray(i: 0);
    final rgba = Uint8List(f.width * f.height * 4);
    for (var i = 0; i < f.width * f.height; i++) {
      final v = f.pixels[i];
      rgba[i * 4] = v;
      rgba[i * 4 + 1] = v;
      rgba[i * 4 + 2] = v;
      rgba[i * 4 + 3] = 255;
    }
    final done = Completer<ui.Image>();
    ui.decodeImageFromPixels(rgba, f.width, f.height, ui.PixelFormat.rgba8888, done.complete);
    final img = await done.future;
    if (mounted) setState(() => _image = img);
  }

  void _set(int lv) {
    setState(() => _lv = lv);
    _rebuild();
  }

  @override
  Widget build(BuildContext context) {
    final lv = vCalLevels[_lv];
    // 本番 vcode 送信 (vcode_send_screen) と同一レイアウト:
    // 白背景・padding16・Expanded で中央 contain。校正で合わせた位置/サイズが本番でも
    // 一致するように、コードの描き方を必ず揃える。レベル操作だけ下部のコンパクト行で追加。
    return Container(
      color: Colors.white,
      padding: const EdgeInsets.all(16),
      child: Column(
        children: [
          Expanded(
            child: Center(
              child: _image == null
                  ? const CircularProgressIndicator()
                  : AspectRatio(
                      aspectRatio: _image!.width / _image!.height,
                      child: RawImage(
                          image: _image, fit: BoxFit.contain, filterQuality: FilterQuality.none),
                    ),
            ),
          ),
          Row(
            mainAxisAlignment: MainAxisAlignment.center,
            children: [
              IconButton(
                onPressed: _lv > 0 ? () => _set(_lv - 1) : null,
                icon: const Icon(Icons.chevron_left, color: Colors.black),
              ),
              Text('${lv.label}   (${_lv + 1}/${vCalLevels.length})',
                  style: const TextStyle(color: Colors.black, fontSize: 15)),
              IconButton(
                onPressed: _lv < vCalLevels.length - 1 ? () => _set(_lv + 1) : null,
                icon: const Icon(Icons.chevron_right, color: Colors.black),
              ),
            ],
          ),
        ],
      ),
    );
  }
}

// ============ 受信 (どの密度まで検出できるか) ============
class _VCalReceive extends StatefulWidget {
  const _VCalReceive();
  @override
  State<_VCalReceive> createState() => _VCalReceiveState();
}

class _VCalReceiveState extends State<_VCalReceive> {
  CameraController? _cam;
  VcodeRx? _rx;
  bool _busy = false;
  bool _active = false;
  final Set<int> _readable = {};
  int _lastBlocksOk = 0;
  int _lastBlocksTotal = 0;

  @override
  void initState() {
    super.initState();
    _initCamera();
  }

  Future<void> _initCamera() async {
    // 直前まで受信タブがカメラを掴んでいる場合があるので、解放待ちで数回リトライ。
    for (var attempt = 0; attempt < 6; attempt++) {
      if (!mounted) return;
      CameraController? cam;
      try {
        final cams = await availableCameras();
        final back = cams.firstWhere((c) => c.lensDirection == CameraLensDirection.back,
            orElse: () => cams.first);
        // 本番受信と同じカメラ設定 (1080p + 60fps 要求) で検出性能を揃える。
        cam = CameraController(back, ResolutionPreset.veryHigh,
            enableAudio: false, fps: 60, imageFormatGroup: ImageFormatGroup.yuv420);
        await cam.initialize();
        if (!mounted) {
          await cam.dispose();
          return;
        }
        _rx = VcodeRx();
        await cam.startImageStream(_onFrame);
        await WakelockPlus.enable();
        setState(() {
          _cam = cam;
          _active = true;
        });
        return;
      } catch (_) {
        try {
          await cam?.dispose();
        } catch (_) {}
        if (attempt < 5) await Future.delayed(const Duration(milliseconds: 300));
      }
    }
  }

  int _levelFromBlocks(int blocks) {
    for (var i = 0; i < vCalLevels.length; i++) {
      if (vCalLevels[i].blocks == blocks) return i;
    }
    return -1;
  }

  Future<void> _onFrame(CameraImage img) async {
    if (_busy || !_active) return;
    _busy = true;
    try {
      final y = img.planes[0];
      final rx = _rx;
      if (rx == null) return;
      final report = await rx.scan(
        y: y.bytes,
        width: img.width,
        height: img.height,
        stride: y.bytesPerRow,
        rotationDeg: _cam?.description.sensorOrientation ?? 90,
        // 本番受信と同一のスキャン範囲。校正で✅なら本番でも同じ枠で読めることを保証する。
        guideFrac: kVcodeGuideFrac,
        debugDump: false,
      );
      if (!_active) return;
      if (report.detected && report.blocksTotal > 0) {
        _lastBlocksOk = report.blocksOk;
        _lastBlocksTotal = report.blocksTotal;
        if (report.blocksOk * 10 >= report.blocksTotal * 8) {
          final lv = _levelFromBlocks(report.blocksTotal);
          if (lv >= 0) _readable.add(lv);
        }
        if (mounted) setState(() {});
      }
    } catch (_) {
    } finally {
      _busy = false;
    }
  }

  @override
  void dispose() {
    _active = false;
    _cam?.dispose();
    WakelockPlus.disable();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final cam = _cam;
    final best = _readable.isEmpty ? -1 : _readable.reduce((a, b) => a > b ? a : b);
    return Column(
      children: [
        Expanded(
          child: Container(
            color: Colors.black,
            // 本番受信 (VcodeReceiveScreen) と同一の描画 + 緑ガイド枠。
            // 校正で見えている枠・位置がそのまま本番受信でも成立する。
            child: cam == null || !cam.value.isInitialized
                ? const Center(child: CircularProgressIndicator())
                : VcodeCameraView(cam),
          ),
        ),
        Container(
          padding: const EdgeInsets.all(12),
          color: Theme.of(context).colorScheme.surfaceContainerHighest,
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  Expanded(
                    child: Text(
                      best < 0
                          ? '送信側のテストフレームに向けてください'
                          : '✅ 読めた最密: ${vCalLevels[best].label}',
                      style: Theme.of(context).textTheme.titleMedium,
                    ),
                  ),
                  TextButton.icon(
                    onPressed: () => setState(() => _readable.clear()),
                    icon: const Icon(Icons.refresh),
                    label: const Text('リセット'),
                  ),
                ],
              ),
              if (_lastBlocksTotal > 0)
                Text('直近: $_lastBlocksOk / $_lastBlocksTotal ブロック検出',
                    style: Theme.of(context).textTheme.bodySmall),
              const SizedBox(height: 6),
              Wrap(
                spacing: 6,
                children: [
                  for (var i = 0; i < vCalLevels.length; i++)
                    Chip(
                      visualDensity: VisualDensity.compact,
                      avatar: Icon(
                        _readable.contains(i) ? Icons.check_circle : Icons.circle_outlined,
                        color: _readable.contains(i) ? Colors.green : Colors.grey,
                        size: 18,
                      ),
                      label: Text('Lv${i + 1}'),
                    ),
                ],
              ),
            ],
          ),
        ),
      ],
    );
  }
}
