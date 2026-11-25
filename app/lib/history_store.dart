import 'dart:convert';
import 'dart:io';
import 'package:flutter/foundation.dart';
import 'package:path_provider/path_provider.dart';

import 'receive/file_sniff.dart';

/// 履歴の1エントリ。received は本体ファイルを端末に保存 (file に相対パス)、
/// sent は試行の記録 (grid/ec など設定)。
class HistoryItem {
  final String id;
  final String name;
  final String type;
  final int size;
  final int tsMs; // epoch ms
  final String? file; // received: 保存ファイルの相対パス
  final String? grid; // sent: 使用グリッド
  final String? ec; // sent: 使用 EC
  final String? note; // 受信統計など任意のメモ (vcode: 所要時間/スループット/カメラfps 等)

  HistoryItem({
    required this.id,
    required this.name,
    required this.type,
    required this.size,
    required this.tsMs,
    this.file,
    this.grid,
    this.ec,
    this.note,
  });

  DateTime get time => DateTime.fromMillisecondsSinceEpoch(tsMs);

  Map<String, dynamic> toJson() => {
        'id': id,
        'name': name,
        'type': type,
        'size': size,
        'tsMs': tsMs,
        if (file != null) 'file': file,
        if (grid != null) 'grid': grid,
        if (ec != null) 'ec': ec,
        if (note != null) 'note': note,
      };

  factory HistoryItem.fromJson(Map<String, dynamic> j) => HistoryItem(
        id: j['id'] as String,
        name: j['name'] as String? ?? 'data',
        type: j['type'] as String? ?? 'application/octet-stream',
        size: (j['size'] as num?)?.toInt() ?? 0,
        tsMs: (j['tsMs'] as num?)?.toInt() ?? 0,
        file: j['file'] as String?,
        grid: j['grid'] as String?,
        ec: j['ec'] as String?,
        note: j['note'] as String?,
      );
}

/// ローカル履歴 (オフライン)。受信成功と送信試行を JSON インデックスで管理。
class HistoryStore {
  HistoryStore._();

  /// テスト用。シングルトンとは別の、まっさらなインスタンス
  @visibleForTesting
  HistoryStore.forTest();
  static final HistoryStore instance = HistoryStore._();

  late Directory _dir;
  File get _index => File('${_dir.path}/history.json');
  Directory get _received => Directory('${_dir.path}/received');

  final List<HistoryItem> received = [];
  final List<HistoryItem> sent = [];

  /// 履歴が変わるたびに増える (UI の再描画トリガ)。
  final ValueNotifier<int> revision = ValueNotifier<int>(0);

  /// [dir] はテスト用 (省略時はアプリのサポートディレクトリ)。
  Future<void> init({Directory? dir}) async {
    _dir = dir ?? await getApplicationSupportDirectory();
    if (!await _received.exists()) await _received.create(recursive: true);
    await _load();
    // 索引に載っていない受信ファイルを拾い直す (索引が壊れて消えた履歴の復旧)
    if (await _recoverOrphans() > 0) await _persist();
    revision.value++;
  }

  /// 索引を読む。以前は 1 か所でも読めないと全件を黙って捨て、次の保存でその空の状態を
  /// 書き込んでいた (2026-11-25 に受信履歴が全部消えた)。読めない項目だけを飛ばし、
  /// 索引そのものが壊れていたら消さずに別名で残す。
  Future<void> _load() async {
    if (!await _index.exists()) return;
    final Map<String, dynamic> j;
    try {
      j = jsonDecode(await _index.readAsString()) as Map<String, dynamic>;
    } catch (e) {
      final keep = '${_index.path}.broken-${DateTime.now().millisecondsSinceEpoch}';
      debugPrint('[history] 索引が読めないので $keep に退避: $e');
      try {
        await _index.rename(keep);
      } catch (_) {}
      return;
    }
    List<HistoryItem> items(String key) {
      final out = <HistoryItem>[];
      for (final e in j[key] as List? ?? const []) {
        try {
          out.add(HistoryItem.fromJson(e as Map<String, dynamic>));
        } catch (_) {/* この 1 件だけ飛ばす */}
      }
      return out;
    }

    received
      ..clear()
      ..addAll(items('received'));
    sent
      ..clear()
      ..addAll(items('sent'));
  }

  /// received/ にあるのに索引に載っていないファイルを履歴に戻す。名前と種別は
  /// 分からないので、中身から種別を推定し、保存した時刻 (ID = マイクロ秒) から名前を付ける。
  Future<int> _recoverOrphans() async {
    final known = received.map((e) => e.file).toSet();
    var n = 0;
    await for (final f in _received.list()) {
      if (f is! File) continue;
      final id = f.uri.pathSegments.last;
      final rel = 'received/$id';
      final us = int.tryParse(id);
      if (known.contains(rel) || us == null) continue;
      final size = await f.length();
      if (size == 0) continue;
      final raf = await f.open();
      final head = await raf.read(4096);
      await raf.close();
      final t = DateTime.fromMicrosecondsSinceEpoch(us);
      final (name, mime) = resolveReceivedName(head, now: t);
      received.add(HistoryItem(
        id: id,
        name: name.replaceFirst('vcode_', '復旧_'),
        type: mime,
        size: size,
        tsMs: t.millisecondsSinceEpoch,
        file: rel,
        note: '履歴の索引から消えていたファイルを復旧 (元の名前は不明)',
      ));
      n++;
    }
    if (n > 0) {
      received.sort((a, b) => b.tsMs.compareTo(a.tsMs));
      debugPrint('[history] 索引に無い受信ファイル $n 件を復旧');
    }
    return n;
  }

  String _newId() => DateTime.now().microsecondsSinceEpoch.toString();

  /// 保存は 1 つずつ順番に行う。以前は同時に 2 回呼ばれると (受信の完了が 2 回走った)、
  /// 「空にしてから書く」が重なって索引が壊れた。
  Future<void> _writing = Future.value();

  Future<void> _persist() {
    final j = jsonEncode({
      'received': received.map((e) => e.toJson()).toList(),
      'sent': sent.map((e) => e.toJson()).toList(),
    });
    return _writing = _writing.then((_) async {
      // 一時ファイルに書いてから置き換える (書いている途中で落ちても元の索引が残る)
      final tmp = File('${_index.path}.tmp');
      await tmp.writeAsString(j, flush: true);
      await tmp.rename(_index.path);
    });
  }

  /// ストリーミング受信用: 出力先パスを確保 (実データはここへ直接書き込む)。
  ({String id, String path}) reserveReceivedPath() {
    final id = _newId();
    return (id: id, path: '${_received.path}/$id');
  }

  /// 受信完了を履歴に登録 (実データは reserveReceivedPath のパスに書き込み済み)。
  /// note には受信統計 (所要時間/スループット等) を残せる。
  Future<void> registerReceived(String id, String name, String type, int size,
      {String? note}) async {
    received.insert(
      0,
      HistoryItem(
          id: id,
          name: name,
          type: type,
          size: size,
          tsMs: DateTime.now().millisecondsSinceEpoch,
          file: 'received/$id',
          note: note),
    );
    await _persist();
    revision.value++;
  }

  Future<void> addSent(String name, String type, int size, String grid, String ec) async {
    sent.insert(
      0,
      HistoryItem(
          id: _newId(),
          name: name,
          type: type,
          size: size,
          tsMs: DateTime.now().millisecondsSinceEpoch,
          grid: grid,
          ec: ec),
    );
    await _persist();
    revision.value++;
  }

  /// 受信データのファイル (画像は Image.file でストリーム表示、テキストは小さい時だけ読む)。
  File? receivedFile(HistoryItem item) =>
      item.file == null ? null : File('${_dir.path}/${item.file}');

  Future<Uint8List?> readReceived(HistoryItem item) async {
    final f = receivedFile(item);
    if (f == null || !await f.exists()) return null;
    return f.readAsBytes();
  }
}
