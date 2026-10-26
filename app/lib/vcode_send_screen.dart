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

/// 送るものの種類。送るのは常にこのうち選ばれている 1 つだけ。
/// 以前はテキスト欄とファイル欄が並んでいて、両方に入れるとテキストが黙って無視された。
enum TxSource { file, text, test }

const _kSourceLabel = {
  TxSource.file: 'ファイル',
  TxSource.text: 'テキスト',
  TxSource.test: '計測データ',
};

/// 選択中のファイル 1 件
typedef _Picked = ({String path, String name, String mime, int size});

/// vcode (独自フォーマット) 送信画面。ペイロードをアニメーション vcode で送出する。
class VcodeSendScreen extends StatefulWidget {
  const VcodeSendScreen({super.key});
  @override
  State<VcodeSendScreen> createState() => _VcodeSendScreenState();
}

class _VcodeSendScreenState extends State<VcodeSendScreen> {
  final _textCtrl = TextEditingController();
  TxSource _source = TxSource.file;
  // 選択中のファイル。選び直しても置き換えず、足していく
  final List<_Picked> _files = [];
  // 計測用テスト画像 (毎回同じ内容で条件比較できる)。既定は定常が測れる 1MB
  String _testAsset = kTestImages[2].$2;
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

  @override
  void initState() {
    super.initState();
    // 入力のたびに送信内容のまとめ (サイズ・目安) を更新する
    _textCtrl.addListener(() => setState(() {}));
  }

  Future<void> _pickFiles() async {
    final picked = await openFiles();
    if (picked.isEmpty) return;
    final added = <_Picked>[];
    for (final f in picked) {
      added.add((
        path: f.path,
        name: f.name,
        mime: f.mimeType ?? '',
        size: await f.length(),
      ));
    }
    if (!mounted) return;
    setState(() {
      for (final f in added) {
        // 同じものを 2 度選んだときは足さない
        if (!_files.any((g) => g.path == f.path)) _files.add(f);
      }
    });
  }

  int get _textBytes => utf8.encode(_textCtrl.text).length;

  /// 今の選択で送るものの大きさ (何も無ければ 0)
  int get _planBytes => switch (_source) {
        TxSource.file => _files.fold(0, (a, f) => a + f.size),
        TxSource.text => _textBytes,
        TxSource.test =>
          kTestImages.firstWhere((t) => t.$2 == _testAsset).$3,
      };

  /// 今の選択で送るものの説明
  String get _planText => switch (_source) {
        TxSource.file => _files.isEmpty
            ? 'ファイルが選ばれていません'
            : _files.length == 1
                ? '${_files.first.name} · ${fmtSize(_planBytes)}'
                : 'ファイル ${_files.length} 個 (ZIP) · ${fmtSize(_planBytes)}',
        TxSource.text => _textBytes == 0
            ? 'テキストが入力されていません'
            : 'テキスト · ${fmtSize(_textBytes)}',
        TxSource.test =>
          '計測データ · ${testImageName(_testAsset)} (${fmtSize(_planBytes)})',
      };

  /// 選んでいない側に残っている入力 (送られないことを明示するため)
  String? get _leftover {
    final left = [
      if (_source != TxSource.file && _files.isNotEmpty) 'ファイル ${_files.length} 個',
      if (_source != TxSource.text && _textBytes > 0) '入力済みのテキスト',
    ];
    if (left.isEmpty) return null;
    return '${left.join("と")}は送られません (送るのは「${_kSourceLabel[_source]}」)';
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

  /// 選ばれているものを、送る 1 本のバイト列 (と名前・種別) にする
  Future<({Uint8List bytes, String name, String mime})> _buildPayload() async {
    final now = DateTime.now();
    String two(int v) => v.toString().padLeft(2, '0');
    final stamp = '${now.year}${two(now.month)}${two(now.day)}_'
        '${two(now.hour)}${two(now.minute)}${two(now.second)}';
    switch (_source) {
      case TxSource.test:
        return (
          bytes: await loadTestImage(_testAsset),
          name: testImageName(_testAsset),
          mime: 'image/jpeg',
        );
      case TxSource.text:
        // 毎回 message.txt だと受信側で同名が積み重なるので、送信時刻を付ける
        return (
          bytes: Uint8List.fromList(utf8.encode(_textCtrl.text)),
          name: 'message_$stamp.txt',
          mime: 'text/plain;charset=utf-8',
        );
      case TxSource.file:
        if (_files.length == 1) {
          final f = _files.first;
          return (
            bytes: await XFile(f.path).readAsBytes(),
            name: f.name,
            mime: f.mime,
          );
        }
        // 複数ファイルは無圧縮 ZIP にまとめる (形式は 1 本のバイト列しか運ばない)。
        // 受信側がまとめを解けない古い版でも、ZIP としてそのまま開ける。
        final entries = <VcodeBundleFile>[
          for (final f in _files)
            VcodeBundleFile(
                name: f.name, data: await XFile(f.path).readAsBytes()),
        ];
        return (
          bytes: vcodeBundleFiles(
            files: entries,
            year: now.year,
            month: now.month,
            day: now.day,
            hour: now.hour,
            minute: now.minute,
            second: now.second,
          ),
          name: 'vloom_$stamp.zip',
          mime: 'application/zip',
        );
    }
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
    if (_planBytes == 0) return;
    final ({Uint8List bytes, String name, String mime}) src;
    try {
      src = await _buildPayload();
    } catch (e) {
      // 選んだ後で消された・権限が切れた等
      if (mounted) setState(() => _status = '読み込めませんでした: $e');
      return;
    }
    final name = src.name;
    final mime = src.mime;
    // 元のファイル名/MIME をヘッダに埋めて送る (受信側で元名・種別をそのまま復元)
    final payload = vcodeWrapFile(name: name, mime: mime, data: src.bytes);
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
      _status = '';
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
      // 巡ごとに 1 フレームずらす (デスクトップ送信の _frame_arg と同じ)。毎巡同じ並びだと、
      // 受信側の間引きの位相が揃ったときに同じフレームを落とし続け、読めないブロック位置の
      // パケットも何巡しても届かない。ずらせば同じ位置が別のパケットを運ぶ
      final img = await _frameImage(
          _frameIdx % _frameCount + _frameIdx ~/ _frameCount);
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
          child: SafeArea(
            minimum: const EdgeInsets.all(16),
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
                  '${_sending == null ? "" : "${fmtSize(_sending!.size)}  ·  "}'
                  '${_fmtElapsed(_sendElapsed)}  ·  '
                  '${_frameCount == 0 ? 1 : _frameIdx ~/ _frameCount + 1} 巡目'
                  '${_loopSeconds > 0 ? " (1 巡 ${_loopSeconds.toStringAsFixed(1)} 秒)" : ""}',
                  textAlign: TextAlign.center,
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
        ),
      );
    }

    // Scaffold が本文を AppBar とナビの下まで伸ばしている (extendBody) ので、
    // その分を余白に足す。固定の余白だけだと上下の端が隠れる
    final inset = MediaQuery.paddingOf(context);
    return ListView(
      padding: EdgeInsets.fromLTRB(12, inset.top + 12, 12, inset.bottom + 16),
      children: [
        _section(
          context,
          '送るもの',
          [
            SizedBox(
              width: double.infinity,
              child: SegmentedButton<TxSource>(
                segments: [
                  for (final e in _kSourceLabel.entries)
                    ButtonSegment(value: e.key, label: Text(e.value)),
                ],
                selected: {_source},
                showSelectedIcon: false,
                onSelectionChanged: (s) => setState(() => _source = s.first),
              ),
            ),
            const SizedBox(height: 12),
            ...switch (_source) {
              TxSource.file => _fileSource(context),
              TxSource.text => _textSource(context),
              TxSource.test => _testSource(context),
            },
          ],
        ),
        _section(
          context,
          '設定',
          [
            // プリセット: 格子・階調・fps をまとめて決める。個別に合わせると送受信で
            // 食い違い、何が原因か分からなくなるため、まずこれを選ぶ。
            Wrap(
              spacing: 6,
              runSpacing: 4,
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
            const SizedBox(height: 6),
            Text(
              _presetIndex >= 0
                  ? '${kPresets[_presetIndex].description}\n'
                      '受信側は「${kPresets[_presetIndex].name}」を選ぶ'
                  : 'カスタム ($_grid · ${_bpc}bit · ${_fps}fps)',
              style: Theme.of(context).textTheme.bodySmall,
            ),
            ExpansionTile(
              title: Text('詳細設定 (通常は触らない)',
                  style: Theme.of(context).textTheme.bodySmall),
              tilePadding: EdgeInsets.zero,
              shape: const Border(),
              children: _advanced(context),
            ),
          ],
        ),
        _section(
          context,
          '送信',
          [
            Text(
              _planText,
              style: Theme.of(context).textTheme.titleMedium?.copyWith(
                    color: _planBytes == 0
                        ? Theme.of(context).colorScheme.onSurfaceVariant
                        : null,
                  ),
            ),
            if (_leftover != null) _notice(context, _leftover!),
            const SizedBox(height: 10),
            ..._estimate(context),
            const SizedBox(height: 12),
            SizedBox(
              width: double.infinity,
              child: FilledButton.icon(
                onPressed: _planBytes == 0 ? null : _start,
                icon: const Icon(Icons.play_arrow),
                label: const Padding(
                  padding: EdgeInsets.symmetric(vertical: 12),
                  child: Text('送信開始'),
                ),
              ),
            ),
            if (_status.isNotEmpty) ...[
              const SizedBox(height: 8),
              Text(_status,
                  style: TextStyle(color: Theme.of(context).colorScheme.error)),
            ],
          ],
        ),
      ],
    );
  }

  /// 見出し付きのまとまり
  Widget _section(BuildContext context, String title, List<Widget> children) {
    final scheme = Theme.of(context).colorScheme;
    return Card(
      margin: const EdgeInsets.only(bottom: 12),
      child: Padding(
        padding: const EdgeInsets.all(14),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(title,
                style: Theme.of(context)
                    .textTheme
                    .labelMedium
                    ?.copyWith(color: scheme.onSurfaceVariant)),
            const SizedBox(height: 10),
            ...children,
          ],
        ),
      ),
    );
  }

  /// 見落とされたくない注意 (送られない入力がある等)
  Widget _notice(BuildContext context, String text) {
    return Container(
      width: double.infinity,
      margin: const EdgeInsets.only(top: 8),
      padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 8),
      decoration: BoxDecoration(
        color: const Color(0x38B45309),
        borderRadius: BorderRadius.circular(8),
      ),
      child: Text(text,
          style: const TextStyle(color: Color(0xFFF3C98B), fontSize: 12)),
    );
  }

  List<Widget> _fileSource(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return [
      SizedBox(
        width: double.infinity,
        child: OutlinedButton.icon(
          onPressed: _pickFiles,
          icon: const Icon(Icons.attach_file),
          label: Padding(
            padding: const EdgeInsets.symmetric(vertical: 12),
            child: Text(_files.isEmpty ? 'ファイルを選ぶ (複数可)' : 'ファイルを追加'),
          ),
        ),
      ),
      for (final f in _files)
        Container(
          margin: const EdgeInsets.only(top: 6),
          padding: const EdgeInsets.only(left: 12),
          decoration: BoxDecoration(
            color: scheme.surfaceContainerHighest,
            borderRadius: BorderRadius.circular(8),
          ),
          child: Row(
            children: [
              Expanded(child: Text(f.name, overflow: TextOverflow.ellipsis)),
              const SizedBox(width: 8),
              Text(fmtSize(f.size),
                  style: TextStyle(
                      fontSize: 12, color: scheme.onSurfaceVariant)),
              IconButton(
                icon: const Icon(Icons.close, size: 18),
                tooltip: '${f.name} を外す',
                visualDensity: VisualDensity.compact,
                onPressed: () => setState(() => _files.remove(f)),
              ),
            ],
          ),
        ),
      if (_files.isNotEmpty)
        Row(
          children: [
            Expanded(
              child: Text(
                _files.length > 1 ? '${_files.length} 個を 1 つの ZIP にまとめて送ります' : '',
                style: Theme.of(context).textTheme.bodySmall,
              ),
            ),
            TextButton(
              onPressed: () => setState(_files.clear),
              child: const Text('すべて外す'),
            ),
          ],
        ),
    ];
  }

  List<Widget> _textSource(BuildContext context) => [
        TextField(
          controller: _textCtrl,
          maxLines: 5,
          decoration: const InputDecoration(
            border: OutlineInputBorder(),
            hintText: '送るテキストを入力',
          ),
        ),
        const SizedBox(height: 6),
        Text('受信側にはテキストファイル (.txt) として届きます。',
            style: Theme.of(context).textTheme.bodySmall),
      ];

  // 計測用テストデータ。条件を振って比べるとき、毎回同じ内容を送れるようにする。
  List<Widget> _testSource(BuildContext context) => [
        Wrap(
          spacing: 6,
          runSpacing: 4,
          children: [
            for (final (label, asset, _) in kTestImages)
              ChoiceChip(
                label: Text(label),
                selected: _testAsset == asset,
                onSelected: (_) => setState(() => _testAsset = asset),
              ),
          ],
        ),
        const SizedBox(height: 6),
        Text(
          '条件を振って比べるための固定画像です。サイズを焼き込んであるので、'
          '受信側で開けばどれが壊れずに届いたか分かります。',
          style: Theme.of(context).textTheme.bodySmall,
        ),
      ];

  /// 所要時間の目安。理論値ではなく実測の中央値から出す (出どころは preset.dart)
  List<Widget> _estimate(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final small = Theme.of(context).textTheme.bodySmall;
    final theory = '理論 ${_theoreticalKbps.toStringAsFixed(0)} KB/s'
        ' ($_grid · ${_bpc}bit · ${_fps}fps)'
        '${_fps > kRefreshSafeFps ? "  ※${kRefreshSafeFps}fps 超は 120Hz 画面向け" : ""}';
    final kbps = measuredKbps(_grid, _bpc, _fps);
    if (_planBytes == 0) return [Text(theory, style: small)];
    if (kbps == null) {
      return [
        Text('この設定は実測が無いため、所要時間の目安を出せません。\n$theory', style: small),
      ];
    }
    return [
      Container(
        width: double.infinity,
        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 10),
        decoration: BoxDecoration(
          color: scheme.surfaceContainerLowest,
          borderRadius: BorderRadius.circular(8),
          border: Border.all(color: scheme.outlineVariant),
        ),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text('所要時間の目安 (アプリで受信)', style: small),
            Text(
              fmtEstimate(_planBytes / 1024 / kbps),
              style: Theme.of(context)
                  .textTheme
                  .headlineSmall
                  ?.copyWith(fontWeight: FontWeight.w600),
            ),
            Text('実測 ${kbps.toStringAsFixed(0)} KB/s', style: small),
          ],
        ),
      ),
      const SizedBox(height: 6),
      Text('実測の中央値からの目安 (三脚固定・Pixel 9a)。手持ちや距離で変わります。\n$theory',
          style: small),
    ];
  }

  List<Widget> _advanced(BuildContext context) => [
        Row(
          children: [
            const SizedBox(width: 44, child: Text('格子')),
            // 6 つ並ぶと端末幅に入らないので横スクロールさせる
            Expanded(
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
            const SizedBox(width: 44, child: Text('階調')),
            Expanded(
              child: SingleChildScrollView(
                scrollDirection: Axis.horizontal,
                child: SegmentedButton<int>(
                  segments: const [
                    ButtonSegment(value: 1, label: Text('白黒 1bit')),
                    ButtonSegment(value: 2, label: Text('4値 2bit')),
                  ],
                  selected: {_bpc},
                  showSelectedIcon: false,
                  onSelectionChanged: (s) =>
                      setState(() { _bpc = s.first; _presetIndex = -1; }),
                ),
              ),
            ),
          ],
        ),
        // 1 フレームは最低 2 リフレッシュ周期表示しないとキャプチャが遷移を捉える。
        // 60Hz 画面なら 30fps、120Hz 画面なら 60fps が上限の目安。
        Row(
          children: [
            const SizedBox(width: 44, child: Text('FPS')),
            Expanded(
              child: Slider(
                value: _fps.toDouble(),
                min: 3,
                max: 60,
                divisions: 57,
                label: '$_fps',
                onChanged: (v) =>
                    setState(() { _fps = v.round(); _presetIndex = -1; }),
              ),
            ),
            SizedBox(width: 44, child: Text('$_fps', textAlign: TextAlign.end)),
          ],
        ),
        Row(
          children: [
            const SizedBox(width: 44, child: Text('repair')),
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
            SizedBox(
                width: 44,
                child: Text('${(_repairRate * 100).round()}%',
                    textAlign: TextAlign.end)),
          ],
        ),
      ];
}
