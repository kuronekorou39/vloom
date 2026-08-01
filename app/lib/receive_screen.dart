import 'dart:io';
import 'dart:typed_data';
import 'package:flutter/material.dart';
import 'package:mobile_scanner/mobile_scanner.dart';
import 'package:share_plus/share_plus.dart';
import 'package:wakelock_plus/wakelock_plus.dart';
import 'history_store.dart';
import 'protocol.dart';
import 'ui_common.dart';
import 'src/rust/api/fountain.dart';
import 'vcode_view.dart';

/// mobile_scanner の rawDecodedBytes (sealed) から実バイト列を取り出す。
Uint8List? _decodedBytes(BarcodeBytes? b) {
  return switch (b) {
    DecodedBarcodeBytes(:final bytes) => bytes,
    DecodedVisionBarcodeBytes(:final bytes, :final rawBytes) => bytes ?? rawBytes,
    null => null,
  };
}

String _hex4(Uint8List b) {
  final n = b.length < 4 ? b.length : 4;
  final sb = StringBuffer();
  for (var i = 0; i < n; i++) {
    sb.write(b[i].toRadixString(16).padLeft(2, '0'));
  }
  return sb.toString();
}

String _fmtSize(int n) {
  if (n >= 1024 * 1024 * 1024) return '${(n / 1024 / 1024 / 1024).toStringAsFixed(2)}GB';
  if (n >= 1024 * 1024) return '${(n / 1024 / 1024).toStringAsFixed(1)}MB';
  if (n >= 1024) return '${(n / 1024).toStringAsFixed(1)}KB';
  return '${n}B';
}

class ReceiveScreen extends StatefulWidget {
  /// このタブが表示中で校正も開いていないとき true。false の間はカメラを解放し、
  /// 他画面 (校正・V受信) と背面カメラを奪い合わないようにする。
  const ReceiveScreen({super.key, this.active = true});
  final bool active;
  @override
  State<ReceiveScreen> createState() => _ReceiveScreenState();
}

class _ReceiveScreenState extends State<ReceiveScreen> {
  MobileScannerController? _controller;

  StreamManifest? _manifest;
  String? _recvId;
  String? _outPath;
  RandomAccessFile? _outFile;
  bool _settingUp = false;
  bool _finalizing = false;

  final Set<int> _doneBlocks = {};
  final Map<int, FountainDecoder> _decoders = {};
  final Map<int, Set<String>> _seen = {};
  final List<(int, Uint8List)> _writeQueue = [];
  bool _writing = false;

  ({String name, String type, int size}) get _mInfo =>
      (name: _manifest!.name, type: _manifest!.type, size: _manifest!.totalSize);

  ({String name, String type, int size, String path})? _result;
  String? _error;

  @override
  void initState() {
    super.initState();
    if (widget.active) _startCamera();
  }

  Future<void> _startCamera() async {
    await WakelockPlus.enable();
    _resetState();
    setState(() {
      _controller = MobileScannerController(
        detectionSpeed: DetectionSpeed.unrestricted,
        formats: const [BarcodeFormat.qrCode],
      );
    });
  }

  void _resetState() {
    _manifest = null;
    _recvId = null;
    _outPath = null;
    _outFile?.close();
    _outFile = null;
    _settingUp = false;
    _finalizing = false;
    _doneBlocks.clear();
    _decoders.clear();
    _seen.clear();
    _writeQueue.clear();
    _writing = false;
    _result = null;
    _error = null;
  }

  Future<void> _stopCamera() async {
    await _controller?.dispose();
    await _outFile?.close();
    _outFile = null;
    await WakelockPlus.disable();
    setState(() => _controller = null);
  }

  void _onDetect(BarcodeCapture capture) {
    if (_result != null || _error != null) return;
    var changed = false;
    for (final bc in capture.barcodes) {
      final bytes = _decodedBytes(bc.rawDecodedBytes);
      if (bytes == null || bytes.isEmpty) continue;
      final t = bytes[0];
      if (t == kFrameManifest) {
        if (_manifest == null && !_settingUp) {
          final m = StreamManifest.tryParse(bytes);
          if (m != null) _setupOutput(m);
        }
      } else if (t == kFrameData) {
        if (_manifest == null || _outFile == null) continue;
        final d = parseDataQr(bytes);
        if (d != null && _feedData(d.blockIndex, d.packet)) changed = true;
      }
    }
    if (changed) setState(() {});
  }

  Future<void> _setupOutput(StreamManifest m) async {
    _settingUp = true;
    try {
      final r = HistoryStore.instance.reserveReceivedPath();
      _recvId = r.id;
      _outPath = r.path;
      _outFile = await File(r.path).open(mode: FileMode.write);
      _manifest = m;
      if (mounted) setState(() {});
    } catch (e) {
      _error = '出力ファイル作成失敗: $e';
      if (mounted) setState(() {});
    } finally {
      _settingUp = false;
    }
  }

  bool _feedData(int idx, Uint8List packet) {
    final m = _manifest!;
    if (idx < 0 || idx >= m.blockCount || _doneBlocks.contains(idx)) return false;
    final seen = _seen.putIfAbsent(idx, () => <String>{});
    final key = _hex4(packet);
    if (seen.contains(key)) return false;
    seen.add(key);
    final dec = _decoders.putIfAbsent(idx, () => FountainDecoder(otiBytes: m.otiFor(idx)));
    try {
      if (dec.addPacket(packet: Uint8List.fromList(packet))) {
        final b = dec.payload();
        if (b != null) {
          _doneBlocks.add(idx);
          _decoders.remove(idx);
          _seen.remove(idx);
          _writeQueue.add((idx, b));
          _drainWrites();
        }
      }
    } catch (_) {}
    return true;
  }

  Future<void> _drainWrites() async {
    if (_writing) return;
    _writing = true;
    try {
      final m = _manifest!;
      while (_writeQueue.isNotEmpty) {
        final item = _writeQueue.removeAt(0);
        await _outFile!.setPosition(item.$1 * m.blockSize);
        await _outFile!.writeFrom(item.$2);
      }
      if (_doneBlocks.length == m.blockCount && !_finalizing) {
        await _finalize();
      }
    } catch (e) {
      _error = 'ストレージ書き込み失敗 (容量不足?): $e';
      if (mounted) setState(() {});
    } finally {
      _writing = false;
    }
  }

  Future<void> _finalize() async {
    _finalizing = true;
    final m = _manifest!;
    await _outFile!.flush();
    await _outFile!.close();
    _outFile = null;
    await HistoryStore.instance.registerReceived(_recvId!, m.name, m.type, m.totalSize);
    _result = (name: m.name, type: m.type, size: m.totalSize, path: _outPath!);
    await _controller?.dispose();
    _controller = null;
    await WakelockPlus.disable();
    if (mounted) setState(() {});
  }

  @override
  void didUpdateWidget(ReceiveScreen old) {
    super.didUpdateWidget(old);
    if (!old.active && widget.active) {
      // 表示に戻った: 結果/エラー表示中でなければ自動でスキャン再開 (vcode 受信と統一)
      if (_controller == null && _result == null && _error == null) {
        _startCamera();
      }
    } else if (old.active && !widget.active && _controller != null) {
      // 非表示 / 校正表示中になったらカメラを解放し、奪い合いを防ぐ
      _stopCamera();
    }
  }

  @override
  void dispose() {
    _controller?.dispose();
    _outFile?.close();
    WakelockPlus.disable();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    if (_result != null) return _buildResult(context);
    if (_error != null) return _buildError(context);
    if (_controller == null) return _buildIdle(context);
    return _buildScanning(context);
  }

  Widget _buildIdle(BuildContext context) {
    return Center(
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(Icons.photo_camera, size: 64, color: Theme.of(context).colorScheme.primary),
          const SizedBox(height: 16),
          const Text('送信側の QR にカメラを向けて受信します'),
          const SizedBox(height: 16),
          FilledButton.icon(
              onPressed: _startCamera, icon: const Icon(Icons.play_arrow), label: const Text('カメラ開始')),
        ],
      ),
    );
  }

  Widget _buildScanning(BuildContext context) {
    final m = _manifest;
    final done = _doneBlocks.length;
    final total = m?.blockCount ?? 0;
    final pct = total > 0 ? (done / total) : 0.0;
    return Column(
      children: [
        Expanded(
          child: Stack(
            fit: StackFit.expand,
            children: [
              // 拡大率を vcode 側 (幅基準) と揃える
              MobileScanner(
                  controller: _controller!,
                  onDetect: _onDetect,
                  fit: BoxFit.fitWidth),
              const ScanGuideOverlay(),
            ],
          ),
        ),
        Padding(
          padding: const EdgeInsets.all(12),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              if (m == null)
                const Text('マニフェスト待ち... 送信側の QR に向けてください')
              else ...[
                Text('${_mInfo.name}  ·  ${_fmtSize(_mInfo.size)}',
                    overflow: TextOverflow.ellipsis),
                const SizedBox(height: 4),
                LinearProgressIndicator(value: pct),
                const SizedBox(height: 4),
                Text('ブロック $done / $total  (${(pct * 100).toStringAsFixed(1)}%)'
                    '   進行中 ${_decoders.length}'),
              ],
              const SizedBox(height: 8),
              FilledButton.tonalIcon(
                  onPressed: _stopCamera, icon: const Icon(Icons.stop), label: const Text('停止')),
            ],
          ),
        ),
      ],
    );
  }

  Widget _buildResult(BuildContext context) {
    final r = _result!;
    final isImage = r.type.startsWith('image/');
    return ListView(
      padding: const EdgeInsets.all(16),
      children: [
        Card(
          color: Colors.green.withValues(alpha: 0.15),
          child: ListTile(
            leading: const Icon(Icons.check_circle, color: Colors.green),
            title: Text('復元成功: ${r.name}'),
            subtitle: Text('${r.type}  ${_fmtSize(r.size)}'),
          ),
        ),
        const SizedBox(height: 12),
        if (isImage)
          ClipRRect(
            borderRadius: BorderRadius.circular(8),
            child: Image.file(File(r.path), fit: BoxFit.contain),
          )
        else
          Container(
            padding: const EdgeInsets.symmetric(vertical: 28),
            color: Colors.black.withValues(alpha: 0.25),
            child: Column(
              children: [
                Icon(r.type.startsWith('video/') ? Icons.movie : Icons.insert_drive_file,
                    size: 44, color: Theme.of(context).colorScheme.onSurfaceVariant),
                const SizedBox(height: 8),
                Text('アプリで開いて中身を確認できます',
                    style: Theme.of(context).textTheme.bodySmall),
              ],
            ),
          ),
        const SizedBox(height: 16),
        // 受信したものを取り出す導線。従来は保存先パスを出すだけで、
        // 保存も共有も開くこともできなかった。
        Wrap(
          alignment: WrapAlignment.center,
          spacing: 12,
          runSpacing: 8,
          children: [
            FilledButton.icon(
              onPressed: () => openWithDefaultApp(context, File(r.path)),
              icon: const Icon(Icons.open_in_new),
              label: const Text('アプリで開く'),
            ),
            FilledButton.tonalIcon(
              onPressed: () => SharePlus.instance
                  .share(ShareParams(
                  files: [XFile(r.path, mimeType: r.type, name: r.name)])),
              icon: const Icon(Icons.share),
              label: const Text('共有'),
            ),
            FilledButton.tonalIcon(
                onPressed: _startCamera,
                icon: const Icon(Icons.replay),
                label: const Text('もう一度受信')),
          ],
        ),
      ],
    );
  }

  Widget _buildError(BuildContext context) {
    return Center(
      child: Padding(
        padding: const EdgeInsets.all(24),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            const Icon(Icons.error, color: Colors.red, size: 48),
            const SizedBox(height: 12),
            Text(_error ?? 'エラー', textAlign: TextAlign.center),
            const SizedBox(height: 16),
            FilledButton.icon(
                onPressed: _startCamera, icon: const Icon(Icons.replay), label: const Text('やり直す')),
          ],
        ),
      ),
    );
  }
}
