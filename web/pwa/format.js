// 表示用の整形 (サイズ・時間)。送信・受信・目安の表示で共用する。

/** バイト数を「1.2MB」の形にする */
export const fmtSize = (n) => {
  if (n >= 1 << 30) return (n / (1 << 30)).toFixed(2) + "GB";
  if (n >= 1 << 20) return (n / (1 << 20)).toFixed(1) + "MB";
  if (n >= 1024) return (n / 1024).toFixed(1) + "KB";
  return n + "B";
};

/** 見込みの秒数を「約 8 秒」「約 1 分 20 秒」の形にする (目安なので細かく出さない) */
export function fmtDuration(sec) {
  if (sec < 1) return "1 秒未満";
  if (sec < 10) return `約 ${sec.toFixed(1)} 秒`;
  if (sec < 60) return `約 ${Math.round(sec)} 秒`;
  const m = Math.floor(sec / 60);
  const s = Math.round((sec - m * 60) / 10) * 10;
  if (s === 60) return `約 ${m + 1} 分`;
  return s ? `約 ${m} 分 ${s} 秒` : `約 ${m} 分`;
}

/** 経過した秒数を「12 秒」「1 分 05 秒」の形にする */
export function fmtElapsed(sec) {
  const t = Math.floor(sec);
  return t < 60 ? `${t} 秒` : `${Math.floor(t / 60)} 分 ${String(t % 60).padStart(2, "0")} 秒`;
}

/** ファイル名に付ける時刻 (YYYYMMDD_HHMMSS) */
export function fmtStamp(t) {
  const pad = (n) => String(n).padStart(2, "0");
  return `${t.getFullYear()}${pad(t.getMonth() + 1)}${pad(t.getDate())}_` +
    `${pad(t.getHours())}${pad(t.getMinutes())}${pad(t.getSeconds())}`;
}
