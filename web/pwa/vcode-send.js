// vcode 送信: ペイロード全体を VcodeTx で符号化し、フレームをキャンバスに循環表示する。
// 形式はアプリ版・デスクトップ版と同じ (Rust コアを WASM で共有)。

import { VcodeTx } from "./pkg/vloom_core_wasm.js";
import { fmtSize, fmtElapsed } from "./format.js";

/** リペアパケットの比率 (ソース比)。アプリ・デスクトップの既定と同値 */
const REPAIR_RATE = 0.5;

/** bpc ごとの RaptorQ packet_size (Layout::BLOCK=20 前提で block_payload_len - 4) */
export const packetSizeFor = (bpc) => (bpc === 2 ? 92 : 42);

/** 1 フレームは 2 リフレッシュ周期表示する必要があるため、60Hz 画面での fps 上限 */
export const REFRESH_SAFE_FPS = 30;

// 送信ステージの余白 (物理画素)。マーカーの外側に白が要る (受信の環/余白の対比検査)。
// 画面の外は黒い縁なので、画面いっぱいには描かない。
//
// 以前はここをセル数 (片側 6 セル) で取っていた。倍率を上げると余白も比例して太るため、
// 整数へ切り捨てた時点で 1 セルぶん丸ごと失う。窓 502x943 では理想 1.85px/セル が
// 1px/セル に落ち、面積の 71% を捨てて「読めないコードを黙って出す」状態になっていた。
// 余白の役目はマーカーの外の白なので、画素で最低限を確保すれば足りる。
const TX_MARGIN_PX = 16;
// ただし 1 セルが大きく写る構成では、画素の下限だけだと相対的に細くなりすぎる。
// セル数でも下限を置く (元は 6 セル固定。半分に減らすぶんは実機で確かめること)。
const TX_MARGIN_CELLS_MIN = 3;

// 送信側の 1 セルがこれを下回ると、受信側は何をしても読めない。カメラは情報を
// 増やせないので、近づいても解像度を上げても回復しない (格子を粗くするか、
// 表示を大きくするしかない)。受信側の下限 3px/セル と同じ値。
const TX_MIN_PX_PER_CELL = 3;

/** 選べる格子 (密度の高い順)。窓に収まる最密のものを勧めるのに使う */
const TX_GRIDS = ["13x18", "18x13", "11x14", "11x10", "9x8", "7x6", "5x4"];

/** 格子からコード全体のセル数 [幅, 高さ] (Rust の Layout::width/height と同じ) */
function txLayoutCells(grid) {
  const [gw, gh] = grid.split("x").map(Number);
  return [gw * 20, gh * 20 + 56];
}

/** 余白 (片側, 物理画素) を決めて、収まる倍率を返す。
 *  余白はセル数にも依存するので、画素の下限で一度求めてから 1 度だけ効かせ直す。 */
function txFit(canvasW, canvasH, cw, ch) {
  const fit = (m) =>
    Math.max(0, Math.min((canvasW - 2 * m) / cw, (canvasH - 2 * m) / ch));
  const first = fit(TX_MARGIN_PX);
  return fit(Math.max(TX_MARGIN_PX, first * TX_MARGIN_CELLS_MIN));
}

/** 表示領域 (物理画素) に対して、その格子が何 px/セル で描けるか */
export function txPxPerCell(canvasW, canvasH, grid) {
  let [cw, ch] = txLayoutCells(grid);
  // 画面とコードの縦横が食い違うなら 90 度回して描く (_rotated と同じ判定)
  if ((canvasW > canvasH) !== (cw > ch)) [cw, ch] = [ch, cw];
  return txFit(canvasW, canvasH, cw, ch);
}

/** この表示領域で TX_MIN_PX_PER_CELL を満たす最密の格子 (無ければ null) */
export function txBestGrid(canvasW, canvasH) {
  return TX_GRIDS.find((g) => txPxPerCell(canvasW, canvasH, g) >= TX_MIN_PX_PER_CELL) || null;
}

export class VcodeSender {
  constructor({ canvas, onStatus }) {
    this.canvas = canvas;
    this.ctx = canvas.getContext("2d");
    this.onStatus = onStatus;
    this.off = document.createElement("canvas");
    this.running = false;
    this.seq = 0;
    // 表示の大きさ (収まる最大に対する %) と静止 (同じフレームを出し続ける)
    this.sizePct = 100;
    this.hold = false;
    this.shownIdx = -1;
    this.dirty = false;
    this.cssW = 0; this.cssH = 0;
    this.wakeLock = null;
    document.addEventListener("visibilitychange", () => {
      if (document.visibilityState === "visible" && this.running) this._acquireWakeLock();
    });
  }

  async start(fileOrBytes, gridStr, bpc, fps) {
    const payload = fileOrBytes instanceof Uint8Array
      ? fileOrBytes
      : new Uint8Array(await fileOrBytes.arrayBuffer());
    const [gw, gh] = gridStr.split("x").map(Number);
    const packetSize = packetSizeFor(bpc);
    const sourcePackets = Math.ceil(payload.length / packetSize);
    const extraRepair = Math.ceil(sourcePackets * REPAIR_RATE);
    const tx = new VcodeTx(payload, extraRepair, gw, gh, bpc);
    this.tx = tx;
    this.w = tx.frameWidth();
    this.h = tx.frameHeight();
    this.frameCount = tx.frameCount();
    this.off.width = this.w;
    this.off.height = this.h;
    this.fps = fps;
    this.payloadLen = payload.length;
    this.t0 = undefined;
    this.shownIdx = -1;
    this.dirty = true;
    this.running = true;
    const mySeq = ++this.seq;
    this.fit(this.cssW, this.cssH);
    await this._acquireWakeLock();
    requestAnimationFrame((t) => this._tick(mySeq, t));
  }

  /** 表示領域 (CSS px) を与えて、キャンバスの裏バッファを端末の物理画素に合わせる。
   *  以前は 1080x1080 固定の裏バッファを CSS で拡縮していて、セルの境界が画素に
   *  またがってぼけていた (スマホは DPR 3 なので特に)。裏バッファを物理画素に
   *  合わせたうえで、そこへ最近傍で描く。 */
  fit(cssW, cssH) {
    this.cssW = cssW; this.cssH = cssH;
    if (!cssW || !cssH) return;
    const { canvas } = this;
    const dpr = window.devicePixelRatio || 1;
    const W = Math.round(cssW * dpr), H = Math.round(cssH * dpr);
    if (canvas.width !== W || canvas.height !== H) { canvas.width = W; canvas.height = H; }
    canvas.style.width = `${cssW}px`;
    canvas.style.height = `${cssH}px`;
    this.dirty = true;
  }

  setSizePct(pct) { this.sizePct = pct; this.dirty = true; }
  setHold(on) { this.hold = on; this.dirty = true; }

  /** 現在の描画情報 (診断表示用): 1 セルの物理画素数と画面上の大きさ。
   *  読めない大きさでしか描けていないときは、その場で対処を出す。 */
  info() {
    if (!this.w) return "";
    const px = this._cellPx();
    const geom =
      `${this.w}×${this.h} セル · ${px.toFixed(1)}px/セル · ` +
      `${Math.round(this.w * px)}×${Math.round(this.h * px)}px` +
      (this._rotated() ? " · 90°回転" : "");
    if (px >= TX_MIN_PX_PER_CELL) return geom;
    // カメラは情報を増やせないので、この状態では受信側は何をしても読めない
    const { canvas } = this;
    const best = txBestGrid(canvas.width, canvas.height);
    const need = Math.ceil((TX_MIN_PX_PER_CELL / px) * 100);
    return `${geom}\n⚠ この大きさでは受信できません (${TX_MIN_PX_PER_CELL}px/セル 必要)。` +
      (best
        ? `ウィンドウを ${need}% に広げるか、格子を ${best.replace("x", "×")} にしてください。`
        : "ウィンドウを大きくしてください (今の大きさではどの格子も足りません)。");
  }

  /** 画面とコードの縦横が食い違うとき (横向きの端末で縦長コード) は 90° 回して描く。
   *  大きく描けるだけでなく、画面の書き換え方向 (端末の上→下) がコードに対して横になり、
   *  縦持ちの受信カメラのローリングシャッター (上→下) と直交する。平行だと切り替えの
   *  混ざりが画面の半分に及んだ (iPhone 12 Pro → Pixel 9a、縦持ち同士で実測)。 */
  _rotated() {
    const { canvas } = this;
    return (canvas.width > canvas.height) !== (this.w > this.h);
  }

  /** 1 セルあたりの物理画素数。整数に丸めない。
   *
   *  以前は「1 セル = 整数画素」に切り捨てていた (セル境界が画素をまたぐぼけを避けるため)。
   *  だが実機 A/B では、整数 2.00px/セル と端数 2.32px/セル の実効スループットが
   *  133.0 対 133.2 KB/s で並んだ (2026-09-07)。丸めて得られる分は測れないのに、
   *  丸めて失う分は理想倍率の落ち方しだいで面積の 7 割に達する。だから丸めない。 */
  _cellPx() {
    const { canvas } = this;
    const [cw, ch] = this._rotated() ? [this.h, this.w] : [this.w, this.h];
    const max = txFit(canvas.width, canvas.height, cw, ch);
    return Math.max(0.1, max * this.sizePct / 100);
  }

  _drawFrame(i) {
    const gray = this.tx.frameGray(i);
    const octx = this.off.getContext("2d");
    const id = octx.createImageData(this.w, this.h);
    for (let p = 0; p < this.w * this.h; p++) {
      const v = gray[p];
      id.data[p * 4] = v; id.data[p * 4 + 1] = v; id.data[p * 4 + 2] = v; id.data[p * 4 + 3] = 255;
    }
    octx.putImageData(id, 0, 0);

    const { ctx, canvas } = this;
    ctx.fillStyle = "#fff";
    ctx.fillRect(0, 0, canvas.width, canvas.height);
    ctx.imageSmoothingEnabled = false;
    // 整数倍で中央に置く (物理画素に揃う)。回すときは中心を整数座標にして 90° 回転
    const px = this._cellPx();
    const dw = this.w * px, dh = this.h * px;
    if (this._rotated()) {
      const cx = (canvas.width / 2) | 0, cy = (canvas.height / 2) | 0;
      ctx.save();
      ctx.translate(cx, cy);
      ctx.rotate(Math.PI / 2);
      ctx.drawImage(this.off, 0, 0, this.w, this.h, -((dw / 2) | 0), -((dh / 2) | 0), dw, dh);
      ctx.restore();
    } else {
      const dx = ((canvas.width - dw) / 2) | 0, dy = ((canvas.height - dh) / 2) | 0;
      ctx.drawImage(this.off, 0, 0, this.w, this.h, dx, dy, dw, dh);
    }
  }

  /** requestAnimationFrame 駆動。経過時間から出すべきフレーム番号を決めるので、
   *  setTimeout の遅れが積み上がらず、切り替えが画面の書き換え (vsync) に揃う。
   *  60Hz で 20fps なら 3 回の書き換えごとに 1 フレーム。 */
  _tick(mySeq, now) {
    if (!this.running || mySeq !== this.seq) return;
    if (this.t0 === undefined) this.t0 = now;
    const interval = 1000 / this.fps;
    // rAF の時刻は切り替え時刻よりわずかに早く来ることがあるので 1/4 間隔だけ前倒しで判定
    const want = this.hold && this.shownIdx >= 0
      ? this.shownIdx
      : Math.floor((now - this.t0) / interval + 0.25);
    if (want !== this.shownIdx || this.dirty) {
      // 巡ごとに 1 フレームずらして渡す (デスクトップ送信の _frame_arg と同じ)。
      // 毎巡同じ並びだと、受信側の間引きの位相が揃ったときに同じフレームを
      // 落とし続け、同じブロック位置が読めないときもそのパケットは何巡しても届かない
      // (ゲージが半分を過ぎてから極端に遅くなる)。ずらせば同じ位置が別のパケットを運ぶ
      this._drawFrame((want % this.frameCount) + Math.floor(want / this.frameCount));
      this.shownIdx = want;
      this.dirty = false;
      const pass = Math.floor(want / this.frameCount) + 1;
      // 一方向なので受信できたかは分からないが、経過と 1 巡の長さは出せる
      // (受信側は最低 1 巡ぶん見ていれば全パケットを一度は目にする)
      const loop = this.frameCount / this.fps;
      this.onStatus(
        `${this.hold ? "静止" : "送信中"} · ${fmtSize(this.payloadLen)} · ` +
        `${fmtElapsed((now - this.t0) / 1000)} · ${pass} 巡目 (1 巡 ${loop.toFixed(1)} 秒)`);
    }
    requestAnimationFrame((t) => this._tick(mySeq, t));
  }

  // 画面の自動消灯を止める (iOS Safari 16.4+ / Android Chrome)。失敗しても送信は続ける
  async _acquireWakeLock() {
    try {
      if (navigator.wakeLock && !this.wakeLock) {
        this.wakeLock = await navigator.wakeLock.request("screen");
        this.wakeLock.addEventListener("release", () => { this.wakeLock = null; });
      }
    } catch (_) { /* 非対応・省電力モードなど */ }
  }

  stop() {
    this.seq++;
    this.running = false;
    if (this.wakeLock) { this.wakeLock.release().catch(() => {}); this.wakeLock = null; }
  }
}
