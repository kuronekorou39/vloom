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

// 送信ステージの余白 (セル)。マーカーの外側に白が要る (受信の環/余白の対比検査)。
// 画面の外は黒い縁なので、画面いっぱいには描かない
const TX_MARGIN_CELLS = 6;

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
   *  1 セルを整数個の物理画素で描く。以前は 1080x1080 固定の裏バッファを CSS で拡縮
   *  していて、セルの境界が画素にまたがってぼけていた (スマホは DPR 3 なので特に)。 */
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

  /** 現在の描画情報 (診断表示用): 1 セルの物理画素数と画面上の大きさ */
  info() {
    if (!this.w) return "";
    const px = this._cellPx();
    return `${this.w}×${this.h} セル · ${px}px/セル · ${this.w * px}×${this.h * px}px${this._rotated() ? " · 90°回転" : ""}`;
  }

  /** 画面とコードの縦横が食い違うとき (横向きの端末で縦長コード) は 90° 回して描く。
   *  大きく描けるだけでなく、画面の書き換え方向 (端末の上→下) がコードに対して横になり、
   *  縦持ちの受信カメラのローリングシャッター (上→下) と直交する。平行だと切り替えの
   *  混ざりが画面の半分に及んだ (iPhone 12 Pro → Pixel 9a、縦持ち同士で実測)。 */
  _rotated() {
    const { canvas } = this;
    return (canvas.width > canvas.height) !== (this.w > this.h);
  }

  _cellPx() {
    const { canvas } = this;
    const m = TX_MARGIN_CELLS * 2;
    const [cw, ch] = this._rotated() ? [this.h, this.w] : [this.w, this.h];
    const max = Math.min(canvas.width / (cw + m), canvas.height / (ch + m));
    return Math.max(1, Math.floor(max * this.sizePct / 100));
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
    this.worker = null;
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
    this._startWorker();
  }

  /** 走査ワーカーを起こす。別スレッドにする理由は scan-worker.js の先頭を参照。 */
  _startWorker() {
    try {
      this.worker = new Worker(new URL("./scan-worker.js", import.meta.url), { type: "module" });
    } catch (e) {
      console.warn("[vcode-rx] ワーカーを作れないので主スレッドで走査する", e);
      this._fallbackToMainThread();
      return;
    }
    this.worker.onmessage = (e) => this._onWorker(e.data);
    this.worker.onerror = (e) => {
      console.warn("[vcode-rx] ワーカーが起動できないので主スレッドで走査する", e.message);
      this._fallbackToMainThread();
    };
    this.worker.postMessage({ k: "init", grid: this.grid });
  }

  _onWorker(m) {
    switch (m.k) {
      case "ready":
        this._feedWorker();
        break;
      case "progress":
        this._onWorkerProgress(m);
        break;
      case "ack":
        // 退避経路: 1 枚ぶんの処理が終わった合図 (次の 1 枚を送ってよい)
        this._inFlight = false;
        break;
      case "integrity":
        console.warn("[vcode-rx] 整合性エラー: 復元結果が破損。デコーダを作り直して受信続行");
        this.lastDistinct = 0;
        this.lastGainAt = performance.now();
        break;
      case "done":
        this._finish(m.payload);
        break;
      case "error":
        console.warn("[vcode-rx] worker:", m.msg);
        break;
      default:
        break;
    }
  }

  /** カメラ映像をワーカーへ渡す。VideoFrame を直接読める環境ならそちらを使う。 */
  _feedWorker() {
    const track = this.stream && this.stream.getVideoTracks()[0];
    if (track && typeof MediaStreamTrackProcessor !== "undefined") {
      try {
        // プレビュー用の track とは別に読む (同じ track を 2 か所で消費しないよう複製)
        this.procTrack = track.clone();
        const proc = new MediaStreamTrackProcessor({ track: this.procTrack });
        this.worker.postMessage({ k: "stream", readable: proc.readable }, [proc.readable]);
        return;
      } catch (e) {
        console.warn("[vcode-rx] VideoFrame 経路が使えないので描画経路にする", e);
        if (this.procTrack) { this.procTrack.stop(); this.procTrack = null; }
      }
    }
    this._startPushLoop();
  }

  /** 退避経路: 主スレッドで輝度に落としてワーカーへ送る (VideoFrame が使えない環境)。 */
  _startPushLoop() {
    this._inFlight = false;
    const loop = () => {
      if (!this.stream || this.finished) return;
      this.rafId = requestAnimationFrame(loop);
      if (this._inFlight) return; // 処理中に送っても古くなるだけなので待つ
      const g = this._grabGray();
      if (!g) return;
      this._inFlight = true;
      this.worker.postMessage(
        { k: "frame", gray: g.gray.buffer, w: g.w, h: g.h, stride: g.w },
        [g.gray.buffer],
      );
    };
    this.rafId = requestAnimationFrame(loop);
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
    if (this.worker) { this.worker.terminate(); this.worker = null; }
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
    this._inlineEsi = null;
  }

  /** 探索する格子を切り替える ("auto" で候補総当たり)。受信中でも即反映する。 */
  setGrid(grid) {
    this.grid = grid;
    this._positionGuide();
    if (this.worker) { this.worker.postMessage({ k: "grid", grid }); return; }
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
      `${this.stats.fps.toFixed(1)} fps · ${lumaText(this.stats, this.exposure)}\n` +
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
    if (this.worker) {
      this.worker.postMessage({ k: "stop" });
      this.worker.terminate();
      this.worker = null;
    }
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

  /** ワーカーからの進捗を UI へ流す。 */
  _onWorkerProgress(m) {
    this._inFlight = false;
    this.frames = m.frames;
    if (m.detected > 0 && this.firstDetectedAt === null) {
      // 所要時間は「初検出 → 復元完了」で測る (カメラを向けるまでの時間を含めない)
      this.firstDetectedAt = performance.now();
    }
    this.detected = m.detected;
    this.distinct = m.distinct;
    this.needed = m.needed;
    this.scanW = m.scanW;
    this.scanH = m.scanH;
    this.blocks = m.blocks;
    this.blocksTotal = m.blocksTotal;
    this.stats = { fps: m.fps, mean: m.mean, sat: m.sat };
    this._setGuideLocked(m.blocksTotal > 0);
    this._positionGuide();
    this._diag();
    this._progress();
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
    const seen = this._inlineEsi || (this._inlineEsi = new Set());
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
