import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import 'ui_common.dart';

/// 新しい版の入手口。
///
/// Vloom は「アプリ自体はネットワーク通信をしない」と約束している (プライバシーポリシー)。
/// そのため u2b-loop のようにアプリが GitHub に問い合わせて更新を知らせることはせず、
/// Releases の最新版のページをブラウザで開くだけにする (通信するのはブラウザ)。
/// 配布物は CI がタグ (v*) でリリース鍵署名した APK なので、そこから入れれば上書きできる。
class UpdateService {
  static const releasesUrl = 'https://github.com/kuronekorou39/vloom/releases/latest';
  static const _ch = MethodChannel('app.vloom/launch');

  /// APK を配る仕組みなので Android だけ (iOS は Sideloadly で入れ直す)
  static bool get supported => Platform.isAndroid;

  /// 端末に入っているアプリの版 (versionName)。取れなければ null
  static Future<String?> currentVersion() async {
    if (!supported) return null;
    try {
      return await _ch.invokeMethod<String>('version');
    } catch (_) {
      return null;
    }
  }

  /// いまの版を示して、最新版のページをブラウザで開くか尋ねる。
  static Future<void> openReleases(BuildContext context, String? current) async {
    final go = await showDialog<bool>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: const Text('新しい版を確認'),
        content: Text(
          '${current == null ? '' : 'いま入っているのは v$current です。\n'}'
          'ブラウザで最新版のページを開きます。新しい版があれば、そこにある '
          'app-release.apk をダウンロードして開くと上書きで更新できます。\n\n'
          'Vloom のアプリ自体は通信しません (確認もブラウザで行います)。',
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(ctx, false),
            child: const Text('やめる'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(ctx, true),
            child: const Text('ブラウザで開く'),
          ),
        ],
      ),
    );
    if (go != true) return;
    try {
      await _ch.invokeMethod('openUrl', {'url': releasesUrl});
    } catch (_) {
      if (context.mounted) {
        showToast(context, 'ブラウザを開けませんでした', kind: ToastKind.error);
      }
    }
  }
}
