// beyond-qr PWA Service Worker: アプリシェル + WASM をキャッシュしてオフライン動作させる。
// バージョンを上げるとキャッシュを更新する。
const CACHE = "beyond-qr-pwa-v8";
const ASSETS = [
  "./",
  "./index.html",
  "./app.js",
  "./sender.js",
  "./receiver.js",
  "./vcode.js",
  "./calibration.js",
  "./camera.js",
  "./protocol.js",
  "./qr_util.js",
  "./vendor/qrcode.js",
  "./vendor/jsQR.js",
  "./manifest.webmanifest",
  "./pkg/beyond_qr_core_wasm.js",
  "./pkg/beyond_qr_core_wasm_bg.wasm",
];

self.addEventListener("install", (e) => {
  e.waitUntil(caches.open(CACHE).then((c) => c.addAll(ASSETS)).then(() => self.skipWaiting()));
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
