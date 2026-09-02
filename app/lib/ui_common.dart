// 受信結果まわりの共有 UI。トーストと、復元したファイルのプレビューを一箇所にまとめる。
//
// 保存は flutter_file_dialog (システムの保存ダイアログ) 経由なので、保存先の実パスは
// アプリに返ってこない。そのため「保存先を表示する」のではなく「開く」導線を出す。

import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:open_filex/open_filex.dart';
import 'package:path_provider/path_provider.dart';

/// トーストの種別。色とアイコンだけが変わる。
enum ToastKind { info, success, error }

/// アプリ共通のトースト。既定の SnackBar は素っ気ないので、
/// アイコン・色・角丸・floating を揃えて出す。
void showToast(
  BuildContext context,
  String message, {
  ToastKind kind = ToastKind.info,
  SnackBarAction? action,
}) {
  final scheme = Theme.of(context).colorScheme;
  final (Color color, IconData icon) = switch (kind) {
    ToastKind.success => (const Color(0xFF4CAF50), Icons.check_circle),
    ToastKind.error => (scheme.error, Icons.error_outline),
    ToastKind.info => (scheme.primary, Icons.info_outline),
  };
  ScaffoldMessenger.of(context)
    ..hideCurrentSnackBar()
    ..showSnackBar(
      SnackBar(
        behavior: SnackBarBehavior.floating,
        margin: const EdgeInsets.all(12),
        padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 12),
        backgroundColor: const Color(0xFF23262E),
        shape: RoundedRectangleBorder(
          borderRadius: BorderRadius.circular(12),
          side: BorderSide(color: color.withValues(alpha: 0.6)),
        ),
        duration: Duration(seconds: action != null ? 6 : 3),
        content: Row(
          children: [
            Icon(icon, color: color, size: 20),
            const SizedBox(width: 10),
            Expanded(
              child: Text(
                message,
                style: const TextStyle(color: Colors.white, fontSize: 14),
              ),
            ),
          ],
        ),
        action: action,
      ),
    );
}

/// 保存済みのファイルを既定アプリで開く。開けなければ理由をトーストで返す。
Future<void> openWithDefaultApp(BuildContext context, File file) async {
  final result = await OpenFilex.open(file.path);
  if (!context.mounted) return;
  if (result.type != ResultType.done) {
    showToast(context, '開けませんでした: ${result.message}', kind: ToastKind.error);
  }
}

/// 受信したファイル名を、パスとして安全な形に落とす。
/// 名前は送信側 (= 他人の画面) が決めるので、区切り文字・親ディレクトリ参照・制御文字を
/// そのまま使うとアプリのサンドボックス内の別ファイルを指せてしまう。
String safeFileName(String n) {
  final s = n
      .replaceAll(RegExp(r'[\\/:*?"<>|\x00-\x1f]'), '_')
      .replaceAll(RegExp(r'^\.+'), '_')
      .trim();
  return s.isEmpty ? 'file' : (s.length > 120 ? s.substring(0, 120) : s);
}

/// 復元したバイト列を一時ファイルに書き出して既定アプリで開く。
/// 端末に保存する前でも中身を確認できるようにするための導線。
Future<void> openBytesWithDefaultApp(
  BuildContext context,
  Uint8List bytes,
  String name,
) async {
  final dir = await getTemporaryDirectory();
  // 受信名は相手が決めるので、そのままパスに使わない (../ や / でサンドボックス内の
  // 別ファイルを書き換えられる)。区切り・制御文字を落としてから使う
  final file = File('${dir.path}/${safeFileName(name)}');
  await file.writeAsBytes(bytes, flush: true);
  if (!context.mounted) return;
  await openWithDefaultApp(context, file);
}

/// 復元したファイルのプレビュー。
///
/// 画像とテキストはその場で表示し、それ以外 (動画・PDF・Office 等) は種別を示して
/// 「アプリで開く」に誘導する。動画を内蔵再生するには追加の依存とライフサイクル管理が
/// 要るので、既定アプリに委ねている。
class ReceivedPreview extends StatelessWidget {
  const ReceivedPreview({
    super.key,
    required this.bytes,
    required this.mime,
    required this.name,
    this.maxHeight = 260,
  });

  final Uint8List bytes;
  final String mime;
  final String name;
  final double maxHeight;

  bool get _isImage => mime.startsWith('image/');
  bool get _isText => mime.startsWith('text/');
  bool get _isVideo => mime.startsWith('video/');
  bool get _isAudio => mime.startsWith('audio/');

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    Widget body;

    if (_isImage) {
      // HEIC など Flutter がデコードできない形式は errorBuilder で受ける
      body = Image.memory(
        bytes,
        fit: BoxFit.contain,
        errorBuilder: (_, _, _) =>
            _fallback(context, Icons.image_not_supported, 'この形式はアプリ内で表示できません'),
      );
    } else if (_isText) {
      body = Container(
        width: double.infinity,
        padding: const EdgeInsets.all(12),
        color: Colors.black.withValues(alpha: 0.25),
        child: SingleChildScrollView(
          child: Text(
            _decodeText(bytes),
            style: const TextStyle(fontSize: 13, height: 1.5),
          ),
        ),
      );
    } else {
      body = _fallback(
        context,
        _isVideo
            ? Icons.movie
            : _isAudio
            ? Icons.audiotrack
            : Icons.insert_drive_file,
        _isVideo || _isAudio
            ? '${_isVideo ? "動画" : "音声"}は「アプリで開く」で既定のプレイヤーで再生できます'
            : 'この種別はアプリで開けます',
      );
    }

    return Column(
      mainAxisSize: MainAxisSize.min,
      children: [
        ConstrainedBox(
          constraints: BoxConstraints(maxHeight: maxHeight),
          child: ClipRRect(
            borderRadius: BorderRadius.circular(10),
            child: body,
          ),
        ),
        const SizedBox(height: 8),
        Text(
          '$name · ${mime.isEmpty ? "種別不明" : mime}',
          style: TextStyle(fontSize: 11, color: scheme.onSurfaceVariant),
          textAlign: TextAlign.center,
        ),
      ],
    );
  }

  Widget _fallback(BuildContext context, IconData icon, String label) {
    final scheme = Theme.of(context).colorScheme;
    return Container(
      width: double.infinity,
      padding: const EdgeInsets.symmetric(vertical: 28),
      color: Colors.black.withValues(alpha: 0.25),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(icon, size: 44, color: scheme.onSurfaceVariant),
          const SizedBox(height: 8),
          Text(
            label,
            style: TextStyle(fontSize: 12, color: scheme.onSurfaceVariant),
          ),
        ],
      ),
    );
  }

  /// 先頭 8KB だけ UTF-8 として読む (巨大なテキストで固まらないように)。
  /// 途中で切ると末尾のマルチバイト文字が壊れるので allowMalformed で受ける。
  static String _decodeText(Uint8List bytes) {
    const limit = 8192;
    final head = bytes.length > limit ? bytes.sublist(0, limit) : bytes;
    final s = utf8.decode(head, allowMalformed: true);
    return bytes.length > limit ? '$s\n\n… (以降は省略)' : s;
  }
}
