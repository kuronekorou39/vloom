// Vloom PC PWA — エントリ (結線)。
// Rust コア (fountain) を WASM で初期化し、送信 (Sender) / 受信 (Receiver) を UI に繋ぐ。

import init, { FountainEncoder, FountainDecoder, vcodeWrapFile } from "./pkg/vloom_core_wasm.js";
import { Sender } from "./sender.js";
import { Receiver } from "./receiver.js";
import { VcodeSender, VcodeReceiver, packetSizeFor, REFRESH_SAFE_FPS } from "./vcode.js";
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
  el.textContent = ok ? `コア: OK (${total} pkt 往復)` : "コア異常";
  return ok;
}

// ---- タブ ----
const PANES = { send: "paneSend", recv: "paneRecv", vsend: "paneVSend", vrecv: "paneVRecv", cal: "paneCal" };
const TABS = { send: "tabSend", recv: "tabRecv", vsend: "tabVSend", vrecv: "tabVRecv", cal: "tabCal" };
function showPane(which) {
  for (const [k, id] of Object.entries(PANES)) $(id).classList.toggle("active", k === which);
  for (const [k, id] of Object.entries(TABS)) $(id).classList.toggle("active", k === which);
  if (which !== "cal") calibration.stop(); // 校正タブを離れたらカメラ/表示を止める
}
for (const k of Object.keys(TABS)) $(TABS[k]).addEventListener("click", () => showPane(k));

// ---- 校正 ----
const calibration = setupCalibration({
  kindBtns: Array.from(document.querySelectorAll("#calKind button")),
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

// ---- 送信 (QR / vcode 共用ステージ) ----
let activeSender = null;
const sender = new Sender({
  canvas: $("txCanvas"),
  onStatus: (s) => { $("txStatus").textContent = s; },
});
const vcodeSender = new VcodeSender({
  canvas: $("txCanvas"),
  onStatus: (s) => { $("txStatus").textContent = s; },
});

function fitTxCanvas() {
  // 画面に収まる最大の正方形で表示 (下部バーの分を少し引く)
  const size = Math.min(window.innerWidth, window.innerHeight - 56) - 8;
  $("txCanvas").style.width = `${size}px`;
  $("txCanvas").style.height = `${size}px`;
}
window.addEventListener("resize", fitTxCanvas);

// 送信の輝度/モアレ調整。QR・vcode とも同じ #txCanvas に描くので、キャンバスへの
// CSS フィルタ一つで両方に効く。輝度=白レベルを下げて受信側の白飛びを抑える。
// モアレ=軽いぼかしで表示グリッドとカメラ画素の干渉縞を減らす (格子を粗くするのも有効)。
function applyTxFilter() {
  const b = (parseInt($("txBright").value) || 100) / 100;
  const blur = parseFloat($("txBlur").value) || 0;
  $("txCanvas").style.filter = `brightness(${b}) blur(${blur}px)`;
}
$("txBright").addEventListener("input", applyTxFilter);
$("txBlur").addEventListener("input", applyTxFilter);
applyTxFilter();

$("txStart").addEventListener("click", async () => {
  let file = $("txFile").files[0];
  if (!file) {
    // ファイル未選択ならテキストを送信
    const text = $("txText").value;
    if (!text) { $("txInfo").textContent = "ファイルを選択するかテキストを入力してください"; return; }
    file = new File([new TextEncoder().encode(text)], "message.txt", { type: "text/plain;charset=utf-8" });
  }
  const grid = $("txGrid").value;
  const ec = $("txEc").value;
  const fps = Math.max(2, Math.min(30, parseInt($("txFps").value) || 10));
  $("txInfo").textContent = "";
  fitTxCanvas();
  $("txStage").style.display = "flex";
  activeSender = sender;
  try {
    await sender.start(file, grid, ec, fps);
  } catch (e) {
    $("txStage").style.display = "none";
    $("txInfo").textContent = "送信エラー: " + (e && e.message ? e.message : e);
  }
});
$("txStop").addEventListener("click", () => {
  if (activeSender) activeSender.stop();
  $("txStage").style.display = "none";
});

// ---- V送信 (vcode) ----
// 設定の理論スループット (ブロック数 × packet_size × fps) を出す。実測との比較基準。
function updateVtxTheory() {
  const [gw, gh] = $("vtxGrid").value.split("x").map(Number);
  const bpc = parseInt($("vtxBpc").value) || 2;
  const fps = Math.max(2, Math.min(60, parseInt($("vtxFps").value) || 12));
  const kbps = (gw * gh * packetSizeFor(bpc) * fps) / 1024;
  const warn = fps > REFRESH_SAFE_FPS ? `  ※${REFRESH_SAFE_FPS}fps 超は 120Hz 画面向け` : "";
  $("vtxTheory").textContent =
    `理論 ${kbps.toFixed(0)} KB/s  (${$("vtxGrid").value} · ${bpc}bit · ${fps}fps)${warn}`;
}
for (const id of ["vtxGrid", "vtxBpc", "vtxFps"]) {
  $(id).addEventListener("change", updateVtxTheory);
  $(id).addEventListener("input", updateVtxTheory);
}
updateVtxTheory();

$("vtxStart").addEventListener("click", async () => {
  const file = $("vtxFile").files[0];
  const testAsset = $("vtxTest").value;
  let bytes, name, mime;
  if (testAsset) {
    // 同梱の計測用画像。サイズを焼き込んであるので、受信側で開けば
    // どの条件のデータが壊れずに届いたか一目で分かる。
    bytes = new Uint8Array(await (await fetch(testAsset)).arrayBuffer());
    name = testAsset.split("/").pop();
    mime = "image/jpeg";
  } else if (file) {
    bytes = new Uint8Array(await file.arrayBuffer());
    name = file.name;
    mime = file.type || "";
  } else {
    // ファイル未選択ならテキストを送信
    const text = $("vtxText").value;
    if (!text) { $("vtxInfo").textContent = "ファイルを選択するかテキストを入力してください"; return; }
    bytes = new TextEncoder().encode(text);
    name = "message.txt";
    mime = "text/plain;charset=utf-8";
  }
  // 元のファイル名/MIME をヘッダに埋めて送る (受信側で元名・種別をそのまま復元)
  const source = vcodeWrapFile(name, mime, bytes);
  const grid = $("vtxGrid").value;
  const bpc = parseInt($("vtxBpc").value) || 2;
  const fps = Math.max(2, Math.min(60, parseInt($("vtxFps").value) || 12));
  $("vtxInfo").textContent = "";
  fitTxCanvas();
  $("txStage").style.display = "flex";
  activeSender = vcodeSender;
  try {
    await vcodeSender.start(source, grid, bpc, fps);
  } catch (e) {
    $("txStage").style.display = "none";
    $("vtxInfo").textContent = "V送信エラー: " + (e && e.message ? e.message : e);
  }
});

// ---- 受信 ----
const fmtSize = (n) => {
  if (n >= 1 << 30) return (n / (1 << 30)).toFixed(2) + "GB";
  if (n >= 1 << 20) return (n / (1 << 20)).toFixed(1) + "MB";
  if (n >= 1024) return (n / 1024).toFixed(1) + "KB";
  return n + "B";
};

const rxPicker = new CameraPicker($("rxCamera"));
const vrxPicker = new CameraPicker($("vrxCamera"));

const receiver = new Receiver({
  video: $("rxVideo"),
  onDiag: (t) => { $("rxDiag").textContent = t; },
  onProgress: ({ name, size, done, total, inflight }) => {
    $("rxProgress").value = total ? done / total : 0;
    $("rxInfo").textContent = name
      ? `${name} (${fmtSize(size)}) · ブロック ${done}/${total} · 進行中 ${inflight}`
      : "マニフェスト待ち...";
  },
  onDone: ({ name, type, size, blob }) => {
    $("rxProgress").value = 1;
    $("rxInfo").innerHTML = `<span class="ok">✅ 復元成功: ${name} (${fmtSize(size)})</span>`;
    const url = URL.createObjectURL(blob);
    const isImage = type.startsWith("image/");
    $("rxResult").innerHTML =
      (isImage ? `<p><img src="${url}" style="max-width:100%;border-radius:8px" /></p>` : "") +
      `<p><a href="${url}" download="${name}"><button>ダウンロード: ${name}</button></a></p>`;
    $("rxStart").disabled = false;
    $("rxStop").disabled = true;
  },
});

$("rxStart").addEventListener("click", async () => {
  $("rxResult").innerHTML = "";
  $("rxProgress").value = 0;
  try {
    await receiver.start($("rxGrid").value, rxPicker.deviceId);
    rxPicker.refresh(streamDeviceId(receiver.stream)); // 許可後はデバイス名が取れるので一覧を更新
    $("rxStart").disabled = true;
    $("rxStop").disabled = false;
    $("rxInfo").textContent = "スキャン中 — 送信側の QR に向けてください";
  } catch (e) {
    $("rxInfo").textContent = "カメラ起動失敗: " + (e && e.message ? e.message : e);
  }
});
$("rxStop").addEventListener("click", () => {
  receiver.stop();
  $("rxStart").disabled = false;
  $("rxStop").disabled = true;
  $("rxInfo").textContent = "停止しました";
});

// ---- V受信 (vcode) ----
const vcodeReceiver = new VcodeReceiver({
  video: $("vrxVideo"),
  onDiag: (t) => { $("vrxDiag").textContent = t; },
  onProgress: ({ frames, detected, blocks, blocksTotal }) => {
    $("vrxInfo").textContent =
      `スキャン中 · frames ${frames} · 検出 ${detected} · 直近 ${blocks}/${blocksTotal} ブロック`;
  },
  onDone: ({ name, type, size, blob, stats }) => {
    $("vrxInfo").innerHTML = `<span class="ok">✅ 復元成功: ${name} (${fmtSize(size)})</span>`;
    const url = URL.createObjectURL(blob);
    const isImage = type.startsWith("image/");
    // 条件を振って比べられるよう、計測値を一緒に出す (所要時間は初検出→完了)
    const s = stats || {};
    const rows = [
      ["実効スループット", `${(s.kbps || 0).toFixed(1)} KB/s`],
      ["所要時間 (初検出→完了)", `${((s.ms || 0) / 1000).toFixed(2)} 秒`],
      ["フレーム", `${s.detected || 0} 検出 / ${s.frames || 0} 走査`],
      ["スキャン fps", (s.scanFps || 0).toFixed(1)],
      ["格子指定", s.grid === "auto" ? "自動 (候補総当たり)" : s.grid],
    ];
    $("vrxResult").innerHTML =
      (isImage ? `<p><img src="${url}" style="max-width:100%;border-radius:8px" /></p>` : "") +
      `<table class="stats">${rows
        .map(([k, v]) => `<tr><th>${k}</th><td>${v}</td></tr>`)
        .join("")}</table>` +
      `<p><a href="${url}" download="${name}"><button>ダウンロード: ${name}</button></a></p>`;
    $("vrxStart").disabled = false;
    $("vrxStop").disabled = true;
  },
});
$("vrxStart").addEventListener("click", async () => {
  $("vrxResult").innerHTML = "";
  $("vrxError").textContent = "";
  $("vrxInfo").textContent = "スキャン中 — 送信側の vcode を枠に収めてください";
  $("vrxStage").classList.add("active"); // 先に全画面を出す (映像の表示サイズが確定してから走査)
  try {
    await vcodeReceiver.start(vrxPicker.deviceId, $("vrxGrid").value);
    vrxPicker.refresh(streamDeviceId(vcodeReceiver.stream)); // 許可後はデバイス名が取れるので一覧を更新
    setupVrxExpo(); // 露出コントロールを対応状況に合わせて出す
    $("vrxStart").disabled = true;
    $("vrxStop").disabled = false;
  } catch (e) {
    $("vrxStage").classList.remove("active");
    $("vrxError").textContent = "カメラ起動失敗: " + (e && e.message ? e.message : e);
  }
});
// 受信中でも格子指定を切り替えられる (総当たり→固定で初回検出が速くなる)
$("vrxGrid").addEventListener("change", () => vcodeReceiver.setGrid($("vrxGrid").value));
$("vrxStop").addEventListener("click", () => {
  vcodeReceiver.stop();
  $("vrxStage").classList.remove("active");
  $("vrxStart").disabled = false;
  $("vrxStop").disabled = true;
});

// V受信の手動露出。露出制御対応カメラでのみスライダーを出す (スマホ Chrome は非対応が
// 多く、その場合は非対応の注記を出す)。手動中は自動制御 (白飛び対策) を止める。
const vrxExpoAuto = $("vrxExpoAuto"), vrxExpoRange = $("vrxExpoRange"), vrxExpoVal = $("vrxExpoVal");
function setupVrxExpo() {
  const g = vcodeReceiver.exposure;
  const ok = g && g.supported;
  $("vrxExpo").hidden = !ok;
  $("vrxExpoNote").hidden = ok;
  if (!ok) return;
  vrxExpoRange.min = g.min;
  vrxExpoRange.max = g.max;
  vrxExpoRange.step = Math.max(1, Math.round((g.max - g.min) / 100));
  vrxExpoRange.value = g.currentValue();
  vrxExpoAuto.checked = true;
  vrxExpoRange.disabled = true;
  vrxExpoVal.textContent = "";
}
vrxExpoAuto.addEventListener("change", () => {
  const g = vcodeReceiver.exposure;
  if (!g) return;
  if (vrxExpoAuto.checked) {
    g.setAuto();
    vrxExpoRange.disabled = true;
    vrxExpoVal.textContent = "";
  } else {
    vrxExpoRange.disabled = false;
    g.setManual(parseInt(vrxExpoRange.value));
    vrxExpoVal.textContent = g.valueText();
  }
});
vrxExpoRange.addEventListener("input", () => {
  const g = vcodeReceiver.exposure;
  if (!g || vrxExpoAuto.checked) return;
  g.setManual(parseInt(vrxExpoRange.value));
  vrxExpoVal.textContent = g.valueText();
});

// ---- 起動 ----
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
  window.addEventListener("load", () => {
    navigator.serviceWorker.register("./sw.js").catch(() => {});
  });
}
