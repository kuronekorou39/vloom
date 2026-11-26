import 'dart:convert' show utf8;
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:vloom/receive/detected_code.dart';
import 'package:vloom/receive/file_sniff.dart';
import 'package:vloom/receive/focus_hunter.dart';
import 'package:vloom/receive/rx_speed.dart';
import 'package:vloom/receive/rx_stats.dart';

void main() {
  group('RxSpeedMeter', () {
    final t0 = DateTime(2026, 11, 26, 12);

    test('直近の幅だけで速さを出す (掴むまでの時間や昔の速さを引きずらない)', () {
      final m = RxSpeedMeter(window: const Duration(seconds: 4));
      expect(m.bytesPerSec, isNull);
      // 最初の 10 秒は毎秒 100KB、その後は毎秒 400KB
      var bytes = 0;
      for (var s = 0; s <= 20; s++) {
        m.add(t0.add(Duration(seconds: s)), bytes);
        bytes += (s < 10 ? 100 : 400) * 1024;
      }
      expect(m.bytesPerSec! / 1024, closeTo(400, 1));
      expect(m.eta(4000 * 1024)!.inSeconds, 10);
    });

    test('詰まった呼び出しは間引く・止まっていれば残り時間は出さない', () {
      final m = RxSpeedMeter();
      m.add(t0, 0);
      m.add(t0.add(const Duration(milliseconds: 30)), 999999); // 間隔が短いので捨てる
      expect(m.bytesPerSec, isNull);
      m.add(t0.add(const Duration(seconds: 1)), 0);
      expect(m.bytesPerSec, 0);
      expect(m.eta(1000), isNull);
    });

    test('残り時間の書き方', () {
      expect(fmtEta(const Duration(seconds: 2)), 'まもなく');
      expect(fmtEta(const Duration(seconds: 42)), '約 42 秒');
      expect(fmtEta(const Duration(seconds: 118)), '約 2 分');
      expect(fmtEta(const Duration(seconds: 133)), '約 2 分 10 秒');
    });
  });

  group('FocusHunter', () {
    const total = 100;
    const good = 80, bad = 10;

    /// 検出枚数を数えながら ok を n 枚流し、出た操作を集める
    List<FocusAction> feed(FocusHunter h, int ok, int n, List<int> detected) {
      final out = <FocusAction>[];
      for (var i = 0; i < n; i++) {
        detected[0]++;
        final a = h.update(ok, total, framesDetected: detected[0]);
        if (a != null) out.add(a);
      }
      return out;
    }

    test('据え置き (ずっと合っている) なら固定しない', () {
      final h = FocusHunter();
      final d = [0];
      expect(feed(h, good, 500, d), isEmpty);
      expect(h.hunts, 0);
    });

    test('探り直しを 2 往復見たら、合った瞬間に固定し、外れたら解除する', () {
      final h = FocusHunter();
      final d = [0];
      feed(h, good, kFocusSettleFrames + kFocusStateFrames, d);
      for (var c = 0; c < kFocusHuntCycles - 1; c++) {
        feed(h, bad, kFocusStateFrames, d);
        expect(feed(h, good, kFocusStateFrames, d), isEmpty);
      }
      feed(h, bad, kFocusStateFrames, d);
      expect(h.hunts, kFocusHuntCycles);
      expect(feed(h, good, kFocusStateFrames, d), [FocusAction.lock]);
      expect(h.locked, isTrue);
      expect(feed(h, bad, kFocusRelockBadFrames, d), [FocusAction.unlock]);
      expect(h.locked, isFalse);
    });

    test('開始直後の低い回収率は探り直しと数えない', () {
      final h = FocusHunter();
      final d = [0];
      feed(h, good, kFocusStateFrames, d);
      feed(h, bad, kFocusStateFrames, d);
      expect(h.hunts, 0);
    });
  });

  group('file_sniff', () {
    test('先頭バイトで種別を当てる', () {
      expect(sniffFileType(Uint8List.fromList([0xFF, 0xD8, 0xFF, 0xE0])).$1, 'jpg');
      expect(sniffFileType(Uint8List.fromList(utf8.encode('%PDF-1.7'))).$1, 'pdf');
      expect(sniffFileType(Uint8List.fromList(utf8.encode('こんにちは'))).$1, 'txt');
      expect(sniffFileType(Uint8List.fromList([0, 1, 2, 3, 4])).$1, 'bin');
    });

    test('ヘッダの名前を優先し、無ければ日時名にする', () {
      final png = Uint8List.fromList([0x89, 0x50, 0x4E, 0x47, 0, 0, 0, 0]);
      expect(resolveReceivedName(png, headerName: 'a.png', headerMime: 'image/png'),
          ('a.png', 'image/png'));
      final (name, mime) =
          resolveReceivedName(png, now: DateTime(2026, 11, 24, 9, 5, 7));
      expect(name, 'vcode_2026-11-24T09-05-07.png');
      expect(mime, 'image/png');
    });
  });

  group('RxStats', () {
    test('分布・失敗内訳・30 枚ごとの timing', () {
      final s = RxStats();
      String? timing;
      for (var i = 0; i < 30; i++) {
        final det = i.isEven;
        s.addScan(scanMs: 10, detected: det, rotateUs: 1000, decodeUs: 2000);
        if (det) {
          s.addDetected(tracked: true, ok: i < 10 ? 234 : 0, total: 234);
        } else {
          s.addMiss('rot90/13x18:CornerMismatch');
        }
        timing = s.addFrameTime(12) ?? timing;
      }
      expect(s.framesSeen, 30);
      expect(s.framesDetected, 15);
      expect(s.histText(), '0:10(67%)  満点:5(33%)');
      expect(s.errorKindsText(), 'CornerMismatch:15(100%)');
      expect(s.avgRustMs, closeTo(3.0, 1e-9));
      expect(timing, startsWith('total=12.0ms scan(frb)=10.0ms'));
      expect(s.addFrameTime(12), isNull); // 区間はリセットされている
    });
  });

  group('DetectedCode', () {
    // 1000x1000 の画像の中央 50% にある 100x100 セルのコード
    final code = DetectedCode(
      corners: [250, 250, 750, 250, 750, 750, 250, 750],
      imgW: 1000,
      imgH: 1000,
      rot: 90,
      cellsW: 100,
      cellsH: 100,
    );

    test('外接矩形と px/セル', () {
      final b = code.previewBounds(90);
      expect(b.left, closeTo(0.25, 1e-9));
      expect(b.width, closeTo(0.5, 1e-9));
      expect(code.pxPerCell, closeTo(5.0, 1e-9));
      expect(code.guideHint(90), isNull);
    });

    test('端に寄っていれば離す/寄せる誘導を出す', () {
      final edge = DetectedCode(
        corners: [5, 300, 600, 300, 600, 900, 5, 900],
        imgW: 1000,
        imgH: 1000,
        rot: 90,
        cellsW: 100,
        cellsH: 100,
      );
      expect(edge.guideHint(90), '少し離してください');
      final small = DetectedCode(
        corners: [400, 400, 600, 400, 600, 600, 400, 600],
        imgW: 1000,
        imgH: 1000,
        rot: 90,
        cellsW: 100,
        cellsH: 100,
      );
      expect(small.guideHint(90), 'もう少し近づけてください');
    });
  });
}
