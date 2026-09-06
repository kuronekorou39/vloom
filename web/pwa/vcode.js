// PC 用 vcode 送受信。スマホアプリの vcode と同形式:
//   - 送信: 生ペイロード全体を VcodeTx で符号化 (チャンク無し=単一ペイロード)、フレーム循環表示
//   - 受信: カメラ→輝度Y→VcodeRx.scan→パケット→FountainDecoder→生バイト→型sniff→保存
// 受信側で CANDIDATES に無い格子は検出できないため、格子は 7x6 / 5x4 のみ。

import { VcodeTx, VcodeRx, FountainDecoder, vcodeUnwrapPayload, vcodeUnwrapFile } from "./pkg/vloom_core_wasm.js";
import {
  openCamera, ExposureGuard, cameraInfoText, lumaText, cellPxText,
  gridTooDense, pxPerCell,
} from "./camera.js";

const REPAIR_RATE = 0.5;

// スキャナに渡すガイド枠幅 (中央正方形クロップの幅に対する比)。UI のガイド枠と一致させる。
// アプリ側 kVcodeGuideFrac と同値。
export const VCODE_GUIDE_FRAC = 0.8;
// 走査解像度は「選んだ格子が要求する px/セル」から決める。
//
// 以前は「長辺 1280px」の一律縮小だった。vcode は縦長 (13x18 で 260x416 セル) なので
// 効くのは短辺のほうで、1920x1080 のカメラは長辺基準だと 1280x720 に落ち、短辺 720px を
// 416 セルで割って 1.5px/セル — 3px/セル の下限をまったく満たさない。この設定では
// 「検出は毎フレーム成功するのに 234 ブロック中 55 しか回収できず、同じ 55 が延々と
// 返ってきて永久に復元できない」状態になっていた (擬似カメラで再現済み)。
//
// 縮小は処理を軽くするためのものなので、要求を満たしたところで止める。足りない映像を
// さらに縮めても読めなくなるだけなので、そのときは等倍で渡す。
const SCAN_TARGET_PX_PER_CELL = 3.6;
// 1 枚あたりの走査画素数の上限。走査はメインスレッドで回るので、これ以上広げると
// 1 枚の処理がフレーム間隔を超えて取りこぼしのほうが増える。
const SCAN_MAX_PIXELS = 2560 * 1440;
// 検出できているのに新しいパケットが増えない状態がこれだけ続いたら、原因を出す
const STALL_MS = 6000;
// 診断表示の更新間隔
const DIAG_INTERVAL_MS = 500;

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
      this._drawFrame(want % this.frameCount);
      this.shownIdx = want;
      this.dirty = false;
      const pass = Math.floor(want / this.frameCount) + 1;
      this.onStatus(`${this.hold ? "静止" : "送信中"} · ${this.payloadLen}B · frame ${want % this.frameCount + 1}/${this.frameCount} · ${pass} 巡目`);
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

export class VcodeReceiver {
  constructor({ video, onProgress, onDone, onError, onDiag }) {
    this.video = video;
    this.onProgress = onProgress;
    this.onDone = onDone;
    this.onError = onError;
    this.onDiag = onDiag || (() => {});
    this.cap = document.createElement("canvas");
    this.stream = null;
    this.rafId = null;
    this.workers = [];
    this.guideEl = null;
    this._onResize = () => this._positionGuide();
  }

  async start(deviceId, grid = "auto") {
    this._reset();
    this.grid = grid;
    this.stream = await openCamera(deviceId);
    this.exposure = new ExposureGuard(this.stream); // スマホ画面の白飛び対策
    this.video.srcObject = this.stream;
    await this.video.play();
    this._ensureGuide();
    this.video.addEventListener("loadedmetadata", this._onResize);
    window.addEventListener("resize", this._onResize);
    this._startWorkers();
  }

  /** 走査ワーカーの数。
   *
   *  1 枚 170ms (実機 Pixel 9a) では 1 本では 6 fps しか出ず、カメラの 30fps に
   *  まったく追いつかない。ワーカーはそれぞれ独立した wasm と追従状態を持つので、
   *  1 枚ずつ配れば枚単位で並列になる (SharedArrayBuffer も COOP/COEP も要らない)。
   *  実機 (Pixel 9a / Tensor G4 = 大1 + 中3 + 小4) で本数を振った結果、20 秒あたりの
   *  回収パケットは 1 本 13,223 / 2 本 21,289 / 3 本 23,969 / 4 本 21,068 / 6 本 21,698。
   *  1 -> 2 で +61%、3 で頭打ち、それ以上は増えない (速いコアの数で決まる)。
   *  増やすほどワーカーごとに wasm を持つぶんメモリと電力も食うので 3 で止める。
   *  掃引は後半ほど端末が温まるので、4 本以降がやや不利に出ている可能性はある。 */
  _workerCount() {
    // 実機で本数を振って決められるように localStorage で上書きできる
    // (localStorage.setItem("vloom.workers", "6") など)
    const forced = parseInt(localStorage.getItem("vloom.workers") || "", 10);
    if (forced >= 1 && forced <= 8) return forced;
    const cores = navigator.hardwareConcurrency || 4;
    return Math.max(1, Math.min(3, cores - 1));
  }

  /** 走査ワーカーの一団を起こす。理由は scan-worker.js の先頭を参照。 */
  _startWorkers() {
    const n = this._workerCount();
    this.workers = [];
    try {
      for (let i = 0; i < n; i++) {
        const w = new Worker(new URL("./scan-worker.js", import.meta.url), { type: "module" });
        w.busy = false;
        w.onmessage = (e) => this._onWorker(w, e.data);
        w.onerror = (err) => {
          console.warn("[vcode-rx] ワーカーが起動できないので主スレッドで走査する", err.message);
          this._fallbackToMainThread();
        };
        w.postMessage({ k: "init", grid: this.grid });
        this.workers.push(w);
      }
    } catch (e) {
      console.warn("[vcode-rx] ワーカーを作れないので主スレッドで走査する", e);
      this._fallbackToMainThread();
      return;
    }
    this.readyCount = 0;
  }

  _onWorker(w, m) {
    switch (m.k) {
      case "ready":
        w.ready = true;
        // 1 本でも動き出したら映像を流し始める (残りは追いついた順に加わる)
        if (++this.readyCount === 1) this._feedWorkers();
        break;
      case "scanned":
        w.busy = false;
        this._onScanned(m);
        break;
      default:
        break;
    }
  }

  /** 空いているワーカーを 1 本返す (無ければ null)。 */
  _idleWorker() {
    return (this.workers || []).find((w) => w.ready && !w.busy) || null;
  }

  /** カメラ映像をワーカーへ配る。VideoFrame を直接読める環境ならそちらを使う。 */
  _feedWorkers() {
    if (this.feeding) return;
    this.feeding = true;
    const track = this.stream && this.stream.getVideoTracks()[0];
    if (track && typeof MediaStreamTrackProcessor !== "undefined") {
      try {
        // プレビュー用の track とは別に読む (同じ track を 2 か所で消費しないよう複製)
        this.procTrack = track.clone();
        const proc = new MediaStreamTrackProcessor({ track: this.procTrack });
        this._pump(proc.readable);
        return;
      } catch (e) {
        console.warn("[vcode-rx] VideoFrame 経路が使えないので描画経路にする", e);
        if (this.procTrack) { this.procTrack.stop(); this.procTrack = null; }
      }
    }
    this._startPushLoop();
  }

  /** VideoFrame を読み続けて、空いているワーカーへ 1 枚ずつ渡す。
   *  全員ふさがっている枚は捨てる (溜めても古くなるだけで、閉じないと供給が止まる)。 */
  async _pump(readable) {
    const reader = readable.getReader();
    this.reader = reader;
    while (this.stream && !this.finished) {
      let res;
      try {
        res = await reader.read();
      } catch (_) {
        break;
      }
      if (res.done) break;
      const frame = res.value;
      const w = this._idleWorker();
      if (!w) { frame.close(); continue; }
      w.busy = true;
      this.frames++;
      w.postMessage({ k: "frame", seq: this.frames, frame }, [frame]);
    }
    try { reader.cancel(); } catch (_) { /* 既に閉じている */ }
  }

  /** 退避経路: 主スレッドで輝度に落としてワーカーへ送る (VideoFrame が使えない環境)。 */
  _startPushLoop() {
    const loop = () => {
      if (!this.stream || this.finished) return;
      this.rafId = requestAnimationFrame(loop);
      const w = this._idleWorker();
      if (!w) return;
      const g = this._grabGray();
      if (!g) return;
      w.busy = true;
      this.frames++;
      w.postMessage(
        { k: "frame", seq: this.frames, gray: g.gray.buffer, w: g.w, h: g.h, stride: g.w },
        [g.gray.buffer],
      );
    };
    this.rafId = requestAnimationFrame(loop);
  }

  /** ワーカーが返した 1 枚ぶんの結果を、1 つのデコーダに積む。
   *  デコーダをワーカー側に置くと、ワーカーごとに別々の集合になって合流できない。 */
  _onScanned(m) {
    if (this.finished) return;
    this._tickFps();
    if (m.w) { this.scanW = m.w; this.scanH = m.h; }
    if (typeof m.mean === "number") this.stats.mean = m.mean;
    if (typeof m.sat === "number") this.stats.sat = m.sat;
    this._setGuideLocked(!!m.detected);
    this._positionGuide();
    this._diag();
    if (!m.detected) { this.blocks = 0; this.blocksTotal = 0; this._progress(); return; }

    this.detected++;
    // 所要時間は「初検出 → 復元完了」で測る (カメラを向けるまでの時間を含めない)
    if (this.firstDetectedAt === null) this.firstDetectedAt = performance.now();
    this.blocks = m.blocks;
    this.blocksTotal = m.blocksTotal;
    if (!this.dec) {
      try { this.dec = new FountainDecoder(m.oti); } catch (_) { return; }
    }
    const tDec = performance.now();
    let done = false;
    for (const pkt of m.packets) {
      // RaptorQ の payload ID = SBN(1) + ESI(3, big-endian)。単一ソースブロック前提で
      // ESI を「重複を除いた被覆」として数える (進捗と停滞判定に使う)
      if (pkt.length >= 4) this.seenEsi.add((pkt[1] << 16) | (pkt[2] << 8) | pkt[3]);
      if (this.dec.addPacket(pkt)) { done = true; break; }
    }
    if (!this.needed && m.packets.length > 0) {
      const symbol = m.packets[0].length - 4;
      if (symbol > 0) this.needed = Math.ceil(Number(this.dec.payloadSize()) / symbol);
    }
    // 主スレッドでのデコーダ投入時間 (1 枚あたり)。ワーカーを増やしても伸びないとき、
    // ここが詰まっているのかを切り分ける
    this.decMs = this.decMs * 0.9 + (performance.now() - tDec) * 0.1;
    this.distinct = this.seenEsi.size;
    this._progress();
    if (!done) return;
    // エンドツーエンド CRC-32 検証。不一致 = 復元結果が破損 → デコーダを捨てて受信続行
    const payload = vcodeUnwrapPayload(this.dec.payload());
    if (!payload) {
      console.warn("[vcode-rx] 整合性エラー: 復元結果が破損。デコーダを作り直して受信続行");
      this.dec = null;
      this.seenEsi.clear();
      this.distinct = 0;
      this.lastDistinct = 0;
      this.lastGainAt = performance.now();
      return;
    }
    this._finish(payload);
  }

  /** 走査 fps の実測 (ワーカー全体の合計)。 */
  _tickFps() {
    this._fpsFrames = (this._fpsFrames || 0) + 1;
    const now = performance.now();
    if (!this._fpsSince) this._fpsSince = now;
    if (now - this._fpsSince >= 500) {
      this.stats.fps = (this._fpsFrames * 1000) / (now - this._fpsSince);
      this._fpsFrames = 0;
      this._fpsSince = now;
    }
  }

  /** 映像 1 枚を輝度バッファにする (退避経路と主スレッド走査で共用)。 */
  _grabGray() {
    const vw = this.video.videoWidth, vh = this.video.videoHeight;
    if (!vw || !vh) return null;
    const scale = this._scanScale(vw, vh);
    const tw = Math.round(vw * scale), th = Math.round(vh * scale);
    this.cap.width = tw; this.cap.height = th;
    const ctx = this.cap.getContext("2d", { willReadFrequently: true });
    ctx.drawImage(this.video, 0, 0, vw, vh, 0, 0, tw, th);
    const rgba = ctx.getImageData(0, 0, tw, th).data;
    const gray = new Uint8Array(tw * th);
    for (let p = 0, q = 0; p < gray.length; p++, q += 4) {
      gray[p] = (rgba[q] * 77 + rgba[q + 1] * 150 + rgba[q + 2] * 29) >> 8;
    }
    return { gray, w: tw, h: th };
  }

  /** ワーカーがまったく作れない環境向け (module worker 非対応など)。 */
  _fallbackToMainThread() {
    for (const w of this.workers) w.terminate();
    this.workers = [];
    if (this.rafId) cancelAnimationFrame(this.rafId);
    this.rx = new VcodeRx();
    this.setGrid(this.grid);
    this.dec = null;
    const loop = () => {
      if (!this.stream || this.finished) return;
      this._scanInline();
      this.rafId = requestAnimationFrame(loop);
    };
    this.rafId = requestAnimationFrame(loop);
  }

  _reset() {
    this.rx = null; this.dec = null; this.finished = false; this.frames = 0; this.detected = 0;
    this.stats = { fps: 0, mean: 0, sat: 0 };
    this._lastDiag = 0;
    this.firstDetectedAt = null;
    // 重複を除いた被覆 (ESI 集合) と、必要パケット数。進捗表示と停滞判定に使う
    this.distinct = 0;
    this.needed = 0;
    this.lastDistinct = 0;
    this.lastGainAt = performance.now();
    this.scanW = 0;
    this.scanH = 0;
    this.blocks = 0;
    this.blocksTotal = 0;
    // 被覆はワーカーをまたいで 1 つにまとめる (ワーカーごとに持つと合流できない)
    this.seenEsi = new Set();
    this.readyCount = 0;
    this.feeding = false;
    this.reader = null;
    this._fpsFrames = 0;
    this._fpsSince = 0;
    this.decMs = 0;
  }

  /** 探索する格子を切り替える ("auto" で候補総当たり)。受信中でも即反映する。 */
  setGrid(grid) {
    this.grid = grid;
    this._positionGuide();
    if (this.workers.length) {
      for (const w of this.workers) w.postMessage({ k: "grid", grid });
      return;
    }
    if (!this.rx) return;
    if (grid === "auto") {
      this.rx.setLayout(0, 0);
    } else {
      const [gw, gh] = grid.split("x").map(Number);
      this.rx.setLayout(gw, gh);
    }
  }

  // カメラ実解像度・スキャン fps・明るさ・理論 px/セル を出す。読めないときに
  // 「カメラが違う / 解像度不足 / 白飛び / コードが小さすぎ」を切り分けるための実測値。
  _diag() {
    const now = performance.now();
    if (now - this._lastDiag < DIAG_INTERVAL_MS) return;
    this._lastDiag = now;
    this.exposure.update(this.stats);
    this.onDiag(
      `${cameraInfoText(this.stream)}\n` +
      `${this.stats.fps.toFixed(1)} fps · 走査 ${this.workers.length} 本 · ` +
      `投入 ${this.decMs.toFixed(1)}ms · ${lumaText(this.stats, this.exposure)}\n` +
      cellPxText(this.scanW, this.scanH, this.grid)
    );
  }

  // スキャナが探索するボックスを映像に重ねて描く。ユーザーはこの枠にコードを収めれば、
  // スキャナのガイド初期値と一致して検出が始まる。
  _ensureGuide() {
    if (this.guideEl || !this.video.parentElement) return;
    const el = document.createElement("div");
    // z-index は映像 (#rxVideo は z-index:1) より上に。無いと枠が映像の下に潜って見えない。
    el.style.cssText =
      "position:absolute;z-index:2;box-sizing:border-box;pointer-events:none;border:3px solid #f59e0b;" +
      "border-radius:6px;box-shadow:0 0 0 9999px rgba(0,0,0,0.28);transition:border-color .12s;";
    this.video.parentElement.appendChild(el);
    this.guideEl = el;
    this._positionGuide();
  }

  _positionGuide() {
    if (!this.guideEl) return;
    // 映像は object-fit:cover で表示領域いっぱいに出す (外側は切れる)。
    const cw = this.video.clientWidth, ch = this.video.clientHeight;
    if (!cw || !ch) return;
    // 選択中の格子の縦横比で、表示領域に収まる最大枠 × GUIDE_FRAC (自動時は既定の 13x18)
    const g = this.grid === "auto" ? "13x18" : this.grid;
    const [gw2, gh2] = g.split("x").map(Number);
    const cellsW = gw2 * 20, cellsH = gh2 * 20 + 56; // 上下ストリップ 28 セル × 2
    const fit = Math.min(cw / cellsW, ch / cellsH) * VCODE_GUIDE_FRAC;
    const bw = cellsW * fit, bh = cellsH * fit;
    const s = this.guideEl.style;
    s.width = `${bw}px`; s.height = `${bh}px`;
    s.left = `${(cw - bw) / 2}px`; s.top = `${(ch - bh) / 2}px`;
  }

  _setGuideLocked(locked) {
    if (this.guideEl) this.guideEl.style.borderColor = locked ? "#22c55e" : "#f59e0b";
  }

  _removeGuide() {
    if (this.guideEl) { this.guideEl.remove(); this.guideEl = null; }
  }

  stop() {
    if (this.rafId) cancelAnimationFrame(this.rafId);
    this.rafId = null;
    for (const w of this.workers) w.terminate();
    this.workers = [];
    if (this.reader) { try { this.reader.cancel(); } catch (_) { /* 済 */ } this.reader = null; }
    if (this.procTrack) { this.procTrack.stop(); this.procTrack = null; }
    this.video.removeEventListener("loadedmetadata", this._onResize);
    window.removeEventListener("resize", this._onResize);
    this._removeGuide();
    if (this.stream) { this.stream.getTracks().forEach((t) => t.stop()); this.stream = null; }
  }

  /** 走査に使う縮小率。選んだ格子が要求する px/セル を満たすところまでしか縮めない。
   *  ワーカー側にも同じ判断がある (scan-worker.js の scanScale)。 */
  _scanScale(vw, vh) {
    // "auto" は候補の中で最も密なもの (13x18) を基準にする
    const grid = this.grid === "auto" ? "13x18" : this.grid;
    const have = pxPerCell(vw, vh, grid);
    // 要求より大きく写っていれば、その余剰ぶんだけ縮めて処理を軽くする
    const want = have > SCAN_TARGET_PX_PER_CELL ? SCAN_TARGET_PX_PER_CELL / have : 1;
    const budget = Math.sqrt(SCAN_MAX_PIXELS / (vw * vh));
    return Math.min(1, want, budget);
  }

  /** ワーカーが作れない環境向けの、主スレッド走査 1 枚ぶん。 */
  _scanInline() {
    const g = this._grabGray();
    if (!g) return;
    this.frames++;
    this.scanW = g.w;
    this.scanH = g.h;
    let sum = 0, sat = 0, n = 0;
    for (let i = 0; i < g.gray.length; i += 64) {
      const v = g.gray[i];
      sum += v;
      if (v >= 250) sat++;
      n++;
    }
    if (n) { this.stats.mean = sum / n; this.stats.sat = sat / n; }
    let rep;
    try {
      rep = this.rx.scan(g.gray, g.w, g.h, g.w, 0, VCODE_GUIDE_FRAC);
    } catch (_) { return; }
    this._setGuideLocked(rep.detected);
    this._positionGuide();
    this._diag();
    if (!rep.detected) { this.blocks = 0; this.blocksTotal = 0; this._progress(); return; }
    this.detected++;
    if (this.firstDetectedAt === null) this.firstDetectedAt = performance.now();
    this.blocks = rep.blocksOk;
    this.blocksTotal = rep.blocksTotal;
    if (!this.dec) {
      try { this.dec = new FountainDecoder(rep.oti); } catch (_) { return; }
    }
    const seen = this.seenEsi;
    const n2 = rep.packetCount();
    let done = false;
    for (let i = 0; i < n2; i++) {
      const pkt = rep.packet(i);
      // RaptorQ の payload ID = SBN(1) + ESI(3, big-endian)。重複を除いた被覆を数える
      if (pkt.length >= 4) seen.add((pkt[1] << 16) | (pkt[2] << 8) | pkt[3]);
      if (this.dec.addPacket(pkt)) { done = true; break; }
    }
    if (!this.needed && n2 > 0) {
      const symbol = rep.packet(0).length - 4;
      if (symbol > 0) this.needed = Math.ceil(Number(this.dec.payloadSize()) / symbol);
    }
    this.distinct = seen.size;
    this._progress();
    if (!done) return;
    // エンドツーエンド CRC-32 検証。不一致 = 復元結果が破損 → デコーダを捨てて受信続行
    const payload = vcodeUnwrapPayload(this.dec.payload());
    if (!payload) {
      console.warn("[vcode-rx] 整合性エラー: 復元結果が破損。デコーダを作り直して受信続行");
      this.dec = null;
      seen.clear();
      this.distinct = 0;
      this.lastDistinct = 0;
      this.lastGainAt = performance.now();
      return;
    }
    this._finish(payload);
  }

  /** 進捗と、進まないときの原因を UI へ返す。 */
  _progress() {
    if (this.distinct > this.lastDistinct) {
      this.lastDistinct = this.distinct;
      this.lastGainAt = performance.now();
    }
    // 検出できているのにパケットが増えない = 同じ一部のブロックだけを拾い続けている。
    // 原因はほぼ px/セル 不足 (密すぎる格子) なので、必要な値と対処を名指しで出す。
    let stall = null;
    if (this.detected > 0 && performance.now() - this.lastGainAt > STALL_MS) {
      stall =
        gridTooDense(this.scanW, this.scanH, this.grid) ||
        (this.grid === "auto" && this.scanW
          ? `新しいデータが増えていません。走査 ${this.scanW}×${this.scanH} では ` +
            `${pxPerCell(this.scanW, this.scanH, "13x18").toFixed(1)} px/セル しかありません ` +
            `(13×18 の場合)。粗い格子を選ぶか、コードに近づいてください。`
          : "新しいデータが増えていません。ピント・明るさ・写る大きさを見直してください。");
    }
    this.onProgress({
      frames: this.frames,
      detected: this.detected,
      blocks: this.blocks,
      blocksTotal: this.blocksTotal,
      distinct: this.distinct,
      needed: this.needed,
      stall,
      seen: this.seenEsi,
    });
  }

  _finish(rawPayload) {
    this.finished = true;
    const scanFps = this.stats.fps;
    this.stop();
    // ファイル名/MIME ヘッダがあれば元の名前・種別で復元。無ければ従来どおり推測+タイムスタンプ名。
    const ts = new Date().toISOString().replace(/[:.]/g, "-").slice(0, 19);
    const meta = vcodeUnwrapFile(rawPayload);
    let data, name, mime;
    if (meta) {
      data = meta.data;
      const [ext, sm] = sniffType(data);
      name = meta.name || `vcode_${ts}.${ext}`;
      mime = meta.mime || sm;
    } else {
      data = rawPayload;
      const [ext, m] = sniffType(data);
      name = `vcode_${ts}.${ext}`;
      mime = m;
    }
    const blob = new Blob([data], { type: mime });
    // 計測値も一緒に返す。条件を振って比べるには所要時間と実効スループットが要る。
    const ms = this.firstDetectedAt === null ? 0 : performance.now() - this.firstDetectedAt;
    this.onDone({
      name, type: mime, size: data.length, blob,
      stats: {
        ms,
        kbps: ms > 0 ? (data.length / 1024) / (ms / 1000) : 0,
        frames: this.frames,
        detected: this.detected,
        scanFps,
        grid: this.grid,
        distinct: this.distinct,
        needed: this.needed,
        scan: this.scanW ? `${this.scanW}×${this.scanH}` : "",
      },
    });
  }
}

// スマホアプリ _sniffType と同一の型推定
function sniffType(b) {
  if (b.length > 3 && b[0] === 0xff && b[1] === 0xd8) return ["jpg", "image/jpeg"];
  if (b.length > 7 && b[0] === 0x89 && b[1] === 0x50) return ["png", "image/png"];
  if (b.length > 11 && b[8] === 0x57 && b[9] === 0x45 && b[10] === 0x42 && b[11] === 0x50) return ["webp", "image/webp"];
  // ISO-BMFF (オフセット 4 に 'ftyp'): HEIC/AVIF (iOS 写真の既定形式)
  if (b.length > 11 && b[4] === 0x66 && b[5] === 0x74 && b[6] === 0x79 && b[7] === 0x70) {
    const brand = String.fromCharCode(b[8], b[9], b[10], b[11]);
    if (["heic", "heix", "hevc", "heim", "heis", "mif1", "msf1"].includes(brand)) return ["heic", "image/heic"];
    if (brand === "avif" || brand === "avis") return ["avif", "image/avif"];
  }
  if (b.length > 3 && b[0] === 0x25 && b[1] === 0x50 && b[2] === 0x44 && b[3] === 0x46) return ["pdf", "application/pdf"];
  if (b.length > 1 && b[0] === 0x50 && b[1] === 0x4b) return ["zip", "application/zip"];
  const probe = b.subarray(0, 4096);
  let ctrl = 0;
  for (const c of probe) if (c < 9 || (c > 13 && c < 32) || c === 127) ctrl++;
  if (probe.length && ctrl / probe.length < 0.02) return ["txt", "text/plain;charset=utf-8"];
  return ["bin", "application/octet-stream"];
}
