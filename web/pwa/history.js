// 受信履歴。受け取ったファイルを、この端末のブラウザ内 (IndexedDB) に残す。
//
// サーバーには何も送らない (Pages は静的配信なので置き場が無い)。消える条件は
// ブラウザ次第で、iOS の Safari は 7 日間開かないサイトのデータを消すことがある
// (ホーム画面に追加したものは対象外)。確実に残したいものは「保存」で書き出す。

const DB_NAME = "vloom";
const STORE = "received";
/** 残す件数。古いものから消す (端末の容量を食い続けないように) */
export const HISTORY_KEEP = 30;

let dbPromise = null;
function db() {
  if (!dbPromise) {
    dbPromise = new Promise((resolve, reject) => {
      const req = indexedDB.open(DB_NAME, 1);
      req.onupgradeneeded = () =>
        req.result.createObjectStore(STORE, { keyPath: "id", autoIncrement: true });
      req.onsuccess = () => resolve(req.result);
      req.onerror = () => reject(req.error);
    });
    dbPromise.catch(() => { dbPromise = null; });
  }
  return dbPromise;
}

function run(mode, fn) {
  return db().then((d) => new Promise((resolve, reject) => {
    const tx = d.transaction(STORE, mode);
    const out = fn(tx.objectStore(STORE));
    tx.oncomplete = () => resolve(out && "result" in out ? out.result : undefined);
    tx.onerror = () => reject(tx.error);
    tx.onabort = () => reject(tx.error);
  }));
}

/** 新しい順に全件 */
export async function listHistory() {
  const all = await run("readonly", (s) => s.getAll());
  return (all || []).sort((a, b) => b.at - a.at);
}

/** 1 件追加して、古いものを [HISTORY_KEEP] 件まで減らす */
export async function addHistory({ name, type, size, blob, kbps }) {
  // Blob のままより ArrayBuffer のほうが古い Safari でも確実に入る
  const data = await blob.arrayBuffer();
  await run("readwrite", (s) => s.add({ name, type, size, kbps, at: Date.now(), data }));
  const all = await listHistory();
  const old = all.slice(HISTORY_KEEP);
  if (old.length) await run("readwrite", (s) => { for (const r of old) s.delete(r.id); });
  // 対応ブラウザでは、容量が逼迫しても消されにくくする (断られても動作は同じ)
  try { if (navigator.storage && navigator.storage.persist) await navigator.storage.persist(); } catch (_) { /* 任意 */ }
}

export function deleteHistory(id) {
  return run("readwrite", (s) => { s.delete(id); });
}

export function clearHistory() {
  return run("readwrite", (s) => { s.clear(); });
}
