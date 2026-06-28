// Service worker for the Wet Court console PWA.
//
// Strategy:
//   - Precache the SPA shell ('/') on install.
//   - Navigations: network-first, fall back to the cached shell when offline.
//   - Static assets (hashed Vite bundle, icons, manifest): stale-while-revalidate.
//   - Live endpoints (/ws, /operator/*, /maintenance/*, /health) and the SW
//     script itself: never intercepted — always straight to the network.
//
// Bump CACHE to invalidate old precaches on a breaking change.

const CACHE = 'wetcourt-v1';
const APP_SHELL = ['/'];

self.addEventListener('install', (event) => {
  event.waitUntil(
    caches.open(CACHE).then((c) => c.addAll(APP_SHELL)).then(() => self.skipWaiting()),
  );
});

self.addEventListener('activate', (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) => Promise.all(keys.filter((k) => k !== CACHE).map((k) => caches.delete(k))))
      .then(() => self.clients.claim()),
  );
});

// Live/control endpoints that must always hit the network.
function isNetworkOnly(pathname) {
  return /^\/(ws|operator|maintenance|health)(\/|$)/.test(pathname) || pathname === '/sw.js';
}

self.addEventListener('fetch', (event) => {
  const req = event.request;
  if (req.method !== 'GET') return; // mutations go straight to the network
  const url = new URL(req.url);
  if (url.origin !== self.location.origin) return; // ignore cross-origin
  if (isNetworkOnly(url.pathname)) return;

  // SPA navigations: network-first so the latest shell wins online; cached
  // shell keeps the app launchable offline (the client router reads the path).
  if (req.mode === 'navigate') {
    event.respondWith(
      fetch(req)
        .then((res) => {
          const copy = res.clone();
          caches.open(CACHE).then((c) => c.put('/', copy)).catch(() => {});
          return res;
        })
        .catch(() => caches.match('/')),
    );
    return;
  }

  // Static assets: serve cache fast, refresh in the background.
  event.respondWith(
    caches.open(CACHE).then((cache) =>
      cache.match(req).then((cached) => {
        const network = fetch(req)
          .then((res) => {
            if (res && res.ok) cache.put(req, res.clone());
            return res;
          })
          .catch(() => cached);
        return cached || network;
      }),
    ),
  );
});
