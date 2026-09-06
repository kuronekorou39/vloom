// 受信の走査を回すワーカー。1 つのページから複数立てて、1 枚ずつ配って並列に処理する。
//
// 走査はもともとメインスレッドで回っていたが、1 枚あたり
//   drawImage → getImageData → RGBA→輝度 → wasm へコピー → 探索・復号
// と 200 万画素を何度もなめるので、UI の描画と取り合う。スマホのブラウザでは
// これで進捗も表示されず「固まったまま終わらない」ように見えていた。
//
// 別スレッドに出したうえで、なお 1 枚 170ms かかっていた (実機 Pixel 9a で 5.9 fps)。
// ネイティブアプリは同じ処理を rayon で 8 コアに分けて 140〜150 KB/s 出しているので、
// 差はほぼ並列度。wasm のスレッド (SharedArrayBuffer) は COOP/COEP ヘッダが要り
// GitHub Pages では付けられないため、**フレーム単位**で並列にする。
// ワーカーはそれぞれ独立した wasm と追従状態を持ち、自分に回ってきた枚だけを見る。
//
// このワーカーは復号 (FountainDecoder) を持たない。回収したパケットを返すだけで、
// 積み上げは呼び出し側が 1 つのデコーダにまとめる (ワーカーごとに持つと合流できない)。

import init, { VcodeRx } from "./pkg/vloom_core_wasm.js";

/** vcode のブロック一辺と上下ストリップ (Rust の Layout と一致させる) */
const BLOCK = 20;
const STRIP = 28;
/** 走査に載せたい px/セル。これを超えるぶんは縮小してよい (vcode.js と同値) */
const TARGET_PX_PER_CELL = 3.6;
/** 1 枚あたりの走査画素数の上限 */
const MAX_PIXELS = 2560 * 1440;
/** スキャナに渡すガイド枠幅 (中央正方形クロップの幅に対する比) */
const GUIDE_FRAC = 0.8;

let rx = null;
let grid = "auto";
let canvas = null;
let ctx = null;

const layoutCells = (g) => {
  const [gw, gh] = g.split("x").map(Number);
  return [gw * BLOCK, gh * BLOCK + 2 * STRIP];
};

/** 走査に使う縮小率。選んだ格子が要求する px/セル を満たすところまでしか縮めない */
function scanScale(w, h) {
  // "auto" は候補のうち最も密なもの (13x18) を基準にする
  const [cw, ch] = layoutCells(grid === "auto" ? "13x18" : grid);
  const have = Math.min((w * 0.88) / cw, (h * 0.88) / ch);
  const want = have > TARGET_PX_PER_CELL ? TARGET_PX_PER_CELL / have : 1;
  return Math.min(1, want, Math.sqrt(MAX_PIXELS / (w * h)));
}

/** 輝度の平均と飽和率 (露出制御の判断に使う)。64 画素おきの間引きで十分 */
function luma(gray) {
  let sum = 0, sat = 0, n = 0;
  for (let i = 0; i < gray.length; i += 64) {
    const v = gray[i];
    sum += v;
    if (v >= 250) sat++;
    n++;
  }
  return n ? { mean: sum / n, sat: sat / n } : { mean: 0, sat: 0 };
}

/** VideoFrame から輝度を取り出す。等倍なら I420 の Y 面をそのまま使う */
async function grayFromFrame(frame) {
  const w = frame.displayWidth, h = frame.displayHeight;
  const scale = scanScale(w, h);
  if (scale > 0.99) {
    // Y 面を直接読む。RGBA を経由しないので 1 枚あたり数回ぶんのなめが消える。
    // stride はスキャナがそのまま受け取れる (行あたりのバイト数がずれる端末対策)
    try {
      const size = frame.allocationSize({ format: "I420" });
      const buf = new Uint8Array(size);
      const layout = await frame.copyTo(buf, { format: "I420" });
      const y = layout[0];
      return { gray: buf.subarray(y.offset, y.offset + y.stride * h), w, h, stride: y.stride };
    } catch (_) {
      // I420 で取れない端末 (RGB のみのカメラ等) は下の描画経路へ落とす
    }
  }
  const tw = Math.max(1, Math.round(w * scale));
  const th = Math.max(1, Math.round(h * scale));
  if (!canvas || canvas.width !== tw || canvas.height !== th) {
    canvas = new OffscreenCanvas(tw, th);
    ctx = canvas.getContext("2d", { willReadFrequently: true });
  }
  ctx.drawImage(frame, 0, 0, w, h, 0, 0, tw, th);
  const rgba = ctx.getImageData(0, 0, tw, th).data;
  const gray = new Uint8Array(tw * th);
  for (let p = 0, q = 0; p < gray.length; p++, q += 4) {
    gray[p] = (rgba[q] * 77 + rgba[q + 1] * 150 + rgba[q + 2] * 29) >> 8;
  }
  return { gray, w: tw, h: th, stride: tw };
}

/** 輝度プレーン 1 枚を走査して、回収したパケットを返す */
function scanGray(gray, w, h, stride) {
  const { mean, sat } = luma(gray);
  let rep;
  try {
    rep = rx.scan(gray, w, h, stride, 0, GUIDE_FRAC);
  } catch (e) {
    return { detected: false, mean, sat, w, h, error: String(e) };
  }
  if (!rep.detected) return { detected: false, mean, sat, w, h };
  const n = rep.packetCount();
  const packets = [];
  for (let i = 0; i < n; i++) packets.push(rep.packet(i));
  return {
    detected: true, mean, sat, w, h,
    blocks: rep.blocksOk, blocksTotal: rep.blocksTotal,
    oti: rep.oti, packets,
  };
}

function setGrid(g) {
  grid = g;
  if (!rx) return;
  if (g === "auto") {
    rx.setLayout(0, 0);
  } else {
    const [gw, gh] = g.split("x").map(Number);
    rx.setLayout(gw, gh);
  }
}

/** 1 枚ぶんを処理して結果を返す。seq は呼び出し側が対応付けに使う通し番号 */
async function handleFrame(m) {
  let r;
  try {
    const g = m.frame
      ? await grayFromFrame(m.frame)
      : { gray: new Uint8Array(m.gray), w: m.w, h: m.h, stride: m.stride };
    r = scanGray(g.gray, g.w, g.h, g.stride);
  } catch (e) {
    r = { detected: false, error: String(e && e.message ? e.message : e) };
  } finally {
    // VideoFrame は明示的に閉じないとカメラのバッファが尽きて供給が止まる
    if (m.frame) m.frame.close();
  }
  r.k = "scanned";
  r.seq = m.seq;
  // パケットは転送 (transfer) せずコピーで返す。数 KB なので誤差だし、
  // wasm のメモリから切り出したビューを切り離すと危うい
  self.postMessage(r);
}

self.onmessage = async (e) => {
  const m = e.data;
  try {
    switch (m.k) {
      case "init":
        await init();
        rx = new VcodeRx();
        setGrid(m.grid || "auto");
        self.postMessage({ k: "ready" });
        break;
      case "grid":
        setGrid(m.grid);
        break;
      case "frame":
        await handleFrame(m);
        break;
      case "reset":
        // 追従状態を捨てる (受信のやり直し)
        rx = new VcodeRx();
        setGrid(grid);
        break;
      default:
        break;
    }
  } catch (err) {
    self.postMessage({
      k: "scanned", seq: m.seq, detected: false,
      error: String(err && err.message ? err.message : err),
    });
  }
};
