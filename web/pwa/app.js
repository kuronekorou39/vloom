// Vloom PC PWA — エントリ (結線)。
// Rust コア (fountain + vcode) を WASM で初期化し、送信 / 受信を UI に繋ぐ。

import init, {
  FountainEncoder, FountainDecoder, vcodeWrapFile, VcodeBundle, vcodeUnbundle,
} from "./pkg/vloom_core_wasm.js";
import { VcodeSender, VcodeReceiver, REFRESH_SAFE_FPS } from "./vcode.js";
import { estimate, theoryKbps } from "./estimate.js";
import { fmtSize, fmtDuration, fmtStamp } from "./format.js";
import { setupCalibration } from "./calibration.js";
import { CameraPicker, streamDeviceId } from "./camera.js";

const $ = (id) => document.getElementById(id);

// ---- コア自己診断 ----
function selfTest() {
  const n = 5000;
  const payload = new Uint8Array(n);
  for (let i = 0; i < n; i++) payload[i] = (i * 37 + 11) & 0xff;
  const enc = new FountainEncoder(payload, 300, 20);
  const dec = new FountainDecoder(enc.otiBytes());
  const total = enc.packetCount();
  let recovered = null;
  for (let i = 0; i < total && recovered === null; i++) {
    if (dec.addPacket(enc.packet(i))) recovered = dec.payload();
  }
  const ok = recovered && recovered.length === n &&
    recovered[0] === payload[0] && recovered[n - 1] === payload[n - 1];
  const el = $("coreStatus");
  el.className = ok ? "ok" : "ng";
  el.textContent = ok ? "準備完了" : "コア異常";
  el.title = ok ? `自己診断: ${total} パケットの往復に成功` : "";
  return ok;
}

// ---- タブ ----
const PANES = { send: "paneSend", recv: "paneRecv", cal: "paneCal" };
const TABS = { send: "tabSend", recv: "tabRecv", cal: "tabCal" };
function showPane(which) {
  for (const [k, id] of Object.entries(PANES)) $(id).classList.toggle("active", k === which);
  for (const [k, id] of Object.entries(TABS)) $(id).classList.toggle("active", k === which);
  if (which !== "cal") calibration.stop(); // 校正タブを離れたらカメラ/表示を止める
}
for (const k of Object.keys(TABS)) $(TABS[k]).addEventListener("click", () => showPane(k));

// ---- 校正 ----
const calibration = setupCalibration({
  modeBtns: Array.from(document.querySelectorAll("#calMode button")),
  sendView: $("calSendView"),
  recvView: $("calRecvView"),
  showBtn: $("calShow"),
  stage: $("calStage"),
  canvas: $("calCanvas"),
  label: $("calLabel"),
  prev: $("calPrev"),
  next: $("calNext"),
  close: $("calClose"),
  recvStart: $("calRecvStart"),
  recvStop: $("calRecvStop"),
  recvReset: $("calRecvReset"),
  video: $("calVideo"),
  chips: $("calChips"),
  best: $("calBest"),
  blocks: $("calBlocks"),
  cameraSelect: $("calCamera"),
  diag: $("calDiag"),
});

// ---- 送信 ----
const sender = new VcodeSender({
  canvas: $("txCanvas"),
  onStatus: (s) => { $("txStatus").textContent = s; },
});

// 表示領域は画面全体。下部バーは重ねて出し、送信中は操作が止まって 4 秒で隠す
// (コードをタップで再表示)。バーの出入りでコードの大きさは変わらない
function fitTxCanvas() {
  sender.fit(window.innerWidth, window.innerHeight);
}
window.addEventListener("resize", fitTxCanvas);

let txBarTimer = null;
function showTxBar(autoHide = true) {
  $("txBar").classList.remove("hidden");
  clearTimeout(txBarTimer);
  if (autoHide) txBarTimer = setTimeout(() => $("txBar").classList.add("hidden"), 4000);
}
$("txBar").addEventListener("pointerdown", () => showTxBar());
$("txBar").addEventListener("input", () => showTxBar());
$("txCanvas").addEventListener("click", () => {
  if ($("txBar").classList.contains("hidden")) showTxBar(); else $("txBar").classList.add("hidden");
});

// 大きさ / 静止。大きさは収まる最大に対する % で、1 セルは常に整数個の物理画素
$("txSize").addEventListener("input", () => {
  sender.setSizePct(parseInt($("txSize").value) || 100);
  $("txSizeVal").value = `${$("txSize").value}%`;
});
$("txHold").addEventListener("change", () => sender.setHold($("txHold").checked));
setInterval(() => {
  if (!sender.running) return;
  // info() は「寸法」と、読めない大きさのときだけ付く「警告」を改行で返す。
  // 警告は muted の中に混ぜると見落とすので、別の枠に出す。
  const [geom, warn] = sender.info().split("\n");
  $("txGeom").textContent = geom;
  $("txWarn").textContent = warn || "";
}, 500);

// 送信の輝度/モアレ調整。キャンバスへの CSS フィルタで効かせる。
// 輝度=白レベルを下げて受信側の白飛びを抑える。
// モアレ=軽いぼかしで表示グリッドとカメラ画素の干渉縞を減らす (格子を粗くするのも有効)。
function applyTxFilter() {
  const b = (parseInt($("txBright").value) || 100) / 100;
  const blur = parseFloat($("txBlur").value) || 0;
  $("txCanvas").style.filter = `brightness(${b}) blur(${blur}px)`;
  $("txBrightVal").value = `${$("txBright").value}%`;
  $("txBlurVal").value = blur ? `${blur}px` : "なし";
}
$("txBright").addEventListener("input", applyTxFilter);
$("txBlur").addEventListener("input", applyTxFilter);
applyTxFilter();

$("txStop").addEventListener("click", () => {
  sender.stop();
  $("txStage").style.display = "none";
});

// ---- 送るもの (ファイル / テキスト / 計測データ) ----
// 送るのは常に 3 種類のうち選ばれている 1 つだけ。以前は欄が並んでいて
// 「計測データ > ファイル > テキスト」の順で黙って採用され、負けた入力は無視された。
const TX_MODES = { file: "txSrcFile", text: "txSrcText", test: "txSrcTest" };
const TX_MODE_LABEL = { file: "ファイル", text: "テキスト", test: "計測データ" };
let txMode = "file";
/** 選択中のファイル (File の配列)。選び直しても置き換えず、足していく */
let txFiles = [];

function setTxMode(mode) {
  txMode = mode;
  for (const [k, id] of Object.entries(TX_MODES)) $(id).hidden = k !== mode;
  for (const b of document.querySelectorAll("#txMode button")) {
    b.classList.toggle("active", b.dataset.m === mode);
  }
  updateTxSummary();
}
for (const b of document.querySelectorAll("#txMode button")) {
  b.addEventListener("click", () => setTxMode(b.dataset.m));
}

function addTxFiles(list) {
  for (const f of list) {
    // 同じものを 2 度選んだときは足さない (名前だけでは別ファイルと区別できない)
    const dup = txFiles.some((g) =>
      g.name === f.name && g.size === f.size && g.lastModified === f.lastModified);
    if (!dup) txFiles.push(f);
  }
  renderTxFiles();
}

function renderTxFiles() {
  const ul = $("txFiles");
  ul.textContent = "";
  txFiles.forEach((f, i) => {
    const li = document.createElement("li");
    const name = document.createElement("span");
    name.className = "name";
    name.textContent = f.name;
    name.title = f.name;
    const size = document.createElement("span");
    size.className = "size";
    size.textContent = fmtSize(f.size);
    const del = document.createElement("button");
    del.type = "button";
    del.textContent = "✕";
    del.setAttribute("aria-label", `${f.name} を外す`);
    del.addEventListener("click", () => { txFiles.splice(i, 1); renderTxFiles(); });
    li.append(name, size, del);
    ul.append(li);
  });
  $("txFilesFoot").hidden = txFiles.length === 0;
  $("txFilesNote").textContent = txFiles.length > 1
    ? `${txFiles.length} 個を 1 つの ZIP にまとめて送ります`
    : "";
  updateTxSummary();
}

$("txFile").addEventListener("change", () => {
  addTxFiles($("txFile").files);
  $("txFile").value = ""; // 同じファイルを外してから選び直せるように
});
$("txFilesClear").addEventListener("click", () => { txFiles = []; renderTxFiles(); });

// ドロップはページのどこに落としても受ける (枠を狙わせない)
const hasFiles = (e) => e.dataTransfer && Array.from(e.dataTransfer.types).includes("Files");
window.addEventListener("dragover", (e) => {
  if (!hasFiles(e)) return;
  e.preventDefault();
  $("txDrop").classList.add("over");
});
window.addEventListener("dragleave", () => $("txDrop").classList.remove("over"));
window.addEventListener("drop", (e) => {
  if (!hasFiles(e)) return;
  e.preventDefault();
  $("txDrop").classList.remove("over");
  showPane("send");
  setTxMode("file");
  addTxFiles(e.dataTransfer.files);
});

const txTextBytes = () => new TextEncoder().encode($("txText").value).length;

/** 今の選択で送るものの大きさと説明 (何も無ければ bytes = 0) */
function txPlan() {
  if (txMode === "file") {
    const bytes = txFiles.reduce((a, f) => a + f.size, 0);
    if (!txFiles.length) return { bytes: 0, text: "ファイルが選ばれていません" };
    return {
      bytes,
      text: txFiles.length === 1
        ? `${txFiles[0].name} · ${fmtSize(bytes)}`
        : `ファイル ${txFiles.length} 個 (ZIP) · ${fmtSize(bytes)}`,
    };
  }
  if (txMode === "text") {
    const bytes = txTextBytes();
    return { bytes, text: bytes ? `テキスト · ${fmtSize(bytes)}` : "テキストが入力されていません" };
  }
  const opt = $("txTest").selectedOptions[0];
  const bytes = parseInt(opt.dataset.size) || 0;
  return { bytes, text: `計測データ · ${opt.textContent} (${fmtSize(bytes)})` };
}

const txSettings = () => ({
  grid: $("txGrid").value,
  bpc: parseInt($("txBpc").value) || 1,
  fps: Math.max(2, Math.min(60, parseInt($("txFps").value) || 20)),
});

// 送信内容・所要時間の目安・理論値をまとめて出す。
// 目安は理論値ではなく実測の中央値から出す (理由と出どころは estimate.js)。
function updateTxSummary() {
  const plan = txPlan();
  const { grid, bpc, fps } = txSettings();
  $("txSummary").textContent = plan.text;
  $("txSummary").classList.toggle("muted", !plan.bytes);
  $("txStart").disabled = !plan.bytes;

  // 選んでいない側に入力が残っていたら、それは送られないと明示する
  const left = [];
  if (txMode !== "file" && txFiles.length) left.push(`ファイル ${txFiles.length} 個`);
  if (txMode !== "text" && txTextBytes()) left.push("入力済みのテキスト");
  $("txOther").textContent = left.length
    ? `${left.join("と")}は送られません (送るのは「${TX_MODE_LABEL[txMode]}」)`
    : "";

  const est = $("txEstimate");
  est.textContent = "";
  const rows = plan.bytes ? estimate(plan.bytes, grid, bpc, fps) : [];
  for (const r of rows) {
    const box = document.createElement("div");
    box.className = "est";
    for (const [cls, text] of [
      ["who", r.label], ["time", fmtDuration(r.seconds)], ["rate", `実測 ${r.kbps.toFixed(0)} KB/s`],
    ]) {
      const d = document.createElement("div");
      d.className = cls;
      d.textContent = text;
      box.append(d);
    }
    est.append(box);
  }
  const theory = `理論 ${theoryKbps(grid, bpc, fps).toFixed(0)} KB/s (${grid.replace("x", "×")} · ${bpc}bit · ${fps}fps)`;
  const note = !plan.bytes ? ""
    : rows.length ? "所要時間は実測の中央値からの目安 (三脚固定・Pixel 9a)。手持ちや距離で変わります。"
    : "この設定は実測が無いため、所要時間の目安を出せません。";
  const warn = fps > REFRESH_SAFE_FPS ? ` ※${REFRESH_SAFE_FPS}fps 超は 120Hz 画面向け` : "";
  $("txTheory").textContent = `${note}
${theory}${warn}`.trim();
}
for (const id of ["txGrid", "txBpc", "txFps", "txTest", "txText"]) {
  $(id).addEventListener("change", updateTxSummary);
  $(id).addEventListener("input", updateTxSummary);
}
// 既定 fps: スマホ (タッチ端末) は 30 (OLED は切り替わりが速く 60Hz の 2 書き換えごとが最速)。
// PC モニタは LCD が多く、残像で 30fps は逆に遅いので 20 のまま
if (navigator.maxTouchPoints > 0 && Math.min(screen.width, screen.height) < 900) {
  $("txFps").value = 30;
}
renderTxFiles();

/** 選ばれているものを、送る 1 本のバイト列 (と名前・種別) にする */
async function buildTxPayload() {
  const now = new Date();
  if (txMode === "test") {
    // 同梱の計測用画像。サイズを焼き込んであるので、受信側で開けば
    // どの条件のデータが壊れずに届いたか一目で分かる。
    const asset = $("txTest").value;
    const res = await fetch(asset);
    if (!res.ok) throw new Error(`計測データを取得できません (${res.status})`);
    return {
      bytes: new Uint8Array(await res.arrayBuffer()),
      name: asset.split("/").pop(),
      mime: "image/jpeg",
    };
  }
  if (txMode === "text") {
    // 毎回 message.txt だと受信側で同名ファイルが積み重なるので、送信時刻を付ける
    return {
      bytes: new TextEncoder().encode($("txText").value),
      name: `message_${fmtStamp(now)}.txt`,
      mime: "text/plain;charset=utf-8",
    };
  }
  if (txFiles.length === 1) {
    const f = txFiles[0];
    return { bytes: new Uint8Array(await f.arrayBuffer()), name: f.name, mime: f.type || "" };
  }
  // 複数ファイルは無圧縮 ZIP にまとめる (形式は 1 本のバイト列しか運ばない)。
  // 受信側がまとめを解けない古い版でも、ZIP としてそのまま開ける。
  const bundle = new VcodeBundle();
  try {
    for (const f of txFiles) bundle.add(f.name, new Uint8Array(await f.arrayBuffer()));
    return {
      bytes: bundle.finish(now.getFullYear(), now.getMonth() + 1, now.getDate(),
        now.getHours(), now.getMinutes(), now.getSeconds()),
      name: `vloom_${fmtStamp(now)}.zip`,
      mime: "application/zip",
    };
  } finally {
    bundle.free();
  }
}

$("txStart").addEventListener("click", async () => {
  if (!txPlan().bytes) return;
  const { grid, bpc, fps } = txSettings();
  $("txInfo").textContent = "";
  try {
    const { bytes, name, mime } = await buildTxPayload();
    // 元のファイル名/MIME をヘッダに埋めて送る (受信側で元名・種別をそのまま復元)
    const source = vcodeWrapFile(name, mime, bytes);
    $("txStage").style.display = "flex";
    fitTxCanvas();
    showTxBar();
    await sender.start(source, grid, bpc, fps);
  } catch (e) {
    $("txStage").style.display = "none";
    $("txInfo").textContent = "送信エラー: " + (e && e.message ? e.message : e);
  }
});

// ---- 受信 ----
// 受信データ由来の文字列を HTML/属性へ入れるときのエスケープ。ファイル名も MIME も
// 送信側 (= 他人の画面) が決めるので、素のまま innerHTML に入れてはいけない。
const escHtml = (t) =>
  String(t).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
const escAttr = (t) => escHtml(t).replace(/"/g, "&quot;").replace(/'/g, "&#39;");

const rxPicker = new CameraPicker($("rxCamera"));

// どの ESI (パケット番号) が埋まったかを帯で描く。
// 進捗の % だけだと「全体に薄く集まっている」のか「一部だけを繰り返し拾っている」のかが
// 区別できない。後者は永久に終わらない状態で、今日まさにそれで詰まっていた。
//
// 1 マス 1 パケットで描くと、1MB でも 2 万マスを超えて潰れる (実機で細い線の帯になった)。
// 番号順にバケットへまとめ、そのバケットの充足率を濃淡で出す。
// 緑 = source パケットの範囲、水色 = repair の範囲。
const covCanvas = $("rxCoverage");
let covLast = -1;
function drawCoverage(seen, needed) {
  if (!seen || !needed) { covCanvas.classList.remove("on"); return; }
  covCanvas.classList.add("on");
  // 増えていなければ描き直さない
  if (seen.size === covLast) return;
  covLast = seen.size;
  const dpr = window.devicePixelRatio || 1;
  const w = Math.round(covCanvas.clientWidth * dpr);
  const h = Math.round(covCanvas.clientHeight * dpr);
  if (!w || !h) return;
  if (covCanvas.width !== w || covCanvas.height !== h) { covCanvas.width = w; covCanvas.height = h; }
  const ctx = covCanvas.getContext("2d");
  // repair も含めて、届いている中で最大の番号までを描く
  let cap = needed;
  for (const e of seen) if (e + 1 > cap) cap = e + 1;
  const s = Math.max(3 * dpr, Math.round(5 * dpr));
  const cols = Math.max(1, Math.floor(w / s));
  const rows = Math.max(1, Math.floor(h / s));
  const cells = cols * rows;
  const per = Math.max(1, Math.ceil(cap / cells));
  const got = new Uint32Array(cells);
  for (const e of seen) {
    const i = Math.floor(e / per);
    if (i < cells) got[i]++;
  }
  ctx.clearRect(0, 0, w, h);
  ctx.fillStyle = "rgba(20,26,32,0.72)";
  ctx.fillRect(0, 0, w, h);
  const gap = s > 4 * dpr ? dpr : 0;
  for (let i = 0; i < cells; i++) {
    const ratio = Math.min(1, got[i] / per);
    if (ratio === 0) continue;
    // source と repair で色を分ける (バケットの中心がどちら側かで判定)
    const src = (i + 0.5) * per < needed;
    ctx.fillStyle = src
      ? `rgba(76,175,80,${0.25 + 0.75 * ratio})`
      : `rgba(41,182,246,${0.25 + 0.75 * ratio})`;
    ctx.fillRect((i % cols) * s, Math.floor(i / cols) * s, s - gap, s - gap);
  }
}

const receiver = new VcodeReceiver({
  video: $("rxVideo"),
  onDiag: (t) => { $("rxDiag").textContent = t; },
  onProgress: ({ frames, detected, blocks, blocksTotal, distinct, needed, stall, seen }) => {
    // 進捗は「重複を除いたパケット数 / 必要数」で出す。frames や検出回数は増え続けるので、
    // それだけを出していると、同じブロックしか拾えていない状態が「順調に見えて終わらない」
    // になっていた (実際に起きていた壊れ方)。
    const pct = needed ? Math.min(100, Math.floor((distinct / needed) * 100)) : 0;
    const head = needed
      ? `復元まで ${pct}% · ${distinct}/${needed} パケット`
      : detected
        ? "コードを検出 · ヘッダ待ち"
        : "コードを探しています";
    $("rxInfo").textContent =
      `${head} · 直近 ${blocks}/${blocksTotal} ブロック · ${detected}/${frames} 枚検出`;
    $("rxBar").style.width = `${pct}%`;
    $("rxStall").textContent = stall || "";
    drawCoverage(seen, needed);
  },
  onDone: async ({ name, type, size, blob, stats }) => {
    // 受信したファイル名は相手の画面から来る = 攻撃者が選べる。HTML に入れる前に必ず
    // エスケープする (以前は素のまま innerHTML に入れていて XSS になっていた)
    const safeName = escHtml(name);
    $("rxInfo").innerHTML = `<span class="ok">✅ 復元成功: ${safeName} (${fmtSize(size)})</span>`;
    $("rxBar").style.width = "100%";
    $("rxStall").textContent = "";
    covCanvas.classList.remove("on");
    const url = rxUrl(blob);
    const isImage = type.startsWith("image/");
    const isVideo = type.startsWith("video/");
    const isAudio = type.startsWith("audio/");
    const isText = type.startsWith("text/");
    // 動画・音声・テキストもその場で確認できるようにする (ブラウザのデコーダに任せる)
    let preview = "";
    if (isImage) {
      preview = `<p><img src="${url}" style="max-width:100%;border-radius:8px" /></p>`;
    } else if (isVideo) {
      preview = `<p><video controls playsinline src="${url}" style="max-width:100%;border-radius:8px"></video></p>`;
    } else if (isAudio) {
      preview = `<p><audio controls src="${url}" style="width:100%"></audio></p>`;
    } else if (isText) {
      const esc = escHtml;
      let text = await blob.text();
      const cut = text.length > 4000;
      if (cut) text = text.slice(0, 4000);
      preview = `<pre style="max-height:16em;overflow:auto;white-space:pre-wrap;background:rgba(0,0,0,0.25);padding:0.8em;border-radius:8px">${esc(text)}${cut ? "\n… (以下略。全文はダウンロードで)" : ""}</pre>`;
    }
    // 計測値は折りたたむ (アプリの結果画面と同じ)。条件を振って比べるときに開く
    const s = stats || {};
    const rows = [
      ["実効スループット", `${(s.kbps || 0).toFixed(1)} KB/s`],
      ["所要時間 (初検出→完了)", `${((s.ms || 0) / 1000).toFixed(2)} 秒`],
      ["フレーム", `${s.detected || 0} 検出 / ${s.frames || 0} 走査`],
      ["スキャン fps", (s.scanFps || 0).toFixed(1)],
      ["走査解像度", s.scan || "-"],
      ["パケット", s.needed ? `${s.distinct} 回収 / 必要 ${s.needed}` : `${s.distinct || 0} 回収`],
      ["格子指定", s.grid === "auto" ? "自動 (候補総当たり)" : s.grid],
    ];
    const summary = `${fmtSize(size)} · ${((s.ms || 0) / 1000).toFixed(1)} 秒 · ${(s.kbps || 0).toFixed(1)} KB/s`;
    // 複数ファイルをまとめて送られたものは、中身を 1 つずつ保存できるように並べる。
    // 解けるのはこちらがまとめた無圧縮 ZIP だけ (一般の ZIP は ZIP のまま渡す)
    let list = "";
    const packed = type === "application/zip"
      ? vcodeUnbundle(new Uint8Array(await blob.arrayBuffer()))
      : undefined;
    if (packed) {
      const items = [];
      for (let i = 0; i < packed.count; i++) {
        const n = packed.name(i);
        const d = packed.data(i);
        const u = rxUrl(new Blob([d]));
        items.push(`<li><span class="name">${escHtml(n)}</span>` +
          `<span class="size">${fmtSize(d.length)}</span>` +
          `<a href="${u}" download="${escAttr(n)}">保存</a></li>`);
      }
      packed.free();
      list = `<ul class="files">${items.join("")}</ul>`;
    }
    // 共有シート (iOS Safari / Android Chrome)。ファイルを渡せる環境だけボタンを出す
    const file = new File([blob], name, { type: type || "application/octet-stream" });
    const canShare = !!(navigator.canShare && navigator.canShare({ files: [file] }));
    $("rxResult").innerHTML =
      `<div class="rxcard">` +
      `<h2 class="ok">✅ 受け取りました</h2>` +
      `<div class="rxname">${safeName}</div>` +
      `<div class="muted">${summary}</div>` +
      preview + list +
      `<div class="rxactions">` +
      `<a class="btn" href="${url}" download="${escAttr(name)}">${packed ? "まとめて保存" : "保存"}</a>` +
      (canShare ? `<button id="rxShare" class="secondary">共有</button>` : "") +
      `</div>` +
      `<div class="rxactions">` +
      `<button id="rxAgain" class="secondary">続けて受信</button>` +
      `<button id="rxClose" class="secondary">閉じる</button>` +
      `</div>` +
      `<details class="rxstats"><summary>計測値</summary><table class="stats">${rows
        .map(([k, v]) => `<tr><th>${k}</th><td>${v}</td></tr>`)
        .join("")}</table></details>` +
      `</div>`;
    if (canShare) {
      $("rxShare").addEventListener("click", () => {
        // 共有シートを閉じただけでも reject になるので、エラーは出さない
        navigator.share({ files: [file] }).catch(() => {});
      });
    }
    $("rxAgain").addEventListener("click", () => startReceive());
    $("rxClose").addEventListener("click", () => closeReceive());
  },
});
// 完了画面で作った object URL。次の受信や閉じるときに解放する
let rxUrls = [];
function rxUrl(blob) {
  const u = URL.createObjectURL(blob);
  rxUrls.push(u);
  return u;
}
function clearResult() {
  $("rxResult").innerHTML = "";
  for (const u of rxUrls) URL.revokeObjectURL(u);
  rxUrls = [];
}
async function startReceive() {
  clearResult();
  $("rxError").textContent = "";
  $("rxStall").textContent = "";
  $("rxBar").style.width = "0%";
  covLast = -1;
  covCanvas.classList.remove("on");
  $("rxInfo").textContent = "スキャン中 — 送信側の vcode を枠に収めてください";
  $("rxStage").classList.add("active"); // 先に全画面を出す (映像の表示サイズが確定してから走査)
  try {
    await receiver.start(rxPicker.deviceId, $("rxGrid").value);
    rxPicker.refresh(streamDeviceId(receiver.stream)); // 許可後はデバイス名が取れるので一覧を更新
    setupExposure(); // 露出コントロールを対応状況に合わせて出す
    $("rxStart").disabled = true;
    $("rxStop").disabled = false;
  } catch (e) {
    $("rxStage").classList.remove("active");
    $("rxError").textContent = "カメラ起動失敗: " + (e && e.message ? e.message : e);
  }
}
// 受信画面を閉じて設定の画面に戻る (完了後も、途中で止めたときも)
function closeReceive() {
  receiver.stop();
  clearResult();
  $("rxStage").classList.remove("active");
  $("rxStart").disabled = false;
  $("rxStop").disabled = true;
}
$("rxStart").addEventListener("click", () => startReceive());
// 受信中でも格子指定を切り替えられる (総当たり→固定で初回検出が速くなる)
$("rxGrid").addEventListener("change", () => receiver.setGrid($("rxGrid").value));
$("rxStop").addEventListener("click", () => closeReceive());

// 受信の手動露出。露出制御対応カメラでのみスライダーを出す (スマホ Chrome は非対応が
// 多く、その場合は非対応の注記を出す)。手動中は自動制御 (白飛び対策) を止める。
const rxExpoAuto = $("rxExpoAuto"), rxExpoRange = $("rxExpoRange"), rxExpoVal = $("rxExpoVal");
function setupExposure() {
  const g = receiver.exposure;
  const ok = g && g.supported;
  $("rxExpo").hidden = !ok;
  $("rxExpoNote").hidden = ok;
  if (!ok) return;
  rxExpoRange.min = g.min;
  rxExpoRange.max = g.max;
  rxExpoRange.step = Math.max(1, Math.round((g.max - g.min) / 100));
  rxExpoRange.value = g.currentValue();
  rxExpoAuto.checked = true;
  rxExpoRange.disabled = true;
  rxExpoVal.textContent = "";
}
rxExpoAuto.addEventListener("change", () => {
  const g = receiver.exposure;
  if (!g) return;
  if (rxExpoAuto.checked) {
    g.setAuto();
    rxExpoRange.disabled = true;
    rxExpoVal.textContent = "";
  } else {
    rxExpoRange.disabled = false;
    g.setManual(parseInt(rxExpoRange.value));
    rxExpoVal.textContent = g.valueText();
  }
});
rxExpoRange.addEventListener("input", () => {
  const g = receiver.exposure;
  if (!g || rxExpoAuto.checked) return;
  g.setManual(parseInt(rxExpoRange.value));
  rxExpoVal.textContent = g.valueText();
});

// ---- 起動 ----
// 初期表示は送信ペイン。QR 経路の削除でペイン構成を作り直したとき、初期の active 付与が
// 抜けていた (タブを押すまで本文が空)。showPane は calibration を参照するので、定義より
// 後のここで呼ぶ (先頭で呼ぶと TDZ の ReferenceError で後続の配線が全部死ぬ)
showPane("send");
(async () => {
  try {
    await init();
    selfTest();
  } catch (e) {
    const el = $("coreStatus");
    el.className = "ng";
    el.textContent = "コア読み込み失敗: " + (e && e.message ? e.message : e);
  }
})();

if ("serviceWorker" in navigator) {
  // 新しい版の SW に切り替わったら 1 回だけ読み直す。cache-first なので、
  // 読み直さないと表示中のページは古い版のまま残る (iPhone の Safari で
  // 再読み込みしても直らなかった)。受信中なら閉じたときに読み直す
  let reloadPending = false;
  const hadController = !!navigator.serviceWorker.controller;
  navigator.serviceWorker.addEventListener("controllerchange", () => {
    if (!hadController || reloadPending) return; // 初回のインストールでは読み直さない
    reloadPending = true;
    if (!$("rxStage").classList.contains("active")) location.reload();
  });
  $("rxStop").addEventListener("click", () => { if (reloadPending) location.reload(); });
  document.addEventListener("click", (e) => {
    if (reloadPending && e.target && e.target.id === "rxClose") location.reload();
  });
  window.addEventListener("load", () => {
    // sw.js 自体は HTTP キャッシュを通さずに確認する (Pages は 10 分キャッシュする)
    navigator.serviceWorker.register("./sw.js", { updateViaCache: "none" })
      .then((reg) => reg.update())
      .catch(() => {});
  });
}
