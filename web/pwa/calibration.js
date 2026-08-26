// PC PWA 校正モード。スマホアプリ (calibration_screen.dart) と同じ思想:
//   送信(表示): ゆるい→きついレベルのテストフレームを大きく表示
//   受信(確認): カメラで読み、どの密度まで検出できるか (✅) を確認
// 検出できた最も密なレベルが、その環境で使える上限の目安。

import { VcodeTx, VcodeRx } from "./pkg/vloom_core_wasm.js";
import { VCODE_GUIDE_FRAC } from "./vcode.js";
import { openCamera, CameraPicker, ScanStats, ExposureGuard, streamDeviceId, cameraInfoText, lumaText, cellPxText }
  from "./camera.js";

const DIAG_INTERVAL_MS = 500;

// スキャナの CANDIDATES は 5×4 / 7×6 のみ検出可能なため、この2択に絞る
const CAL_LEVELS = [
  { label: "Lv1  5×4 標準", gw: 5, gh: 4 },
  { label: "Lv2  7×6 高密度", gw: 7, gh: 6 },
];

// ---- 表示 (テストフレームを canvas に描く) ----

function drawLevel(ctx, canvas, levelIndex) {
  const lv = CAL_LEVELS[levelIndex];
  // 本番 V送信 の既定と同じ 2bit (4値) で描く
  const tx = new VcodeTx(new TextEncoder().encode("VCAL-CALIBRATION-PATTERN"), 4, lv.gw, lv.gh, 2);
  const w = tx.frameWidth(), h = tx.frameHeight();
  const gray = tx.frameGray(0);
  const off = document.createElement("canvas");
  off.width = w; off.height = h;
  const octx = off.getContext("2d");
  const id = octx.createImageData(w, h);
  for (let p = 0; p < w * h; p++) {
    const v = gray[p];
    id.data[p * 4] = v; id.data[p * 4 + 1] = v; id.data[p * 4 + 2] = v; id.data[p * 4 + 3] = 255;
  }
  octx.putImageData(id, 0, 0);
  ctx.fillStyle = "#fff";
  ctx.fillRect(0, 0, canvas.width, canvas.height);
  ctx.imageSmoothingEnabled = false;
  const scale = Math.min(canvas.width / w, canvas.height / h);
  const dw = Math.floor(w * scale), dh = Math.floor(h * scale);
  const dx = ((canvas.width - dw) / 2) | 0, dy = ((canvas.height - dh) / 2) | 0;
  ctx.drawImage(off, 0, 0, w, h, dx, dy, dw, dh);
}

// ---- 受信 (どの密度まで検出できるか) ----

function detectLevel(rx, imgData, found, onBlocks) {
  const { width, height, data } = imgData;
  const gray = new Uint8Array(width * height);
  for (let p = 0, q = 0; p < gray.length; p++, q += 4) {
    gray[p] = (data[q] * 77 + data[q + 1] * 150 + data[q + 2] * 29) >> 8;
  }
  let report;
  try {
    report = rx.scan(gray, width, height, width, 0, VCODE_GUIDE_FRAC);
  } catch (_) { return; }
  if (report.detected && report.blocksTotal > 0) {
    onBlocks(report.blocksOk, report.blocksTotal);
    if (report.blocksOk * 10 >= report.blocksTotal * 8) {
      const idx = CAL_LEVELS.findIndex((l) => l.gw * l.gh === report.blocksTotal);
      if (idx >= 0) found.add(idx);
    }
  }
}

/**
 * 校正 UI を結線する。els は index.html の各要素。
 */
export function setupCalibration(els) {
  let level = 0;

  // ---- 表示 (フルスクリーン) ----
  const ctx = els.canvas.getContext("2d");
  const redraw = () => {
    drawLevel(ctx, els.canvas, level);
    els.label.textContent = `${CAL_LEVELS[level].label}   (${level + 1}/${CAL_LEVELS.length})`;
  };
  const openStage = () => { els.stage.style.display = "flex"; redraw(); };
  const closeStage = () => { els.stage.style.display = "none"; };
  els.showBtn.addEventListener("click", openStage);
  els.close.addEventListener("click", closeStage);
  els.prev.addEventListener("click", () => { if (level > 0) { level--; redraw(); } });
  els.next.addEventListener("click", () => {
    if (level < CAL_LEVELS.length - 1) { level++; redraw(); }
  });

  // ---- 受信 (カメラ確認) ----
  let stream = null, rafId = null, rx = null;
  const readable = new Set();
  const cap = document.createElement("canvas");
  const picker = new CameraPicker(els.cameraSelect);
  let stats = new ScanStats();
  let exposure = null;
  let lastDiag = 0;

  // カメラ実解像度・スキャン fps・明るさ・理論 px/セル の実測。
  const diag = (crop, target) => {
    const now = performance.now();
    if (now - lastDiag < DIAG_INTERVAL_MS) return;
    lastDiag = now;
    if (exposure) exposure.update(stats);
    const size = crop === target ? `${target}px` : `${crop}→${target}px`;
    els.diag.textContent =
      `${cameraInfoText(stream)}\n` +
      `スキャン ${size} · ${stats.fps.toFixed(1)} fps · ${lumaText(stats, exposure)}\n` +
      cellPxText(target * VCODE_GUIDE_FRAC);
  };

  const renderChips = () => {
    const lvs = CAL_LEVELS;
    const best = readable.size ? Math.max(...readable) : -1;
    els.best.textContent = best < 0
      ? "送信側のテストパターンに向けてください"
      : `✅ 読めた最密: ${lvs[best].label}`;
    els.chips.innerHTML = "";
    for (let i = 0; i < lvs.length; i++) {
      const chip = document.createElement("span");
      chip.className = "chip" + (readable.has(i) ? " ok" : "");
      chip.textContent = (readable.has(i) ? "✅ " : "○ ") + `Lv${i + 1}`;
      els.chips.appendChild(chip);
    }
  };

  const scanLoop = () => {
    if (!stream) return;
    const vw = els.video.videoWidth, vh = els.video.videoHeight;
    if (vw && vh) {
      const crop = Math.min(vw, vh);
      const target = Math.min(1280, crop);
      const cx = (vw - crop) >> 1, cy = (vh - crop) >> 1;
      cap.width = target; cap.height = target;
      const cctx = cap.getContext("2d", { willReadFrequently: true });
      cctx.drawImage(els.video, cx, cy, crop, crop, 0, 0, target, target);
      const img = cctx.getImageData(0, 0, target, target);
      stats.tick(img.data, 4);
      diag(crop, target);
      const before = readable.size;
      detectLevel(rx, img, readable, (ok, total) => {
        els.blocks.textContent = `直近: ${ok} / ${total} ブロック検出`;
      });
      if (readable.size !== before) renderChips();
    }
    rafId = requestAnimationFrame(scanLoop);
  };

  const startRecv = async () => {
    stopRecv();
    readable.clear();
    els.blocks.textContent = "";
    renderChips();
    stats = new ScanStats();
    lastDiag = 0;
    try {
      stream = await openCamera(picker.deviceId);
    } catch (e) {
      els.best.textContent = "カメラ起動失敗: " + (e && e.message ? e.message : e);
      return;
    }
    exposure = new ExposureGuard(stream); // スマホ画面の白飛び対策
    picker.refresh(streamDeviceId(stream)); // 許可後はデバイス名が取れるので一覧を更新
    els.video.srcObject = stream;
    await els.video.play();
    rx = new VcodeRx();
    els.recvStart.disabled = true;
    els.recvStop.disabled = false;
    rafId = requestAnimationFrame(scanLoop);
  };

  const stopRecv = () => {
    if (rafId) cancelAnimationFrame(rafId);
    rafId = null;
    if (stream) { stream.getTracks().forEach((t) => t.stop()); stream = null; }
    rx = null;
    els.recvStart.disabled = false;
    els.recvStop.disabled = true;
  };

  els.recvStart.addEventListener("click", startRecv);
  els.recvStop.addEventListener("click", stopRecv);
  els.recvReset.addEventListener("click", () => { readable.clear(); els.blocks.textContent = ""; renderChips(); });

  // ---- モード切替 ----
  const setMode = (m) => {
    stopRecv();
    els.modeBtns.forEach((b) => b.classList.toggle("active", b.dataset.m === m));
    els.sendView.style.display = m === "send" ? "block" : "none";
    els.recvView.style.display = m === "recv" ? "block" : "none";
  };
  els.modeBtns.forEach((b) => b.addEventListener("click", () => setMode(b.dataset.m)));

  renderChips();

  // タブ離脱時にカメラ/表示を止めるためのフック
  return { stop: () => { stopRecv(); closeStage(); } };
}
