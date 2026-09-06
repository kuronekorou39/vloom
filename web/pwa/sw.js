// Vloom PWA Service Worker: アプリシェル + WASM をキャッシュしてオフライン動作させる。
// バージョンを上げるとキャッシュを更新する。
const CACHE = "vloom-pwa-v44";
const ASSETS = [
  "./",
  "./index.html",
  "./app.js",
  "./vcode.js",
  "./scan-worker.js",
  "./calibration.js",
  "./camera.js",
  "./manifest.webmanifest",
  "./icon-192.png",
  "./icon-512.png",
  "./icon-maskable-512.png",
  "./apple-touch-icon.png",
  "./favicon.png",
  // 計測用画像 (testdata/) は数MBあるため事前キャッシュせず、使用時に取得する
  "./pkg/vloom_core_wasm.js",
  "./pkg/vloom_core_wasm_bg.wasm",
];

self.addEventListener("install", (e) => {
  // HTTP キャッシュを迂回して取り直す。addAll の既定はブラウザのキャッシュを使うので、
  // 配信直後でエッジや端末に古い応答が残っていると、それを新しいバージョンの
  // キャッシュに焼き込んでしまう。実機で「キャッシュは v42 なのに中身は旧版」が
  // 起きて、直したはずの挙動が出ないまま計測してしまった。
  e.waitUntil(
    caches.open(CACHE).then(async (c) => {
      await Promise.all(ASSETS.map(async (u) => {
        const res = await fetch(u, { cache: "reload" });
        if (res.ok) await c.put(u, res);
      }));
    }).then(() => self.skipWaiting())
  );
});

self.addEventListener("activate", (e) => {
  e.waitUntil(
    caches.keys().then((keys) =>
      Promise.all(keys.filter((k) => k !== CACHE).map((k) => caches.delete(k)))
    ).then(() => self.clients.claim())
  );
});

// cache-first (オフライン優先)。無ければネットワーク。
self.addEventListener("fetch", (e) => {
  if (e.request.method !== "GET") return;
  e.respondWith(
    caches.match(e.request).then((hit) => hit || fetch(e.request).then((res) => {
      const copy = res.clone();
      caches.open(CACHE).then((c) => c.put(e.request, copy)).catch(() => {});
      return res;
    }).catch(() => hit))
  );
});
