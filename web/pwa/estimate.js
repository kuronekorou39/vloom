// 所要時間の目安。理論値ではなく、実機で測った実効スループットから出す。
//
// 理論値 (ブロック数 × packet_size × fps) は取りこぼしも RaptorQ の冗長も含まないので、
// そのまま時間に直すと必ず短く出る (13×18 @20fps は理論 192 KB/s、実測の中央値は 151.7)。
// 送信側は受信できたかを知る手段がない (一方向通信) ため、その場の実測も使えない。
// 残る根拠は docs/ に記録してある計測で、ここはその写し。
//
// 数字を更新するときは docs/experiments.md と docs/measurements/ の記録を先に足し、
// アプリ側の写し (app/lib/preset.dart の kMeasuredKbps) も揃えること。

import { packetSizeFor, REFRESH_SAFE_FPS } from "./vcode.js";

/**
 * 実効スループットの実測 (KB/s, 1bit)。値は中央値、fps は測ったときの送信 fps。
 * 受信は Pixel 9a・三脚固定。1 本ごとに ±15% ほどばらつく。
 *
 * app (ネイティブアプリで受信):
 *   5x4 / 7x6 / 9x8 / 11x10 — 2026-08-28 (BenQ, 各 2 本)
 *   11x14                   — 2026-08-30 格子の天井 (76.7 / 89.5 / 80.5)
 *   13x18 @20               — 2026-09-07 LCD 送信, 5 本 (143.6〜154.5)
 *   13x18 @30               — 2026-08-31 OLED 送信 (201〜207)
 * web (ブラウザで受信):
 *   13x18                   — 2026-09 Pages / Chrome, 2.0MB を 49.4 秒
 */
const MEASURED = {
  app: {
    "5x4": [{ fps: 20, kbps: 9.5 }],
    "7x6": [{ fps: 20, kbps: 22.0 }],
    "9x8": [{ fps: 20, kbps: 39.5 }],
    "11x10": [{ fps: 20, kbps: 60.0 }],
    "11x14": [{ fps: 20, kbps: 80.5 }],
    "13x18": [{ fps: 20, kbps: 151.7 }, { fps: 30, kbps: 204 }],
  },
  web: {
    "13x18": [{ fps: 20, kbps: 41.3 }],
  },
};

/** 設定の理論スループット (KB/s)。実測との比較基準 */
export function theoryKbps(grid, bpc, fps) {
  const [gw, gh] = grid.split("x").map(Number);
  return (gw * gh * packetSizeFor(bpc) * fps) / 1024;
}

/**
 * その設定で見込める実効スループット (KB/s)。実測が無い組み合わせは null。
 *
 * アプリ受信は送信 fps が律速なので、測ったときと fps が違えば最も近い実測から fps に
 * 比例させる。60Hz 画面の上限 (REFRESH_SAFE_FPS) を超えるぶんは伸びに数えない
 * (40fps は 30fps より遅かった)。
 * ブラウザ受信は走査の速さ (毎秒 7〜10 枚) が律速で、送信 fps を上げても増えない。
 * 測った fps より遅く送るときだけ比例で下げる。
 */
export function measuredKbps(receiver, grid, bpc, fps) {
  // 輝度 4 値は同じ格子の 1bit より遅く、密な格子では回収 0 になる。目安を出せない
  if (bpc !== 1) return null;
  const runs = MEASURED[receiver][grid];
  if (!runs) return null;
  const f = Math.min(fps, REFRESH_SAFE_FPS);
  const near = runs.reduce((a, b) => (Math.abs(b.fps - f) < Math.abs(a.fps - f) ? b : a));
  const ratio = f / near.fps;
  return near.kbps * (receiver === "web" ? Math.min(1, ratio) : ratio);
}

/**
 * 送るサイズと設定から、所要時間の目安を受信側の種類ごとに返す。
 * 実測が無い受信側は載せない (理論値で埋めると短く出て当てにならない)。
 */
export function estimate(bytes, grid, bpc, fps) {
  const out = [];
  for (const [receiver, label] of [["app", "アプリで受信"], ["web", "ブラウザで受信"]]) {
    const kbps = measuredKbps(receiver, grid, bpc, fps);
    if (kbps) out.push({ receiver, label, kbps, seconds: bytes / 1024 / kbps });
  }
  return out;
}
