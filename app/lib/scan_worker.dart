import 'dart:async';
import 'dart:isolate';

import 'package:flutter/foundation.dart';

import 'src/rust/api/vcode.dart';
import 'src/rust/frb_generated.dart';

/// スキャンを別 isolate で回す。
///
/// UI isolate は、カメラ画像をプラットフォームから受け取る処理だけで 1 枚あたり
/// 20ms 前後使う (YUV420 の 3 面、約 2.9MB の受け渡し)。同じ isolate でスキャン
/// (30ms) まで直列にやると 1 枚 50〜55ms = 毎秒 16〜18 枚で頭打ちになり、カメラの
/// 29fps の半分しか処理できなかった (実測)。受け取りと処理を別 isolate に分ければ
/// 両方が重なり、カメラの供給速度まで処理できる。
///
/// Y 面は TransferableTypedData で渡す (isolate 間の移送はコピーなし)。
/// VcodeRx (Rust 側の追従状態) はワーカーの中だけに置き、UI からはこの窓口を通す。
class ScanWorker {
  ScanWorker._(this._isolate, this._toWorker, this._fromWorker);

  final Isolate _isolate;
  final SendPort _toWorker;
  final ReceivePort _fromWorker;
  final Map<int, Completer<Object?>> _pending = {};
  int _seq = 0;
  bool _disposed = false;

  static Future<ScanWorker> spawn() async {
    final ready = ReceivePort();
    final isolate = await Isolate.spawn(_workerMain, ready.sendPort);
    final from = ReceivePort();
    // 最初のメッセージはワーカー側の受信ポート。以降は結果が流れてくる
    final toWorker = await ready.first as SendPort;
    ready.close();
    final w = ScanWorker._(isolate, toWorker, from);
    toWorker.send(['attach', from.sendPort]);
    from.listen(w._onReply);
    return w;
  }

  void _onReply(dynamic msg) {
    final list = msg as List;
    final id = list[0] as int;
    final c = _pending.remove(id);
    if (c == null) return;
    if (list[1] == 'error') {
      c.completeError(StateError(list[2] as String));
    } else {
      c.complete(list[1]);
    }
  }

  Future<T> _call<T>(String kind, List<Object?> args) {
    if (_disposed) return Future.error(StateError('ScanWorker disposed'));
    final id = _seq++;
    final c = Completer<Object?>();
    _pending[id] = c;
    _toWorker.send([kind, id, ...args]);
    return c.future.then((v) => v as T);
  }

  Future<VcodeScanReport> scan({
    required TransferableTypedData y,
    required int width,
    required int height,
    required int stride,
    required int rotationDeg,
    required double guideFrac,
    required bool debugDump,
  }) =>
      _call('scan', [y, width, height, stride, rotationDeg, guideFrac, debugDump]);

  Future<VcodeAcquireReport> acquire({
    required TransferableTypedData y,
    required int width,
    required int height,
    required int stride,
    required int rotationDeg,
    required bool thorough,
  }) =>
      _call('acquire', [y, width, height, stride, rotationDeg, thorough]);

  Future<void> setLayout({required int gridW, required int gridH}) =>
      _call('setLayout', [gridW, gridH]);

  Future<void> seed({
    required int rot,
    required int gridW,
    required int gridH,
    required List<double> corners,
  }) =>
      _call('seed', [rot, gridW, gridH, corners]);

  /// 追従状態を捨てて作り直す (受信のやり直し)
  Future<void> reset() => _call('reset', []);

  void dispose() {
    _disposed = true;
    for (final c in _pending.values) {
      c.completeError(StateError('ScanWorker disposed'));
    }
    _pending.clear();
    _fromWorker.close();
    _isolate.kill(priority: Isolate.immediate);
  }
}

Future<void> _workerMain(SendPort ready) async {
  final inbox = ReceivePort();
  ready.send(inbox.sendPort);
  // Rust ブリッジはこの isolate でも初期化が要る (共有ライブラリ自体はプロセスで 1 つ)
  await RustLib.init();
  var rx = VcodeRx();
  SendPort? reply;
  await for (final msg in inbox) {
    final list = msg as List;
    final kind = list[0] as String;
    if (kind == 'attach') {
      reply = list[1] as SendPort;
      continue;
    }
    final id = list[1] as int;
    try {
      Object? result;
      switch (kind) {
        case 'scan':
          final y = (list[2] as TransferableTypedData).materialize().asUint8List();
          result = rx.scanSync(
            y: y,
            width: list[3] as int,
            height: list[4] as int,
            stride: list[5] as int,
            rotationDeg: list[6] as int,
            guideFrac: list[7] as double,
            debugDump: list[8] as bool,
          );
        case 'acquire':
          final y = (list[2] as TransferableTypedData).materialize().asUint8List();
          result = await rx.acquire(
            y: y,
            width: list[3] as int,
            height: list[4] as int,
            stride: list[5] as int,
            rotationDeg: list[6] as int,
            thorough: list[7] as bool,
          );
        case 'setLayout':
          rx.setLayout(gridW: list[2] as int, gridH: list[3] as int);
        case 'seed':
          rx.seed(
            rot: list[2] as int,
            gridW: list[3] as int,
            gridH: list[4] as int,
            corners: (list[5] as List).cast<double>(),
          );
        case 'reset':
          rx = VcodeRx();
        default:
          throw StateError('unknown message: $kind');
      }
      reply?.send([id, result]);
    } catch (e, st) {
      debugPrint('[scan-worker] $kind failed: $e\n$st');
      reply?.send([id, 'error', '$e']);
    }
  }
}
