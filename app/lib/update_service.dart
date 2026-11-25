import 'dart:convert';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import 'launch_args.dart';
import 'ui_common.dart';

/// 入れられる新しい版
class UpdateOffer {
  const UpdateOffer({required this.version, required this.apkUrl, this.notes = ''});
  final String version; // v を除いたもの (0.5.0)
  final String apkUrl;
  final String notes; // リリースの本文 (先頭の数行だけ見せる)
}

/// GitHub Releases の最新版を見て、新しければ知らせる (u2b-loop と同じ流れ)。
///
/// 配布物は CI がタグ (v*) を打たれたときにリリース鍵で署名して Releases に上げる APK。
/// ダウンロードとインストールは OS (ブラウザとパッケージインストーラ) に任せる。
/// 端末のアプリも同じリリース鍵で署名されている必要がある (debug 鍵のビルドには
/// 上書きできない)。
class UpdateService {
  static const _repo = 'kuronekorou39/vloom';
  static const _timeout = Duration(seconds: 10);
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

  /// 更新があればダイアログを出す。[manual] (メニューから押したとき) は、無いときも
  /// 結果を知らせる。起動時の確認は、計測の自動化で起動されたときは行わない。
  static Future<void> check(BuildContext context, {bool manual = false}) async {
    if (!supported || (!manual && LaunchArgs.automated)) return;
    try {
      final current = await currentVersion();
      if (current == null) return;
      final offer = pickUpdate(await _fetchLatest(), current);
      if (!context.mounted) return;
      if (offer == null) {
        if (manual) showToast(context, '最新版です (v$current)');
        return;
      }
      _showDialog(context, offer, current);
    } catch (_) {
      // 起動時の確認は黙って諦める (オフラインで使うアプリなので、繋がらないのは普通)
      if (manual && context.mounted) {
        showToast(context, '更新を確認できませんでした', kind: ToastKind.error);
      }
    }
  }

  static Future<Map<String, dynamic>> _fetchLatest() async {
    final client = HttpClient()..connectionTimeout = _timeout;
    try {
      final req = await client
          .getUrl(Uri.parse('https://api.github.com/repos/$_repo/releases/latest'))
          .timeout(_timeout);
      req.headers.set('Accept', 'application/vnd.github+json');
      final res = await req.close().timeout(_timeout);
      if (res.statusCode != 200) throw HttpException('GitHub ${res.statusCode}');
      final body = await res.transform(utf8.decoder).join().timeout(_timeout);
      return jsonDecode(body) as Map<String, dynamic>;
    } finally {
      client.close();
    }
  }

  /// releases/latest の JSON から、[current] より新しく APK の付いた版を選ぶ。無ければ null
  static UpdateOffer? pickUpdate(Map<String, dynamic> release, String current) {
    if (release['draft'] == true || release['prerelease'] == true) return null;
    final tag = (release['tag_name'] as String? ?? '').replaceFirst(RegExp('^v'), '');
    if (compareVersions(tag, current) <= 0) return null;
    for (final a in release['assets'] as List<dynamic>? ?? const []) {
      if (a is Map && (a['name'] as String? ?? '').endsWith('.apk')) {
        final url = a['browser_download_url'] as String?;
        if (url == null) continue;
        return UpdateOffer(
          version: tag,
          apkUrl: url,
          notes: release['body'] as String? ?? '',
        );
      }
    }
    return null;
  }

  /// "0.4.1" 形式の版を比べる (a が新しければ正)。数字でない部分 ("-beta" など) は無視する
  static int compareVersions(String a, String b) {
    List<int> parts(String v) => v
        .split('-')
        .first
        .split('.')
        .map((s) => int.tryParse(s) ?? 0)
        .toList();
    final pa = parts(a), pb = parts(b);
    for (var i = 0; i < 3; i++) {
      final x = i < pa.length ? pa[i] : 0;
      final y = i < pb.length ? pb[i] : 0;
      if (x != y) return x.compareTo(y);
    }
    return 0;
  }

  static void _showDialog(BuildContext context, UpdateOffer offer, String current) {
    final notes = offer.notes.trim().split('\n').take(6).join('\n');
    showDialog<void>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: const Text('アップデートがあります'),
        content: SingleChildScrollView(
          child: Text(
            'v${offer.version} が使えます (いまは v$current)。\n'
            'ダウンロードしてインストールしますか?'
            '${notes.isNotEmpty ? '\n\n$notes' : ''}',
          ),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(ctx),
            child: const Text('あとで'),
          ),
          FilledButton(
            onPressed: () {
              Navigator.pop(ctx);
              _ch.invokeMethod('openUrl', {'url': offer.apkUrl});
            },
            child: const Text('ダウンロード'),
          ),
        ],
      ),
    );
  }
}
