import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:vloom/history_store.dart';

void main() {
  late Directory dir;
  setUp(() => dir = Directory.systemTemp.createTempSync('vloom_hist'));
  tearDown(() => dir.deleteSync(recursive: true));

  test('同時に保存しても索引が壊れない', () async {
    final s = HistoryStore.forTest();
    await s.init(dir: dir);
    await Future.wait([
      for (var i = 0; i < 5; i++) s.registerReceived('id$i', 'f$i.txt', 'text/plain', i),
    ]);
    final j = jsonDecode(File('${dir.path}/history.json').readAsStringSync()) as Map;
    expect((j['received'] as List).length, 5);
  });

  test('壊れた索引は消さずに退避し、索引に無い受信ファイルを拾い直す', () async {
    Directory('${dir.path}/received').createSync();
    // 2026-11-25 10:22 に保存された 30MB 相当のファイル (中身は zip の先頭)
    final us = DateTime(2026, 11, 25, 10, 22).microsecondsSinceEpoch;
    File('${dir.path}/received/$us').writeAsBytesSync([0x50, 0x4B, 3, 4, 1, 2, 3]);
    File('${dir.path}/history.json').writeAsStringSync('{"received": [{"id": "x", "na'); // 途中で切れた
    final s = HistoryStore.forTest();
    await s.init(dir: dir);
    expect(s.received.length, 1);
    expect(s.received.first.name, '復旧_2026-11-25T10-22-00.zip');
    expect(s.received.first.size, 7);
    expect(dir.listSync().any((f) => f.path.contains('history.json.broken-')), isTrue);
    // 復旧した内容で索引が作り直されている
    final j = jsonDecode(File('${dir.path}/history.json').readAsStringSync()) as Map;
    expect((j['received'] as List).length, 1);
  });

  test('読めない項目だけを飛ばす', () async {
    File('${dir.path}/history.json').writeAsStringSync(jsonEncode({
      'received': [
        {'id': 'a', 'name': 'ok.txt', 'size': 1, 'tsMs': 0},
        {'name': 'id が無い'},
      ],
    }));
    final s = HistoryStore.forTest();
    await s.init(dir: dir);
    expect(s.received.map((e) => e.name), ['ok.txt']);
  });
}
