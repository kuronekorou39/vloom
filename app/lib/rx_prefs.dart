import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:path_provider/path_provider.dart';

/// 受信画面の永続設定 (今はプリセットだけ)。
///
/// 「アプリを手で起動すると既定の格子に戻っていて、送信側と合わず一生掴めない」が
/// 実測中に繰り返し起きたので、最後に選んだプリセットを覚える。カメラの解像度を
/// 決めるので、画面が組み上がる前 (main) に読む。
class RxPrefs {
  static int? presetIndex;
  static File? _file;

  static Future<void> load() async {
    try {
      final dir = await getApplicationSupportDirectory();
      _file = File('${dir.path}/rx_prefs.json');
      if (await _file!.exists()) {
        final m = jsonDecode(await _file!.readAsString());
        final i = m['preset'];
        if (i is int) presetIndex = i;
      }
    } catch (e) {
      debugPrint('[rx-prefs] load failed: $e');
    }
  }

  static void savePreset(int i) {
    presetIndex = i;
    final f = _file;
    if (f == null) return;
    f.writeAsString(jsonEncode({'preset': i})).catchError((Object e) {
      debugPrint('[rx-prefs] save failed: $e');
      return f;
    });
  }
}
