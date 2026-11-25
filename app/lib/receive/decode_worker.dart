import 'dart:async';
import 'dart:isolate';

import 'package:flutter/foundation.dart';

import '../src/rust/api/fountain.dart';
import '../src/rust/api/vcode.dart';
import '../src/rust/frb_generated.dart';

/// 復元 (fountain 符号の復号) を別 isolate で回す。
///
/// 1 ブロック (約 5 万シンボル) の復元計算はスマホで 1 回 2〜3 秒かかる。30MB なら
/// 15 ブロックぶんが受信の最後 (99% 付近) にまとまって走り、UI isolate でやると
/// その間カメラの画像も処理されず、画面が止まってピントの制御まで崩れ、完了しなかった
/// (2026-11-25)。デコーダはこの isolate の中だけに置き、パケットは投げっぱなしで渡す。
///
/// 復元できたら CRC-32 (wrap_payload) の検証までここで済ませ、[onDone] に中身を返す。
/// CRC が合わなければ [onCorrupt] を呼ぶ (呼び出し側はワーカーを作り直して受信を続ける)。
class DecodeWorker {
  DecodeWorker._(this._isolate, this._toWorker, this._fromWorker);

  final Isolate _isolate;
  final SendPort _toWorker;
  final ReceivePort _fromWorker;
  bool _closed = false;

  /// [oti] (12 バイト) でデコーダを作る。宣言サイズが不正なら null。
  static Future<DecodeWorker?> spawn(
    Uint8List oti, {
    required void Function(Uint8List payload) onDone,
    required VoidCallback onCorrupt,
  }) async {
    final ready = ReceivePort();
    final isolate = await Isolate.spawn(_workerMain, [ready.sendPort, oti]);
    final first = await ready.first;
    ready.close();
    if (first is! SendPort) {
      // デコーダを作れなかった (細工された OTI など)
      isolate.kill(priority: Isolate.immediate);
      debugPrint('[decode-worker] start failed: $first');
      return null;
    }
    final from = ReceivePort();
    final w = DecodeWorker._(isolate, first, from);
    first.send(['attach', from.sendPort]);
    from.listen((msg) {
      if (w._closed) return;
      final list = msg as List;
      switch (list[0]) {
        case 'done':
          onDone((list[1] as TransferableTypedData).materialize().asUint8List());
        case 'corrupt':
          onCorrupt();
      }
    });
    return w;
  }

  /// 1 枚ぶんのパケットを渡す (待たない)。
  void feed(List<Uint8List> packets) {
    if (_closed || packets.isEmpty) return;
    _toWorker.send(['feed', packets]);
  }

  void dispose() {
    if (_closed) return;
    _closed = true;
    _fromWorker.close();
    _isolate.kill(priority: Isolate.immediate);
  }
}

Future<void> _workerMain(List<Object> args) async {
  final ready = args[0] as SendPort;
  final oti = args[1] as Uint8List;
  // Rust ブリッジはこの isolate でも初期化が要る (共有ライブラリ自体はプロセスで 1 つ)
  await RustLib.init();
  final FountainDecoder dec;
  try {
    dec = FountainDecoder(otiBytes: oti);
  } catch (e) {
    ready.send('$e');
    return;
  }
  final inbox = ReceivePort();
  ready.send(inbox.sendPort);
  SendPort? reply;
  var finished = false;
  await for (final msg in inbox) {
    final list = msg as List;
    if (list[0] == 'attach') {
      reply = list[1] as SendPort;
      continue;
    }
    if (finished) continue;
    for (final p in (list[1] as List).cast<Uint8List>()) {
      if (!dec.addPacket(packet: p)) continue;
      finished = true;
      // エンドツーエンド CRC-32 検証。不一致 = ゴミパケットが RaptorQ を汚染して
      // 復元結果が破損 → 呼び出し側がデコーダを作り直す
      final payload = vcodeUnwrapPayload(payload: dec.payload()!);
      if (payload == null) {
        reply?.send(['corrupt']);
      } else {
        reply?.send(['done', TransferableTypedData.fromList([payload])]);
      }
      break;
    }
  }
}
