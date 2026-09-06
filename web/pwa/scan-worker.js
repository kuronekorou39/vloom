// 受信の走査を別スレッドで回すワーカー。
//
// 走査はメインスレッドで回っていたが、1 枚あたり
//   drawImage → getImageData → RGBA→輝度 → wasm へコピー → 探索・復号
// と 200 万画素を何度もなめるので、UI の描画と取り合う。スマホのブラウザでは
// これで進捗も表示されず「固まったまま終わらない」ように見えていた。
// アプリ側は同じ理由でスキャンを別 isolate に出している (app/lib/scan_worker.dart)。
//
// 復号 (FountainDecoder) もこちらに置く。パケットを毎フレーム主スレッドへ渡す必要が
// なくなり、境界を越えるのは進捗の数値と、完了時のペイロードだけになる。
//
// 入力は 2 経路:
//   1. MediaStreamTrackProcessor の readable (Chromium)。VideoFrame から輝度 (Y) を
//      直接取れるので、RGBA を経由しない。主スレッドは 1 枚も触らない
//   2. 主スレッドが送ってくる輝度バッファ (上が使えないブラウザ向けの退避経路)

import init, {
  VcodeRx, FountainDecoder, vcodeUnwrapPayload,
} from "./pkg/vloom_core_wasm.js";

/** vcode のブロック一辺と上下ストリップ (Rust の Layout と一致させる) */
const BLOCK = 20;
const STRIP = 28;
/** 走査に載せたい px/セル。これを超えるぶんは縮小してよい (vcode.js と同値) */
const TARGET_PX_PER_CELL = 3.6;
/** 1 枚あたりの走査画素数の上限 */
const MAX_PIXELS = 2560 * 1440;
/** スキャナに渡すガイド枠幅 (中央正方形クロップの幅に対する比) */
const GUIDE_FRAC = 0.8;
/** 進捗を主スレッドへ送る間隔 */
const REPORT_MS = 250;

const layoutCells = (grid) => {
  const [gw, gh] = grid.split("x").map(Number);
  return [gw * BLOCK, gh * BLOCK + 2 * STRIP];
};

const state = {
  rx: null,
  dec: null,
  grid: "auto",
  running: false,
  frames: 0,
  detected: 0,
  blocks: 0,
  blocksTotal: 0,
  seenEsi: new Set(),
  needed: 0,
  scanW: 0,
  scanH: 0,
  mean: 0,
  sat: 0,
  fps: 0,
  _fpsFrames: 0,
  _fpsSince: 0,
  _lastReport: 0,
  canvas: null,
  ctx: null,
};

/** "auto" のときは候補のうち最も密なもの (13x18) を基準にする */
const activeGrid = () => (state.grid === "auto" ? "13x18" : state.grid);

/** 走査に使う縮小率。選んだ格子が要求する px/セル を満たすところまでしか縮めない */
function scanScale(w, h) {
  const [cw, ch] = layoutCells(activeGrid());
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
  if (n) {
    state.mean = sum / n;
    state.sat = sat / n;
  }
}

function report(force = false) {
  const now = performance.now();
  if (!force && now - state._lastReport < REPORT_MS) return;
  state._lastReport = now;
  self.postMessage({
    k: "progress",
    frames: state.frames,
    detected: state.detected,
    blocks: state.blocks,
    blocksTotal: state.blocksTotal,
    distinct: state.seenEsi.size,
    needed: state.needed,
    scanW: state.scanW,
    scanH: state.scanH,
    mean: state.mean,
    sat: state.sat,
    fps: state.fps,
  });
}

/** 輝度プレーン 1 枚を走査してデコーダへ入れる。完了したら true */
function scanGray(gray, w, h, stride) {
  state.frames++;
  state.scanW = w;
  state.scanH = h;
  state._fpsFrames++;
  const now = performance.now();
  if (now - state._fpsSince >= 500) {
    state.fps = (state._fpsFrames * 1000) / (now - state._fpsSince);
    state._fpsFrames = 0;
    state._fpsSince = now;
  }
  luma(gray);

  let rep;
  try {
    rep = state.rx.scan(gray, w, h, stride, 0, GUIDE_FRAC);
  } catch (_) {
    report();
    return false;
  }
  if (!rep.detected) {
    state.blocks = 0;
    state.blocksTotal = 0;
    report();
    return false;
  }
  state.detected++;
  state.blocks = rep.blocksOk;
  state.blocksTotal = rep.blocksTotal;
  if (!state.dec) {
    try {
      state.dec = new FountainDecoder(rep.oti);
    } catch (_) {
      report();
      return false;
    }
  }
  const n = rep.packetCount();
  let done = false;
  for (let i = 0; i < n; i++) {
    const pkt = rep.packet(i);
    // RaptorQ の payload ID = SBN(1) + ESI(3, big-endian)。単一ソースブロック前提で
    // ESI を「重複を除いた被覆」として数える (進捗と停滞判定に使う)
    if (pkt.length >= 4) state.seenEsi.add((pkt[1] << 16) | (pkt[2] << 8) | pkt[3]);
    if (state.dec.addPacket(pkt)) {
      done = true;
      break;
    }
  }
  if (!state.needed && n > 0) {
    const symbol = rep.packet(0).length - 4;
    if (symbol > 0) state.needed = Math.ceil(Number(state.dec.payloadSize()) / symbol);
  }
  if (!done) {
    report();
    return false;
  }
  // エンドツーエンド CRC-32 検証。不一致 = 復元結果が破損しているので、
  // デコーダと被覆を捨てて受信を続ける
  const payload = vcodeUnwrapPayload(state.dec.payload());
  if (!payload) {
    state.dec = null;
    state.seenEsi.clear();
    state.needed = 0;
    self.postMessage({ k: "integrity" });
    report(true);
    return false;
  }
  report(true);
  self.postMessage({ k: "done", payload }, [payload.buffer]);
  state.running = false;
  return true;
}

/** VideoFrame から輝度を取り出す。等倍なら I420 の Y 面をそのまま使う */
async function grayFromFrame(frame) {
  const w = frame.displayWidth, h = frame.displayHeight;
  const scale = scanScale(w, h);
  if (scale > 0.99) {
    // Y 面を直接読む。RGBA を経由しないので 1 枚あたり 3 回ぶんのなめが消える。
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
  if (!state.canvas || state.canvas.width !== tw || state.canvas.height !== th) {
    state.canvas = new OffscreenCanvas(tw, th);
    state.ctx = state.canvas.getContext("2d", { willReadFrequently: true });
  }
  state.ctx.drawImage(frame, 0, 0, w, h, 0, 0, tw, th);
  const rgba = state.ctx.getImageData(0, 0, tw, th).data;
  const gray = new Uint8Array(tw * th);
  for (let p = 0, q = 0; p < gray.length; p++, q += 4) {
    gray[p] = (rgba[q] * 77 + rgba[q + 1] * 150 + rgba[q + 2] * 29) >> 8;
  }
  return { gray, w: tw, h: th, stride: tw };
}

/** MediaStreamTrackProcessor の readable から取り続ける */
async function pump(readable) {
  const reader = readable.getReader();
  while (state.running) {
    let res;
    try {
      res = await reader.read();
    } catch (_) {
      break;
    }
    if (res.done) break;
    const frame = res.value;
    try {
      const g = await grayFromFrame(frame);
      if (scanGray(g.gray, g.w, g.h, g.stride)) break;
    } catch (e) {
      self.postMessage({ k: "error", msg: String(e && e.message ? e.message : e) });
    } finally {
      frame.close();
    }
  }
  try {
    reader.cancel();
  } catch (_) { /* 既に閉じている */ }
}

function setGrid(grid) {
  state.grid = grid;
  if (!state.rx) return;
  if (grid === "auto") {
    state.rx.setLayout(0, 0);
  } else {
    const [gw, gh] = grid.split("x").map(Number);
    state.rx.setLayout(gw, gh);
  }
}

self.onmessage = async (e) => {
  const m = e.data;
  try {
    switch (m.k) {
      case "init":
        await init();
        state.rx = new VcodeRx();
        state.dec = null;
        state.seenEsi = new Set();
        state.needed = 0;
        state.frames = 0;
        state.detected = 0;
        state.running = true;
        state._fpsSince = performance.now();
        setGrid(m.grid || "auto");
        self.postMessage({ k: "ready" });
        break;
      case "grid":
        setGrid(m.grid);
        break;
      case "stream":
        pump(m.readable);
        break;
      case "frame":
        // 退避経路: 主スレッドが作った輝度バッファ。
        // 受け取ったことは必ず返す — 進捗 (progress) は 250ms に間引いてあるので、
        // それを次の 1 枚の合図に使うと、間引かれた回で送信が止まってしまう
        scanGray(new Uint8Array(m.gray), m.w, m.h, m.stride);
        self.postMessage({ k: "ack" });
        break;
      case "stop":
        state.running = false;
        break;
      default:
        break;
    }
  } catch (err) {
    self.postMessage({ k: "error", msg: String(err && err.message ? err.message : err) });
  }
};
