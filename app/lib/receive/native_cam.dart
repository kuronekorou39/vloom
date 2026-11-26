import 'dart:async';
import 'dart:convert' show utf8;

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';

import '../src/rust/api/vcode.dart';

/// 走査 1 枚ぶんの結果と、そのときのカメラの状況。
class NativeScanEvent {
  const NativeScanEvent(this.camFrames, this.scanMs, this.report);

  /// カメラから届いた累計の枚数 (走査しなかった枚も含む)
  final int camFrames;

  /// この枚の走査にかかった時間 (コピー〜結果)
  final int scanMs;
  final VcodeScanReport report;
}

/// Android の受信用カメラ。カメラの画像は Dart に渡さず、ネイティブ側 (NativeRxCamera.kt →
/// app/rust/src/native_rx.rs) で走査して、結果だけをここに流す。
///
/// camera プラグイン経由だと画像 (数 MB) の受け渡しだけで UI isolate を 1 枚 20ms 使い、
/// 解像度も 1600x1200 までしか選べなかった。直結で 2560x1920 を毎秒 30 枚走査できる
/// (docs/measurements/2026-11-26-native-camera-probe.md)。
///
/// 走査の本数・追従の状態はネイティブ側が持つ。ここは ScanPool と同じ形の操作
/// (格子の指定・位置の種・やり直し・位置検出) を中継する。
class NativeCam {
  NativeCam._(this.textureId, this.width, this.height, this.sensorOrientation, this.textureRotated);

  static const _ch = MethodChannel('app.vloom/nativecam');
  static const _events = EventChannel('app.vloom/nativecam/reports');

  /// プレビューを描く Texture の ID
  final int textureId;

  /// 走査している画像の寸法 (センサーの向き = 横長)
  final int width;
  final int height;

  /// 画像を起こす回転。走査の結果 (四隅) はこの回転をかけた座標で返る
  final int sensorOrientation;

  /// Texture がすでに正しい向きで描かれるか。false ならプレビューを [sensorOrientation] ぶん回す
  final bool textureRotated;

  StreamSubscription<dynamic>? _sub;

  /// カメラを開いて走査を始める。[onScan] は走査 1 枚ごとに呼ばれる。
  /// [width] x [height] が使えない端末では、ネイティブ側が出せる最大の 4:3 を選ぶ。
  static Future<NativeCam> start({
    required void Function(NativeScanEvent) onScan,
    int width = 0,
    int height = 0,
    int fps = 30,
    int workers = 3,
    double ev = 0,
    bool meterCenter = true,
    int? exposureUs,
    int? iso,
  }) async {
    final m = await _ch.invokeMapMethod<String, dynamic>('start', {
      'width': width,
      'height': height,
      'fps': fps,
      'workers': workers,
      'ev': ev,
      'meterCenter': meterCenter,
      'exposureUs': exposureUs ?? 0,
      'iso': iso ?? 0,
    });
    final cam = NativeCam._(
      m!['textureId'] as int,
      m['width'] as int,
      m['height'] as int,
      m['sensorOrientation'] as int,
      m['textureRotated'] as bool,
    );
    cam._sub = _events.receiveBroadcastStream().listen((e) {
      try {
        onScan(_parseScan(e as Uint8List));
      } catch (err) {
        debugPrint('[native-cam] 走査結果を読めません: $err');
      }
    });
    return cam;
  }

  Future<void> stop() async {
    await _sub?.cancel();
    _sub = null;
    await _ch.invokeMethod<void>('stop');
  }

  /// 探索する格子を固定する (0 で自動)
  Future<void> setLayout({required int gridW, required int gridH}) =>
      _ch.invokeMethod<void>('setLayout', {'gridW': gridW, 'gridH': gridH});

  Future<void> seed({
    required int rot,
    required int gridW,
    required int gridH,
    required List<double> corners,
  }) => _ch.invokeMethod<void>('seed', {'rot': rot, 'gridW': gridW, 'gridH': gridH, 'corners': corners});

  /// 追従の状態を捨てる (格子の指定も消えるので入れ直すこと)
  Future<void> reset() => _ch.invokeMethod<void>('reset');

  /// 次に届く 1 枚で位置検出を走らせる。カメラを止めた・取り消されたときは null
  Future<VcodeAcquireReport?> acquire({required bool thorough}) async {
    final b = await _ch.invokeMethod<Uint8List>('acquire', {'thorough': thorough});
    return b == null ? null : _parseAcquire(b);
  }

  /// 走査をこの間隔に間引く (何も写っていない間の省電力)。[Duration.zero] で全部走査する
  Future<void> setScanInterval(Duration d) =>
      _ch.invokeMethod<void>('setScanInterval', {'ms': d.inMilliseconds});

  Future<void> lockExposure(bool lock) => _ch.invokeMethod<void>('lockExposure', {'lock': lock});

  /// いまのレンズ位置のままピントを固定する (フレームの供給は止まらない)
  Future<void> lockFocus(bool lock) => _ch.invokeMethod<void>('lockFocus', {'lock': lock});
}

/// native_rx.rs の Writer と対になる読み手 (リトルエンディアン)
class _Reader {
  _Reader(Uint8List b) : _b = b, _d = ByteData.sublistView(b);
  final Uint8List _b;
  final ByteData _d;
  int _p = 0;

  int u8() => _b[_p++];
  int u16() {
    final v = _d.getUint16(_p, Endian.little);
    _p += 2;
    return v;
  }

  int u32() {
    final v = _d.getUint32(_p, Endian.little);
    _p += 4;
    return v;
  }

  Uint8List take(int n) {
    // 元のバッファ (1 枚ぶんの結果全体) を掴み続けないようコピーする。パケットは
    // 復元ワーカーへ渡すまで保持されうる
    final v = Uint8List.fromList(Uint8List.sublistView(_b, _p, _p + n));
    _p += n;
    return v;
  }

  Uint8List bytes() => take(u32());

  Float32List floats() {
    final n = u8();
    final v = Float32List(n);
    for (var i = 0; i < n; i++) {
      v[i] = _d.getFloat32(_p, Endian.little);
      _p += 4;
    }
    return v;
  }

  List<bool> bools() {
    final n = u32();
    final v = List<bool>.generate(n, (i) => _b[_p + i] != 0, growable: false);
    _p += n;
    return v;
  }
}

NativeScanEvent _parseScan(Uint8List b) {
  final r = _Reader(b);
  final camFrames = r.u32();
  final scanMs = r.u32();
  final detected = r.u8() != 0;
  final tracked = r.u8() != 0;
  final frameSeq = r.u32();
  final blocksOk = r.u32();
  final blocksTotal = r.u32();
  final imgW = r.u32();
  final imgH = r.u32();
  final cellsW = r.u32();
  final cellsH = r.u32();
  final rot = r.u32();
  final rotateUs = r.u32();
  final decodeUs = r.u32();
  final oti = r.bytes();
  final corners = r.floats();
  final error = utf8.decode(r.bytes(), allowMalformed: true);
  final blockOk = r.bools();
  final n = r.u32();
  final packets = List<Uint8List>.generate(n, (_) => r.take(r.u16()), growable: false);
  return NativeScanEvent(
    camFrames,
    scanMs,
    VcodeScanReport(
      detected: detected,
      tracked: tracked,
      frameSeq: frameSeq,
      oti: oti,
      packets: packets,
      blocksOk: blocksOk,
      blocksTotal: blocksTotal,
      error: error.isEmpty ? null : error,
      corners: corners,
      imgW: imgW,
      imgH: imgH,
      cellsW: cellsW,
      cellsH: cellsH,
      blockOk: blockOk,
      rot: rot,
      rotateUs: rotateUs,
      decodeUs: decodeUs,
      debugW: 0,
      debugH: 0,
    ),
  );
}

VcodeAcquireReport _parseAcquire(Uint8List b) {
  final r = _Reader(b);
  final detected = r.u8() != 0;
  final gridW = r.u8();
  final gridH = r.u8();
  final rot = r.u32();
  final blocksOk = r.u32();
  final blocksTotal = r.u32();
  final imgW = r.u32();
  final imgH = r.u32();
  final cellsW = r.u32();
  final cellsH = r.u32();
  final corners = r.floats();
  final blockOk = r.bools();
  return VcodeAcquireReport(
    detected: detected,
    rot: rot,
    gridW: gridW,
    gridH: gridH,
    blocksOk: blocksOk,
    blocksTotal: blocksTotal,
    corners: corners,
    imgW: imgW,
    imgH: imgH,
    cellsW: cellsW,
    cellsH: cellsH,
    blockOk: blockOk,
  );
}
