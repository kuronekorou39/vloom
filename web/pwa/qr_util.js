// Vloom web 共有ユーティリティ。
// - drawQRInCell     : 1 セルに quiet zone 付きで QR を描画 (sender.html / test_multiqr.html)
// - detectQRCodesGrid: 1 枚の ImageData を rows×cols にタイル分割し各タイルを jsQR で読む
//                      (receiver.html / test_multiqr.html)
// jsQR / qrcode-generator は呼び出し側で <script> ロード済みである前提 (globalThis 経由で参照)。
//
// 注意: jsQR は「1 枚の画像 = 1 個の QR」を前提に作られており、複数 QR が写っていると
// ファインダーを 1 個に組み立てようとして失敗し 0 個になる。そのため複数 QR は
// 「グリッドで切り出して 1 枚ずつ読む」方式を採る。各 QR の周囲 quiet zone がガターになり分離できる。

/**
 * QR を矩形セル [x, y, w, h] の中央に、周囲へ quiet zone を残して描画する。
 * 背景 (白) は呼び出し側で塗ってある前提。黒モジュールのみ塗る。
 * @param {CanvasRenderingContext2D} ctx
 * @param {{getModuleCount():number, isDark(r:number,c:number):boolean}} qr - qrcode-generator のインスタンス
 */
export function drawQRInCell(ctx, qr, x, y, w, h) {
  const modules = qr.getModuleCount();
  const square = Math.min(w, h);
  const margin = Math.round(square * 0.08); // quiet zone (= タイル分割時のガター)
  const cellSize = Math.max(1, Math.floor((square - margin * 2) / modules));
  const qrPx = cellSize * modules;
  const ox = Math.round(x + (w - qrPx) / 2);
  const oy = Math.round(y + (h - qrPx) / 2);
  ctx.fillStyle = "#000";
  for (let r = 0; r < modules; r++) {
    for (let c = 0; c < modules; c++) {
      if (qr.isDark(r, c)) ctx.fillRect(ox + c * cellSize, oy + r * cellSize, cellSize, cellSize);
    }
  }
}

/**
 * ImageData を rows×cols のタイルに分割し、各タイルを jsQR で読んで binaryData (Uint8Array) の配列で返す。
 * overlap は隣タイルへの食い込み割合 (撮影フレームのズレ吸収用。QR 間の quiet zone より小さくして
 * 隣の QR のファインダーを含めないこと)。送受信でグリッドを一致させて使う。
 * rows=cols=1 のときは全画面 1 回 = 従来の単一 QR 読み取りと等価。
 */
export function detectQRCodesGrid(imgData, rows, cols, overlap = 0.06) {
  const jsQR = window.jsQR;
  const out = [];
  const tileW = imgData.width / cols;
  const tileH = imgData.height / rows;
  const ox = Math.round(tileW * overlap);
  const oy = Math.round(tileH * overlap);
  for (let r = 0; r < rows; r++) {
    for (let c = 0; c < cols; c++) {
      const x0 = Math.max(0, Math.floor(c * tileW) - ox);
      const y0 = Math.max(0, Math.floor(r * tileH) - oy);
      const x1 = Math.min(imgData.width, Math.ceil((c + 1) * tileW) + ox);
      const y1 = Math.min(imgData.height, Math.ceil((r + 1) * tileH) + oy);
      const w = x1 - x0, h = y1 - y0;
      if (w < 16 || h < 16) continue;
      const tile = cropImageData(imgData, x0, y0, w, h);
      const code = jsQR(tile.data, w, h, { inversionAttempts: "dontInvert" });
      if (code && code.binaryData && code.binaryData.length) out.push(new Uint8Array(code.binaryData));
    }
  }
  return out;
}

// ImageData の矩形 [x0, y0, w, h] を新しい {data,width,height} に切り出す (jsQR へ渡す用)。
function cropImageData(src, x0, y0, w, h) {
  const data = new Uint8ClampedArray(w * h * 4);
  const sw = src.width;
  for (let y = 0; y < h; y++) {
    const s = ((y0 + y) * sw + x0) * 4;
    data.set(src.data.subarray(s, s + w * 4), y * w * 4);
  }
  return { data, width: w, height: h };
}
