// 校正: どの格子まで読めるかを確かめる。
//   送信(表示): 粗い→密な格子のテスト図柄を、送信と同じく画面いっぱいに表示する
//   受信(確認): 通常の受信と同じ全画面カメラで読み、格子ごとに何割のブロックが
//               読めたかを数える (受信画面側の処理は app.js。ここは表示と判定の基準)
// 読めた最も密な格子が、その組み合わせで使える上限の目安になる。

import { VcodeTx } from "./pkg/vloom_core_wasm.js";

/** 1 枚のうちこの割合以上のブロックが読めたら「読める」とする */
export const CAL_OK_RATIO = 0.8;

// 受信の自動判別 (Rust の Layout::CANDIDATES) が探す格子だけを並べる。
// それ以外の格子 (9×8 など) は受信側で格子を指定しないと検出されず、
// 校正では「読めない」と誤判定してしまう
export const CAL_LEVELS = [
  { grid: "5x4", gw: 5, gh: 4 },
  { grid: "7x6", gw: 7, gh: 6 },
  { grid: "11x14", gw: 11, gh: 14 },
  { grid: "13x18", gw: 13, gh: 18 },
];
export const calLabel = (i) => `Lv${i + 1}  ${CAL_LEVELS[i].grid.replace("x", "×")}`;
/** ブロック総数から格子を引く (候補の格子はブロック数がすべて異なる) */
export const calLevelOf = (blocksTotal) =>
  CAL_LEVELS.findIndex((l) => l.gw * l.gh === blocksTotal);

/** 表示の外周に残す白の割合 (送信画面と同じく、枠の外にも少し白を見せる) */
const FIT = 0.94;

function drawLevel(ctx, canvas, levelIndex) {
  const lv = CAL_LEVELS[levelIndex];
  // 実際の転送の既定と同じ 1bit (白黒) で描く
  const tx = new VcodeTx(new TextEncoder().encode("VCAL-CALIBRATION-PATTERN"), 4, lv.gw, lv.gh, 1);
  const w = tx.frameWidth(), h = tx.frameHeight();
  const gray = tx.frameGray(0);
  tx.free();
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
  const scale = Math.min(canvas.width / w, canvas.height / h) * FIT;
  const dw = Math.floor(w * scale), dh = Math.floor(h * scale);
  const dx = ((canvas.width - dw) / 2) | 0, dy = ((canvas.height - dh) / 2) | 0;
  ctx.drawImage(off, 0, 0, w, h, dx, dy, dw, dh);
}

/** 校正の表示側を結線する。els は index.html の各要素。 */
export function setupCalibration(els) {
  let level = 0;
  const ctx = els.canvas.getContext("2d");
  const fitCanvas = () => {
    // 下のバーを除いた画面いっぱいに描く (以前は正方形 1080px 固定で、縦長の格子が小さかった)
    const dpr = window.devicePixelRatio || 1;
    const cssW = window.innerWidth;
    const cssH = Math.max(1, window.innerHeight - els.bar.offsetHeight);
    els.canvas.style.width = `${cssW}px`;
    els.canvas.style.height = `${cssH}px`;
    els.canvas.width = Math.round(cssW * dpr);
    els.canvas.height = Math.round(cssH * dpr);
  };
  const redraw = () => {
    fitCanvas();
    drawLevel(ctx, els.canvas, level);
    els.label.textContent = `${calLabel(level)}   (${level + 1}/${CAL_LEVELS.length})`;
    els.prev.disabled = level === 0;
    els.next.disabled = level === CAL_LEVELS.length - 1;
  };
  const isOpen = () => els.stage.style.display === "flex";
  const onResize = () => { if (isOpen()) redraw(); };
  const openStage = () => {
    els.stage.style.display = "flex";
    window.addEventListener("resize", onResize);
    redraw();
  };
  const closeStage = () => {
    els.stage.style.display = "none";
    window.removeEventListener("resize", onResize);
  };
  els.showBtn.addEventListener("click", openStage);
  els.close.addEventListener("click", closeStage);
  els.prev.addEventListener("click", () => { if (level > 0) { level--; redraw(); } });
  els.next.addEventListener("click", () => {
    if (level < CAL_LEVELS.length - 1) { level++; redraw(); }
  });

  // ---- モード切替 ----
  const setMode = (m) => {
    els.modeBtns.forEach((b) => b.classList.toggle("active", b.dataset.m === m));
    els.sendView.hidden = m !== "send";
    els.recvView.hidden = m !== "recv";
  };
  els.modeBtns.forEach((b) => b.addEventListener("click", () => setMode(b.dataset.m)));

  // タブ離脱時に表示を止めるためのフック
  return { stop: closeStage };
}
