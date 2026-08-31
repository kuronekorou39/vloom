// Vloom PC PWA — エントリ (結線)。
// Rust コア (fountain + vcode) を WASM で初期化し、送信 / 受信を UI に繋ぐ。

import init, { FountainEncoder, FountainDecoder, vcodeWrapFile } from "./pkg/vloom_core_wasm.js";
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
  $("txSizeVal").textContent = `${$("txSize").value}%`;
});
$("txHold").addEventListener("change", () => sender.setHold($("txHold").checked));
setInterval(() => { if (sender.running) $("txGeom").textContent = sender.info(); }, 500);

// 送信の輝度/モアレ調整。キャンバスへの CSS フィルタで効かせる。
// 輝度=白レベルを下げて受信側の白飛びを抑える。
// モアレ=軽いぼかしで表示グリッドとカメラ画素の干渉縞を減らす (格子を粗くするのも有効)。
function applyTxFilter() {
  const b = (parseInt($("txBright").value) || 100) / 100;
  const blur = parseFloat($("txBlur").value) || 0;
  $("txCanvas").style.filter = `brightness(${b}) blur(${blur}px)`;
}
$("txBright").addEventListener("input", applyTxFilter);
$("txBlur").addEventListener("input", applyTxFilter);
applyTxFilter();

$("txStop").addEventListener("click", () => {
  sender.stop();
  $("txStage").style.display = "none";
});

// 設定の理論スループット (ブロック数 × packet_size × fps) を出す。実測との比較基準。
function updateTheory() {
  const [gw, gh] = $("txGrid").value.split("x").map(Number);
  const bpc = parseInt($("txBpc").value) || 2;
  const fps = Math.max(2, Math.min(60, parseInt($("txFps").value) || 12));
  const kbps = (gw * gh * packetSizeFor(bpc) * fps) / 1024;
  const warn = fps > REFRESH_SAFE_FPS ? `  ※${REFRESH_SAFE_FPS}fps 超は 120Hz 画面向け` : "";
  $("txTheory").textContent =
    `理論 ${kbps.toFixed(0)} KB/s  (${$("txGrid").value} · ${bpc}bit · ${fps}fps)${warn}`;
}
for (const id of ["txGrid", "txBpc", "txFps"]) {
  $(id).addEventListener("change", updateTheory);
  $(id).addEventListener("input", updateTheory);
}
// 既定 fps: スマホ (タッチ端末) は 30 (OLED は切り替わりが速く 60Hz の 2 書き換えごとが最速)。
// PC モニタは LCD が多く、残像で 30fps は逆に遅いので 20 のまま
if (navigator.maxTouchPoints > 0 && Math.min(screen.width, screen.height) < 900) {
  $("txFps").value = 30;
}
updateTheory();

$("txStart").addEventListener("click", async () => {
  const file = $("txFile").files[0];
  const testAsset = $("txTest").value;
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
    const text = $("txText").value;
    if (!text) { $("txInfo").textContent = "ファイルを選択するかテキストを入力してください"; return; }
    bytes = new TextEncoder().encode(text);
    // 毎回 message.txt だと受信側で同名ファイルが積み重なるので、送信時刻を付ける
    const t = new Date();
    const pad = (n) => String(n).padStart(2, "0");
    name = `message_${t.getFullYear()}${pad(t.getMonth() + 1)}${pad(t.getDate())}_${pad(t.getHours())}${pad(t.getMinutes())}${pad(t.getSeconds())}.txt`;
    mime = "text/plain;charset=utf-8";
  }
  // 元のファイル名/MIME をヘッダに埋めて送る (受信側で元名・種別をそのまま復元)
  const source = vcodeWrapFile(name, mime, bytes);
  const grid = $("txGrid").value;
  const bpc = parseInt($("txBpc").value) || 2;
  const fps = Math.max(2, Math.min(60, parseInt($("txFps").value) || 12));
  $("txInfo").textContent = "";
  $("txStage").style.display = "flex";
  fitTxCanvas();
  showTxBar();
  try {
    await sender.start(source, grid, bpc, fps);
  } catch (e) {
    $("txStage").style.display = "none";
    $("txInfo").textContent = "送信エラー: " + (e && e.message ? e.message : e);
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

const receiver = new VcodeReceiver({
  video: $("rxVideo"),
  onDiag: (t) => { $("rxDiag").textContent = t; },
  onProgress: ({ frames, detected, blocks, blocksTotal }) => {
    $("rxInfo").textContent =
      `スキャン中 · frames ${frames} · 検出 ${detected} · 直近 ${blocks}/${blocksTotal} ブロック`;
  },
  onDone: ({ name, type, size, blob, stats }) => {
    $("rxInfo").innerHTML = `<span class="ok">✅ 復元成功: ${name} (${fmtSize(size)})</span>`;
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
    $("rxResult").innerHTML =
      (isImage ? `<p><img src="${url}" style="max-width:100%;border-radius:8px" /></p>` : "") +
      `<table class="stats">${rows
        .map(([k, v]) => `<tr><th>${k}</th><td>${v}</td></tr>`)
        .join("")}</table>` +
      `<p><a href="${url}" download="${name}"><button>ダウンロード: ${name}</button></a></p>`;
    $("rxStart").disabled = false;
    $("rxStop").disabled = true;
  },
});
$("rxStart").addEventListener("click", async () => {
  $("rxResult").innerHTML = "";
  $("rxError").textContent = "";
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
});
// 受信中でも格子指定を切り替えられる (総当たり→固定で初回検出が速くなる)
$("rxGrid").addEventListener("change", () => receiver.setGrid($("rxGrid").value));
$("rxStop").addEventListener("click", () => {
  receiver.stop();
  $("rxStage").classList.remove("active");
  $("rxStart").disabled = false;
  $("rxStop").disabled = true;
});

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
  window.addEventListener("load", () => {
    navigator.serviceWorker.register("./sw.js").catch(() => {});
  });
}
