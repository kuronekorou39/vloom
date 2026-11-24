import 'dart:typed_data';

/// 先頭バイトからファイル種別を推定する。(拡張子, MIME) を返す。
/// 送信側がファイル名/MIME ヘッダを付けなかった (古い送信側・テキスト直送) ときに使う。
(String, String) sniffFileType(Uint8List b) {
  if (b.length > 3 && b[0] == 0xFF && b[1] == 0xD8) return ('jpg', 'image/jpeg');
  if (b.length > 7 && b[0] == 0x89 && b[1] == 0x50) return ('png', 'image/png');
  if (b.length > 11 &&
      b[8] == 0x57 &&
      b[9] == 0x45 &&
      b[10] == 0x42 &&
      b[11] == 0x50) {
    return ('webp', 'image/webp');
  }
  // ISO-BMFF (オフセット 4 に 'ftyp'): HEIC/AVIF (iOS 写真の既定形式)
  if (b.length > 11 &&
      b[4] == 0x66 &&
      b[5] == 0x74 &&
      b[6] == 0x79 &&
      b[7] == 0x70) {
    final brand = String.fromCharCodes(b.sublist(8, 12));
    if (const {
      'heic',
      'heix',
      'hevc',
      'heim',
      'heis',
      'mif1',
      'msf1',
    }.contains(brand)) {
      return ('heic', 'image/heic');
    }
    if (brand == 'avif' || brand == 'avis') return ('avif', 'image/avif');
  }
  if (b.length > 3 &&
      b[0] == 0x25 &&
      b[1] == 0x50 &&
      b[2] == 0x44 &&
      b[3] == 0x46) {
    return ('pdf', 'application/pdf');
  }
  if (b.length > 1 && b[0] == 0x50 && b[1] == 0x4B) {
    return ('zip', 'application/zip');
  }
  // 先頭 4KB の制御文字率でテキスト判定
  final probe = b.take(4096);
  final ctrl = probe
      .where((c) => c < 9 || (c > 13 && c < 32) || c == 127)
      .length;
  if (probe.isNotEmpty && ctrl / probe.length < 0.02) {
    return ('txt', 'text/plain;charset=utf-8');
  }
  return ('bin', 'application/octet-stream');
}

/// 受信データの保存名と MIME を決める。ヘッダの値を優先し、空なら内容から推定して
/// タイムスタンプ名 (vcode_2026-11-24T10-00-00.jpg) にする。
(String, String) resolveReceivedName(
  Uint8List data, {
  String headerName = '',
  String headerMime = '',
  DateTime? now,
}) {
  final ts = (now ?? DateTime.now())
      .toIso8601String()
      .replaceAll(':', '-')
      .substring(0, 19);
  final sniff = sniffFileType(data);
  return (
    headerName.isNotEmpty ? headerName : 'vcode_$ts.${sniff.$1}',
    headerMime.isNotEmpty ? headerMime : sniff.$2,
  );
}
