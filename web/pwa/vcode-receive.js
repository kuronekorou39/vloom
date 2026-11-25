// vcode 受信: カメラ → 輝度 → VcodeRx.scan (ワーカー) → パケット → FountainDecoder
// → CRC 検証 → 元のファイル名/種別で返す。

import { VcodeRx, FountainDecoder, vcodeUnwrapPayload, vcodeUnwrapFile } from "./pkg/vloom_core_wasm.js";
import {
  openCamera, ExposureGuard, cameraInfoText, lumaText, cellPxText,
  gridTooDense, pxPerCell,
} from "./camera.js";
import { PacketCoverage } from "./coverage.js";

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
// 復元ワーカーの準備完了をこれだけ待つ。返事が無ければ主スレッドでの復元に切り替える
const DECODER_READY_TIMEOUT_MS = 5000;
// 診断表示の更新間隔
const DIAG_INTERVAL_MS = 500;

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
    // 校正用: 設定すると復号せず、検出した枚のブロック数だけをこの関数に渡す
    // (どの格子がどれだけ読めるかを見るため。校正の図柄はファウンテン符号の転送ではない)
    this.probe = null;
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
    this._ingest(m.oti, m.blocks, m.blocksTotal, m.packets);
  }

  /** 検出できた 1 枚ぶんのパケットをデコーダに積み、揃ったら復元する。
   *  ワーカー経路と主スレッド経路で共用 (デコーダはワーカーをまたいで 1 つ)。 */
  _ingest(oti, blocks, blocksTotal, packets) {
    this.detected++;
    // 所要時間は「初検出 → 復元完了」で測る (カメラを向けるまでの時間を含めない)
    if (this.firstDetectedAt === null) this.firstDetectedAt = performance.now();
    this.blocks = blocks;
    this.blocksTotal = blocksTotal;
    if (this.probe) { this.probe(blocks, blocksTotal); this._progress(); return; }
    if (!this.cov) {
      this.cov = new PacketCoverage(oti);
      this.needed = this.cov.kt;
      this._startDecoder(oti);
    }
    const tDec = performance.now();
    // 重複を除いた被覆を数える (進捗と停滞判定に使う。複数ソースブロック対応は coverage.js)
    for (const pkt of packets) this.cov.add(pkt);
    // 復元計算は別ワーカーへ投げっぱなし (完了は _onDecoded で受ける)
    this._feedDecoder(packets);
    // 主スレッドで復元した場合はここで完了していることがある (完了表示を進捗で上書きしない)
    if (this.finished) return;
    // 主スレッドでの投入時間 (1 枚あたり)。ワーカーを増やしても伸びないとき、
    // ここが詰まっているのかを切り分ける
    this.decMs = this.decMs * 0.9 + (performance.now() - tDec) * 0.1;
    this.distinct = this.cov.useful;
    this._progress();
  }

  /** 復元ワーカー (decode-worker.js) を立てる。作れない環境では主スレッドで復元する。 */
  _startDecoder(oti) {
    this._stopDecoder();
    const gen = this.decGen;
    // 準備完了の返事が来るまでに渡したパケットは控えておく (ワーカーが使えず主スレッドに
    // 切り替えるとき、渡し直す)
    this.decBacklog = [];
    this.decReady = false;
    const fallback = () => { if (gen === this.decGen && !this.decReady) this._decodeInline(oti); };
    try {
      const w = new Worker(new URL("./decode-worker.js", import.meta.url), { type: "module" });
      w.onmessage = (e) => this._onDecodeMsg(gen, e.data, fallback);
      w.onerror = fallback;
      w.postMessage({ k: "init", oti });
      this.decWorker = w;
      // 読み込みに失敗しても知らせが来ない場合に備え、返事が無ければ主スレッドに切り替える
      setTimeout(fallback, DECODER_READY_TIMEOUT_MS);
    } catch (_) {
      this._decodeInline(oti);
    }
  }

  /** ワーカーが使えないときの退避: 主スレッドで復元する (大きいファイルでは最後に止まる) */
  _decodeInline(oti) {
    const backlog = this.decBacklog || [];
    this._stopDecoder();
    console.warn("[vcode-rx] 復元ワーカーが使えないので主スレッドで復元する");
    try {
      this.decInline = new FountainDecoder(oti);
    } catch (_) {
      this.cov = null; // 宣言サイズが不正なフレーム。次の枚で作り直す
      return;
    }
    for (const packets of backlog) this._feedDecoder(packets);
  }

  _feedDecoder(packets) {
    if (this.decInline) {
      for (const p of packets) {
        if (!this.decInline.addPacket(p)) continue;
        this._onDecoded(vcodeUnwrapPayload(this.decInline.payload()));
        return;
      }
      return;
    }
    if (this.decWorker && packets.length) {
      if (!this.decReady) this.decBacklog.push(packets);
      this.decWorker.postMessage({ k: "feed", packets });
    }
  }

  _onDecodeMsg(gen, m, fallback) {
    if (gen !== this.decGen || this.finished) return;
    if (m.k === "ready") { this.decReady = true; this.decBacklog = []; }
    else if (m.k === "fatal") fallback();
    else if (m.k === "done") this._onDecoded(m.payload);
    else if (m.k === "corrupt") this._onDecoded(null);
    else if (m.k === "error") { this._stopDecoder(); this.cov = null; } // 不正な OTI。次の枚で作り直す
  }

  /** 復元と CRC 検証が終わった (payload が null なら CRC 不一致) */
  _onDecoded(payload) {
    this._stopDecoder();
    if (!payload) {
      // 不一致 = ゴミパケットが RaptorQ を汚染して復元結果が破損 → デコーダを作り直して受信続行
      console.warn("[vcode-rx] 整合性エラー: 復元結果が破損。デコーダを作り直して受信続行");
      this.cov = null;
      this.distinct = 0;
      this.lastDistinct = 0;
      this.lastGainAt = performance.now();
      return;
    }
    this._finish(payload);
  }

  _stopDecoder() {
    this.decGen = (this.decGen || 0) + 1;
    if (this.decWorker) this.decWorker.terminate();
    this.decWorker = null;
    this.decInline = null;
    this.decBacklog = [];
    this.decReady = false;
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
    const loop = () => {
      if (!this.stream || this.finished) return;
      this._scanInline();
      this.rafId = requestAnimationFrame(loop);
    };
    this.rafId = requestAnimationFrame(loop);
  }

  _reset() {
    this._stopDecoder();
    this.rx = null; this.finished = false; this.frames = 0; this.detected = 0;
    this.stats = { fps: 0, mean: 0, sat: 0 };
    this._lastDiag = 0;
    this.firstDetectedAt = null;
    // 重複を除いた被覆 (ブロックごとに必要数で頭打ちした受信数) と、必要パケット数。
    // 進捗表示と停滞判定に使う
    this.distinct = 0;
    this.needed = 0;
    this.lastDistinct = 0;
    this.lastGainAt = performance.now();
    this.scanW = 0;
    this.scanH = 0;
    this.blocks = 0;
    this.blocksTotal = 0;
    // 被覆はワーカーをまたいで 1 つにまとめる (ワーカーごとに持つと合流できない)。
    // OTI が分かった時点 (デコーダと同時) に作る
    this.cov = null;
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
    this._stopDecoder();
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
    const packets = [];
    for (let k = 0, n2 = rep.packetCount(); k < n2; k++) packets.push(rep.packet(k));
    this._ingest(rep.oti, rep.blocksOk, rep.blocksTotal, packets);
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
    if (!this.probe && this.detected > 0 && performance.now() - this.lastGainAt > STALL_MS) {
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
      cov: this.cov,
    });
  }

  _finish(rawPayload) {
    this.finished = true;
    const scanFps = this.stats.fps;
    this.stop();
    // ファイル名/MIME ヘッダがあれば元の名前・種別で復元。無ければ従来どおり推測+タイムスタンプ名。
    const ts = new Date().toISOString().replace(/[:.]/g, "-").slice(0, 19);
    const meta = vcodeUnwrapFile(rawPayload);
    const data = meta ? meta.data : rawPayload;
    const [ext, sniffed] = sniffType(data);
    const name = meta?.name || `vcode_${ts}.${ext}`;
    const mime = meta?.mime || sniffed;
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

// アプリ版 sniffFileType (app/lib/receive/file_sniff.dart) と同一の型推定
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
