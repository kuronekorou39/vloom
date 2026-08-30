import 'dart:async';
import 'dart:convert';
import 'dart:ui' as ui;

import 'package:file_selector/file_selector.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:screen_brightness/screen_brightness.dart';
import 'package:wakelock_plus/wakelock_plus.dart';

import 'history_store.dart';
import 'preset.dart';
import 'test_payload.dart';
import 'ui_common.dart';
import 'src/rust/api/vcode.dart';

/// vcode (独自フォーマット) 送信画面。研究用: 単一ブロック・生バイトを
/// アニメーション vcode で送出する。
class VcodeSendScreen extends StatefulWidget {
  const VcodeSendScreen({super.key});
  @override
  State<VcodeSendScreen> createState() => _VcodeSendScreenState();
}

/// 1 フレームは最低 2 リフレッシュ周期表示しないとキャプチャが遷移を捉えるため、
/// 60Hz 画面で安全に出せる fps の上限。120Hz 画面ならこの倍まで出せる。
const kRefreshSafeFps = 30;

class _VcodeSendScreenState extends State<VcodeSendScreen> {
  final _textCtrl = TextEditingController();
  String? _pickedPath;
  String? _pickedName;
  String? _pickedMime; // 選択ファイルの MIME (受信側で元種別を復元する用)
  int _pickedSize = 0;

  String? _testAsset; // 計測用テスト画像のアセットパス (null = ファイル/テキストを送る)
  // 選択中のプリセット (-1 = 詳細設定を直接触ったカスタム)
  int _presetIndex = kDefaultPresetIndex;
  double _repairRate = 0.5; // リペアパケット比率 (source 比)
  String _grid = kPresets[kDefaultPresetIndex].grid;
  int _bpc = kPresets[kDefaultPresetIndex].bpc;
  int _fps = kPresets[kDefaultPresetIndex].fps;

  bool _running = false;
  DateTime? _startedAt; // 送信開始時刻 (経過時間の表示と履歴記録に使う)
  ({String name, String mime, int size})? _sending; // 送信中のペイロード情報 (終了時に履歴へ)
  int _seq = 0;
  VcodeTx? _tx;
  int _frameIdx = 0;
  int _frameCount = 0;
  ui.Image? _current;
  final Map<int, ui.Image> _cache = {};
  String _status = '';

  Future<void> _pickFile() async {
    final f = await openFile();
    if (f == null) return;
    final len = await f.length();
    setState(() {
      _pickedPath = f.path;
      _pickedName = f.name;
      _pickedMime = f.mimeType;
      _pickedSize = len;
    });
  }

  /// 送信開始からの経過
  Duration get _sendElapsed =>
      _startedAt == null ? Duration.zero : DateTime.now().difference(_startedAt!);

  /// 1 巡にかかる秒数 (= 受信側が最低限これだけ見ていれば全パケットを一度は目にする)
  double get _loopSeconds => _frameCount == 0 ? 0 : _frameCount / _fps;

  static String _fmtElapsed(Duration d) {
    final s = d.inMilliseconds / 1000.0;
    return s >= 60
        ? '${d.inMinutes}分${(s - d.inMinutes * 60).toStringAsFixed(0)}秒'
        : '${s.toStringAsFixed(1)}秒';
  }

  /// 現在の設定で 1 秒あたり何 byte 送れるかの理論値 (実測との比較基準)。
  /// ブロック数 × packet_size × fps。取りこぼしと RaptorQ の符号化冗長は含まない。
  double get _theoreticalKbps {
    final p = _grid.split('x');
    final blocks = int.parse(p[0]) * int.parse(p[1]);
    return blocks * packetSizeFor(_bpc) * _fps / 1024;
  }

  Future<Uint8List?> _buildPayload() async {
    // 計測用テスト画像が選ばれていれば最優先 (毎回同じ内容で条件比較できる)
    if (_testAsset != null) return loadTestImage(_testAsset!);
    if (_pickedPath != null) {
      return XFile(_pickedPath!).readAsBytes();
    }
    final body = Uint8List.fromList(utf8.encode(_textCtrl.text));
    return body.isEmpty ? null : body;
  }

  /// グレースケール frame を ui.Image (RGBA) に変換する
  Future<ui.Image> _toImage(VcodeFrameImage f) {
    final rgba = Uint8List(f.pixels.length * 4);
    for (var i = 0; i < f.pixels.length; i++) {
      final v = f.pixels[i];
      rgba[i * 4] = v;
      rgba[i * 4 + 1] = v;
      rgba[i * 4 + 2] = v;
      rgba[i * 4 + 3] = 255;
    }
    final done = Completer<ui.Image>();
    ui.decodeImageFromPixels(
        rgba, f.width, f.height, ui.PixelFormat.rgba8888, done.complete);
    return done.future;
  }

  Future<ui.Image> _frameImage(int i) async {
    final cached = _cache[i];
    if (cached != null) return cached;
    final img = await _toImage(_tx!.frameGray(i: i));
    // メモリ上限: 600 フレームまでキャッシュ (RGBA 36KB/枚 ≈ 22MB)
    if (_cache.length < 600) _cache[i] = img;
    return img;
  }

  Future<void> _start() async {
    final raw = await _buildPayload();
    if (raw == null) {
      setState(() => _status = 'ペイロードが空です');
      return;
    }
    // 元のファイル名/MIME をヘッダに埋めて送る (受信側で元名・種別をそのまま復元)。
    final name = _testAsset != null
        ? testImageName(_testAsset!)
        : (_pickedName ?? 'message.txt');
    final mime = _testAsset != null
        ? 'image/jpeg'
        : (_pickedPath != null ? (_pickedMime ?? '') : 'text/plain;charset=utf-8');
    final payload = vcodeWrapFile(name: name, mime: mime, data: raw);
    final sourcePackets = (payload.length / packetSizeFor(_bpc)).ceil();
    final gridParts = _grid.split('x');
    final tx = VcodeTx(
        payload: payload,
        extraRepair: (sourcePackets * _repairRate).ceil(),
        gridW: int.parse(gridParts[0]),
        gridH: int.parse(gridParts[1]),
        bitsPerCell: _bpc);
    final seq = ++_seq;
    setState(() {
      _tx = tx;
      _frameIdx = 0;
      _frameCount = tx.frameCount();
      _cache.clear();
      _running = true;
      _status =
          '${payload.length} B → ${tx.packetCount()} pkt / $_frameCount frames';
    });
    debugPrint('[vcode-tx] start: ${payload.length} B, '
        '${tx.packetCount()} packets, $_frameCount frames, $_fps fps');
    // 履歴には送信"終了"時に記録する (送信時間を含めるため)。ここでは材料だけ持っておく。
    _sending = (
      name: name,
      mime: mime.isEmpty ? 'application/octet-stream' : mime,
      size: payload.length,
    );
    _startedAt = DateTime.now();
    await WakelockPlus.enable();
    try {
      await ScreenBrightness().setScreenBrightness(1.0);
    } catch (_) {}
    _txLoop(seq);
  }

  Future<void> _txLoop(int seq) async {
    var lastLog = DateTime.now();
    while (mounted && _running && seq == _seq) {
      final t0 = DateTime.now();
      final img = await _frameImage(_frameIdx % _frameCount);
      if (!mounted || !_running || seq != _seq) break;
      setState(() {
        _current = img;
        _frameIdx++;
      });
      if (DateTime.now().difference(lastLog).inSeconds >= 5) {
        lastLog = DateTime.now();
        debugPrint('[vcode-tx] frame $_frameIdx (pass ${_frameIdx ~/ _frameCount})');
      }
      final elapsed = DateTime.now().difference(t0);
      final interval = Duration(milliseconds: (1000 / _fps).round());
      if (elapsed < interval) {
        await Future.delayed(interval - elapsed);
      }
    }
  }

  Future<void> _stop() async {
    if (!_running) return;
    final info = _sending;
    final elapsed = _sendElapsed;
    final passes = _frameCount == 0 ? 0 : _frameIdx ~/ _frameCount;
    setState(() => _running = false);
    _seq++;
    _startedAt = null;
    _sending = null;
    await WakelockPlus.disable();
    try {
      await ScreenBrightness().resetScreenBrightness();
    } catch (_) {}
    if (info == null) return;
    // grid/ec 欄をフォーマットと条件の記録に流用する (受信側の統計と突き合わせる用)
    unawaited(HistoryStore.instance.addSent(
      info.name,
      info.mime,
      info.size,
      'vcode $_grid',
      '${_fps}fps/${_bpc}bit · ${_fmtElapsed(elapsed)} · $passes 巡',
    ));
    if (mounted) {
      showToast(context, '送信終了: ${_fmtElapsed(elapsed)} · $passes 巡 (履歴に記録)',
          kind: ToastKind.success);
    }
  }

  @override
  void dispose() {
    // 破棄時は setState も context も触れないので、_stop() ではなく
    // 送出ループを止めて端末設定を戻すだけにする (履歴とトーストは省く)。
    _running = false;
    _seq++;
    unawaited(WakelockPlus.disable());
    unawaited(ScreenBrightness().resetScreenBrightness().catchError((_) {}));
    _textCtrl.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    if (_running) {
      // 送信中: 全画面表示 (白背景 = クワイエットゾーン)
      return GestureDetector(
        onTap: _stop,
        child: Container(
          color: Colors.white,
          padding: const EdgeInsets.all(16),
          child: Column(
            children: [
              Expanded(
                child: Center(
                  child: _current == null
                      ? const CircularProgressIndicator()
                      : AspectRatio(
                          aspectRatio: _current!.width / _current!.height,
                          child: RawImage(
                            image: _current,
                            fit: BoxFit.contain,
                            filterQuality: FilterQuality.none,
                          ),
                        ),
                ),
              ),
              // 一方向通信なので受信成功は送信側から分からないが、経過時間は測れる。
              // 受信側の所要時間の目安になり、1 巡に何秒かかるかも分かる。
              Text(
                '${_fmtElapsed(_sendElapsed)}  ·  '
                'frame ${_frameIdx % (_frameCount == 0 ? 1 : _frameCount)}/$_frameCount  ·  '
                '${_frameCount == 0 ? 0 : _frameIdx ~/ _frameCount} 巡'
                '${_loopSeconds > 0 ? " (1巡 ${_loopSeconds.toStringAsFixed(1)}秒)" : ""}',
                style: const TextStyle(color: Colors.black54, fontSize: 12),
              ),
              const SizedBox(height: 8),
              FilledButton.icon(
                onPressed: _stop,
                icon: const Icon(Icons.stop),
                label: const Text('送信を終了'),
              ),
            ],
          ),
        ),
      );
    }

    return ListView(
      padding: const EdgeInsets.all(16),
      children: [
        Text('vcode 送信 (研究)', style: Theme.of(context).textTheme.titleMedium),
        const SizedBox(height: 8),
        TextField(
          controller: _textCtrl,
          maxLines: 3,
          decoration: const InputDecoration(
            border: OutlineInputBorder(),
            labelText: 'テキスト (ファイル未選択時に使用)',
          ),
        ),
        const SizedBox(height: 8),
        Row(
          children: [
            OutlinedButton.icon(
              onPressed: _pickFile,
              icon: const Icon(Icons.attach_file),
              label: const Text('ファイル'),
            ),
            const SizedBox(width: 12),
            Expanded(
              child: Text(
                _pickedName == null ? '未選択' : '$_pickedName ($_pickedSize B)',
                overflow: TextOverflow.ellipsis,
              ),
            ),
            if (_pickedName != null)
              IconButton(
                icon: const Icon(Icons.clear),
                onPressed: () => setState(() {
                  _pickedPath = null;
                  _pickedName = null;
                  _pickedMime = null;
                  _pickedSize = 0;
                }),
              ),
          ],
        ),
        const SizedBox(height: 8),
        // 計測用テストデータ。条件を振って比べるとき、毎回同じ内容を送れるようにする。
        Row(
          children: [
            const Text('計測データ'),
            const SizedBox(width: 12),
            Expanded(
              child: Wrap(
                spacing: 6,
                children: [
                  ChoiceChip(
                    label: const Text('使わない'),
                    selected: _testAsset == null,
                    onSelected: (_) => setState(() => _testAsset = null),
                  ),
                  for (final (label, asset, _) in kTestImages)
                    ChoiceChip(
                      label: Text(label),
                      selected: _testAsset == asset,
                      onSelected: (_) => setState(() {
                        _testAsset = asset;
                        // 誤って古い選択が残らないよう、ファイル選択は解除する
                        _pickedPath = null;
                        _pickedName = null;
                        _pickedMime = null;
                        _pickedSize = 0;
                      }),
                    ),
                ],
              ),
            ),
          ],
        ),
        const SizedBox(height: 8),
        // プリセット: 格子・階調・fps をまとめて決める。個別に合わせると送受信で
        // 食い違い、何が原因か分からなくなるため、まずこれを選ぶ。
        Row(
          children: [
            const Text('プリセット'),
            const SizedBox(width: 12),
            Expanded(
              child: Wrap(
                spacing: 6,
                children: [
                  for (var i = 0; i < kPresets.length; i++)
                    ChoiceChip(
                      label: Text(kPresets[i].name),
                      selected: _presetIndex == i,
                      onSelected: (_) => setState(() {
                        _presetIndex = i;
                        _grid = kPresets[i].grid;
                        _bpc = kPresets[i].bpc;
                        _fps = kPresets[i].fps;
                      }),
                    ),
                ],
              ),
            ),
          ],
        ),
        Padding(
          padding: const EdgeInsets.symmetric(vertical: 6),
          child: Text(
            _presetIndex >= 0
                ? '${kPresets[_presetIndex].description}\n'
                    '理論 ${_theoreticalKbps.toStringAsFixed(0)} KB/s'
                    '  ·  受信側は「${kPresets[_presetIndex].name}」を選ぶ'
                : 'カスタム  ·  理論 ${_theoreticalKbps.toStringAsFixed(0)} KB/s'
                    '  ($_grid · ${_bpc}bit · ${_fps}fps)',
            style: Theme.of(context).textTheme.bodySmall,
          ),
        ),
        ExpansionTile(
          title: Text('詳細設定 (通常は触らない)',
              style: Theme.of(context).textTheme.bodySmall),
          tilePadding: EdgeInsets.zero,
          children: [
        Row(
          children: [
            const Text('格子'),
            const SizedBox(width: 12),
            // 6 つ並ぶと端末幅に入らないので横スクロールさせる
            Flexible(
              child: SingleChildScrollView(
                scrollDirection: Axis.horizontal,
                child: SegmentedButton<String>(
                  segments: const [
                    ButtonSegment(value: '5x4', label: Text('5x4')),
                    ButtonSegment(value: '7x6', label: Text('7x6')),
                    ButtonSegment(value: '9x8', label: Text('9x8')),
                    ButtonSegment(value: '11x10', label: Text('11x10')),
                    ButtonSegment(value: '11x14', label: Text('11x14')),
                    ButtonSegment(value: '13x18', label: Text('13x18')),
                  ],
                  selected: {_grid},
                  showSelectedIcon: false,
                  onSelectionChanged: (s) =>
                      setState(() { _grid = s.first; _presetIndex = -1; }),
                ),
              ),
            ),
          ],
        ),
        const SizedBox(height: 8),
        Row(
          children: [
            const Text('階調'),
            const SizedBox(width: 12),
            SegmentedButton<int>(
              segments: const [
                ButtonSegment(value: 1, label: Text('白黒 1bit')),
                ButtonSegment(value: 2, label: Text('4値 2bit')),
              ],
              selected: {_bpc},
              onSelectionChanged: (s) => setState(() { _bpc = s.first; _presetIndex = -1; }),
            ),
          ],
        ),
        const SizedBox(height: 8),
        Row(
          children: [
            const Text('FPS'),
            Expanded(
              child: Slider(
                value: _fps.toDouble(),
                min: 3,
                max: 60,
                divisions: 57,
                label: '$_fps',
                onChanged: (v) => setState(() { _fps = v.round(); _presetIndex = -1; }),
              ),
            ),
            Text('$_fps'),
          ],
        ),
        // 1 フレームは最低 2 リフレッシュ周期表示しないとキャプチャが遷移を捉える。
        // 60Hz 画面なら 30fps、120Hz 画面なら 60fps が上限の目安。
        Text(
          '理論 ${_theoreticalKbps.toStringAsFixed(0)} KB/s'
          '  ($_grid · ${_bpc}bit · ${_fps}fps)'
          '${_fps > kRefreshSafeFps ? "  ※${kRefreshSafeFps}fps 超は 120Hz 画面向け" : ""}',
          style: Theme.of(context).textTheme.bodySmall,
        ),
        Row(
          children: [
            const Text('repair'),
            Expanded(
              child: Slider(
                value: _repairRate,
                min: 0.1,
                max: 1.5,
                divisions: 14,
                label: _repairRate.toStringAsFixed(1),
                onChanged: (v) => setState(() => _repairRate = v),
              ),
            ),
            Text('${(_repairRate * 100).round()}%'),
          ],
        ),
        ]),
        const SizedBox(height: 8),
        FilledButton.icon(
          onPressed: _start,
          icon: const Icon(Icons.play_arrow),
          label: const Text('送信開始'),
        ),
        const SizedBox(height: 8),
        Text(_status),
      ],
    );
  }
}
