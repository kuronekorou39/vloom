import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';
import 'package:flutter/material.dart';
import 'package:flutter_file_dialog/flutter_file_dialog.dart';
import 'package:path_provider/path_provider.dart';
import 'package:share_plus/share_plus.dart';
import 'history_store.dart';
import 'ui_common.dart';

/// ファイルシステムで安全なファイル名にする (パス区切り・禁止文字を _ に)。

/// 受信ファイルを OS の共有シートで送る (他アプリ送付・ドライブ等)。
/// 内部保存は ID 名 (拡張子なし) なので、受け側が正しい表示名・拡張子を得られるよう
/// 元のファイル名でキャッシュにコピーしてから共有する。
Future<void> shareReceived(HistoryItem item) async {
  final file = HistoryStore.instance.receivedFile(item);
  if (file == null || !await file.exists()) return;
  XFile x;
  try {
    final tmp = await getTemporaryDirectory();
    final dir = Directory('${tmp.path}/share')..createSync(recursive: true);
    final dst = File('${dir.path}/${safeFileName(item.name)}');
    await file.copy(dst.path);
    x = XFile(dst.path, mimeType: item.type, name: item.name);
  } catch (_) {
    x = XFile(file.path, mimeType: item.type, name: item.name);
  }
  await SharePlus.instance.share(ShareParams(files: [x]));
}

/// 受信ファイルを端末の任意の場所へ「ファイルに保存」する (Android は SAF の保存ダイアログ)。
/// 元のファイル名を初期値にする。保存できたら true、キャンセル/失敗で false。
Future<bool> saveReceivedToFile(HistoryItem item) async {
  final file = HistoryStore.instance.receivedFile(item);
  if (file == null || !await file.exists()) return false;
  final params = SaveFileDialogParams(
    sourceFilePath: file.path,
    fileName: safeFileName(item.name),
    mimeTypesFilter: item.type.isNotEmpty ? [item.type] : null,
  );
  final saved = await FlutterFileDialog.saveFile(params: params);
  return saved != null;
}

class HistoryScreen extends StatelessWidget {
  const HistoryScreen({super.key});

  @override
  Widget build(BuildContext context) {
    return ValueListenableBuilder<int>(
      valueListenable: HistoryStore.instance.revision,
      builder: (context, _, _) {
        final store = HistoryStore.instance;
        return DefaultTabController(
          length: 2,
          child: Column(
            children: [
              const TabBar(tabs: [Tab(text: '受信'), Tab(text: '送信')]),
              Expanded(
                child: TabBarView(
                  children: [
                    _ReceivedList(items: store.received),
                    _SentList(items: store.sent),
                  ],
                ),
              ),
            ],
          ),
        );
      },
    );
  }
}

String _fmtTime(DateTime t) {
  String two(int n) => n.toString().padLeft(2, '0');
  return '${t.month}/${t.day} ${two(t.hour)}:${two(t.minute)}';
}

String _fmtSize(int n) {
  if (n >= 1024 * 1024) return '${(n / 1024 / 1024).toStringAsFixed(1)}MB';
  if (n >= 1024) return '${(n / 1024).toStringAsFixed(1)}KB';
  return '${n}B';
}

class _ReceivedList extends StatelessWidget {
  final List<HistoryItem> items;
  const _ReceivedList({required this.items});

  @override
  Widget build(BuildContext context) {
    if (items.isEmpty) return const Center(child: Text('受信履歴はまだありません'));
    return ListView.separated(
      itemCount: items.length,
      separatorBuilder: (_, _) => const Divider(height: 1),
      itemBuilder: (context, i) {
        final it = items[i];
        final isImage = it.type.startsWith('image/');
        return ListTile(
          leading: Icon(isImage ? Icons.image : Icons.insert_drive_file),
          title: Text(it.name, overflow: TextOverflow.ellipsis),
          subtitle: Text([
            '${_fmtSize(it.size)}  ·  ${_fmtTime(it.time)}',
            if (it.note != null) it.note!,
          ].join('\n')),
          isThreeLine: it.note != null,
          trailing: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              IconButton(
                icon: const Icon(Icons.save_alt),
                tooltip: '端末に保存',
                onPressed: () => saveReceivedToFile(it),
              ),
              IconButton(
                icon: const Icon(Icons.share),
                tooltip: '共有',
                onPressed: () => shareReceived(it),
              ),
            ],
          ),
          onTap: () => Navigator.of(context).push(MaterialPageRoute(
            builder: (_) => _ViewerPage(item: it),
          )),
        );
      },
    );
  }
}

class _SentList extends StatelessWidget {
  final List<HistoryItem> items;
  const _SentList({required this.items});

  @override
  Widget build(BuildContext context) {
    if (items.isEmpty) return const Center(child: Text('送信履歴はまだありません'));
    return ListView.separated(
      itemCount: items.length,
      separatorBuilder: (_, _) => const Divider(height: 1),
      itemBuilder: (context, i) {
        final it = items[i];
        return ListTile(
          leading: const Icon(Icons.upload),
          title: Text(it.name, overflow: TextOverflow.ellipsis),
          subtitle: Text(
              '${_fmtSize(it.size)}  ·  grid ${it.grid ?? "-"} / EC ${it.ec ?? "-"}  ·  ${_fmtTime(it.time)}'),
        );
      },
    );
  }
}

class _ViewerPage extends StatelessWidget {
  final HistoryItem item;
  const _ViewerPage({required this.item});

  @override
  Widget build(BuildContext context) {
    final file = HistoryStore.instance.receivedFile(item);
    Widget body;
    if (file == null) {
      body = const Center(child: Text('ファイルが見つかりません'));
    } else if (item.type.startsWith('image/')) {
      // 画像は Image.file でストリーム表示 (大きくてもメモリに全読みしない)。
      body = Center(child: InteractiveViewer(child: Image.file(file)));
    } else if (item.type.startsWith('text/') && item.size <= 1024 * 1024) {
      body = FutureBuilder<Uint8List?>(
        future: HistoryStore.instance.readReceived(item),
        builder: (context, snap) {
          if (!snap.hasData || snap.data == null) {
            return const Center(child: CircularProgressIndicator());
          }
          return SingleChildScrollView(
            padding: const EdgeInsets.all(16),
            child: SelectableText(utf8.decode(snap.data!, allowMalformed: true)),
          );
        },
      );
    } else {
      body = Center(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            const Icon(Icons.check_circle, color: Colors.green, size: 48),
            const SizedBox(height: 8),
            Text(item.name),
            Text('${item.type}  ${_fmtSize(item.size)}'),
            const SizedBox(height: 8),
            const SizedBox(height: 16),
            FilledButton.icon(
              onPressed: () => openWithDefaultApp(context, file),
              icon: const Icon(Icons.open_in_new),
              label: const Text('アプリで開く'),
            ),
          ],
        ),
      );
    }
    return Scaffold(
      appBar: AppBar(
        title: Text(item.name),
        actions: [
          IconButton(
            icon: const Icon(Icons.save_alt),
            tooltip: '端末に保存',
            onPressed: () => saveReceivedToFile(item),
          ),
          IconButton(
            icon: const Icon(Icons.share),
            tooltip: '共有',
            onPressed: () => shareReceived(item),
          ),
          if (file != null)
            IconButton(
              icon: const Icon(Icons.open_in_new),
              tooltip: 'アプリで開く',
              onPressed: () => openWithDefaultApp(context, file),
            ),
        ],
      ),
      body: item.note == null
          ? body
          : Column(
              children: [
                Padding(
                  padding: const EdgeInsets.all(8),
                  child: Text(item.note!,
                      style: Theme.of(context).textTheme.bodySmall),
                ),
                Expanded(child: body),
              ],
            ),
    );
  }
}
