/*
 * fleett service worker.
 *
 * The console is a hash-routed SPA behind a cookie session, so the rules are
 * deliberately narrow:
 *   - navigations        network first, then the cached shell, then offline.html
 *   - /api/* and /auth/* never cached, never replayed
 *   - hashed /assets/*   cache first (the build guarantees a new name)
 *   - the shell + icons  stale-while-revalidate
 *
 * No build step touches this file: bump VERSION when the shell list changes so
 * activate() drops the previous caches.
 */
const VERSION = "v1";
const SHELL_CACHE = `fleett-shell-${VERSION}`;
const ASSET_CACHE = `fleett-assets-${VERSION}`;

const SHELL = [
  "/",
  "/index.html",
  "/offline.html",
  "/manifest.webmanifest",
  "/favicon.svg",
  "/icons/icon-192.png",
  "/icons/icon-512.png",
  "/icons/apple-touch-icon.png",
];

/** Same-origin files worth keeping around when the network is gone. */
const STATIC_FILE = /\.(?:css|js|mjs|svg|png|jpg|jpeg|webp|ico|woff2?|ttf|webmanifest|txt)$/i;

self.addEventListener("install", (event) => {
  event.waitUntil(
    (async () => {
      const shell = await caches.open(SHELL_CACHE);
      // Individually so one missing optional file cannot fail the install.
      await Promise.all(SHELL.map((path) => shell.add(path).catch(() => undefined)));

      // Precache the hashed entry points referenced by the shell.
      const cache = await caches.open(ASSET_CACHE);
      try {
        const response = await fetch("/index.html", { cache: "no-store" });
        if (response.ok) {
          const html = await response.text();
          const assets = new Set(
            [...html.matchAll(/(?:src|href)="(\/assets\/[^"]+)"/g)].map((match) => match[1]),
          );
          await Promise.all([...assets].map((path) => cache.add(path).catch(() => undefined)));
        }
      } catch {
        /* offline install: the next successful navigation fills the cache */
      }

      await self.skipWaiting();
    })(),
  );
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    (async () => {
      const keep = new Set([SHELL_CACHE, ASSET_CACHE]);
      const names = await caches.keys();
      await Promise.all(names.filter((name) => !keep.has(name)).map((name) => caches.delete(name)));
      await self.clients.claim();
    })(),
  );
});

self.addEventListener("fetch", (event) => {
  const request = event.request;
  if (request.method !== "GET") return;

  const url = new URL(request.url);
  if (url.origin !== self.location.origin) return;

  // Credentials and live state: straight to the network, never stored.
  if (url.pathname.startsWith("/api/") || url.pathname.startsWith("/auth/")) return;

  if (request.mode === "navigate") {
    event.respondWith(navigation(request));
    return;
  }

  if (url.pathname.startsWith("/assets/")) {
    event.respondWith(cacheFirst(request, ASSET_CACHE));
    return;
  }

  if (STATIC_FILE.test(url.pathname)) {
    event.respondWith(staleWhileRevalidate(request, SHELL_CACHE));
  }
});

async function navigation(request) {
  const cache = await caches.open(SHELL_CACHE);
  try {
    const response = await fetch(request);
    const path = new URL(request.url).pathname;
    if (response.ok && !response.redirected && path !== "/offline.html") {
      // Keep one canonical copy of the shell, whatever path was requested.
      await cache.put("/index.html", response.clone());
    }
    return response;
  } catch {
    return (
      (await cache.match("/index.html")) ||
      (await cache.match("/offline.html")) ||
      Response.error()
    );
  }
}

async function cacheFirst(request, cacheName) {
  const cache = await caches.open(cacheName);
  const hit = await cache.match(request);
  if (hit) return hit;
  const response = await fetch(request);
  if (response.ok) await cache.put(request, response.clone());
  return response;
}

async function staleWhileRevalidate(request, cacheName) {
  const cache = await caches.open(cacheName);
  const hit = await cache.match(request);
  const network = fetch(request)
    .then(async (response) => {
      if (response.ok) await cache.put(request, response.clone());
      return response;
    })
    .catch(() => null);

  return hit || (await network) || Response.error();
}
