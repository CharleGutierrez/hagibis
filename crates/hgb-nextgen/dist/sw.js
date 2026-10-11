// 🪽 Hagibis Sovereign PWA Studio Service Worker (Optimized Cache-First & Offline Monaco)
const CACHE_NAME = 'hgb-pwa-v3';
const MAX_CACHE_ENTRIES = 120;
const STATIC_ASSETS = [
    '/',
    '/manifest.json',
    '/manifest.webmanifest',
    '/icon.svg',
    'https://cdnjs.cloudflare.com/ajax/libs/font-awesome/6.4.0/css/all.min.css',
    'https://cdn.jsdelivr.net/npm/xterm@5.3.0/css/xterm.min.css',
    'https://cdn.jsdelivr.net/npm/xterm@5.3.0/lib/xterm.min.js',
    'https://cdn.jsdelivr.net/npm/xterm-addon-fit@0.8.0/lib/xterm-addon-fit.min.js',
    'https://cdnjs.cloudflare.com/ajax/libs/monaco-editor/0.45.0/min/vs/loader.min.js',
    'https://cdnjs.cloudflare.com/ajax/libs/monaco-editor/0.45.0/min/vs/editor/editor.main.js',
    'https://cdnjs.cloudflare.com/ajax/libs/monaco-editor/0.45.0/min/vs/editor/editor.main.css',
    'https://cdnjs.cloudflare.com/ajax/libs/monaco-editor/0.45.0/min/vs/editor/editor.main.nls.js'
];

async function trimCache(cacheName, maxEntries) {
    try {
        const cache = await caches.open(cacheName);
        const keys = await cache.keys();
        if (keys.length > maxEntries) {
            await cache.delete(keys[0]);
            await trimCache(cacheName, maxEntries);
        }
    } catch (_) {}
}

self.addEventListener('install', (event) => {
    event.waitUntil(
        caches.open(CACHE_NAME).then((cache) => {
            return cache.addAll(STATIC_ASSETS).catch((err) => {
                console.warn('PWA precache partial notice:', err);
            });
        }).then(() => self.skipWaiting())
    );
});

self.addEventListener('activate', (event) => {
    event.waitUntil(
        caches.keys().then((keys) => {
            return Promise.all(
                keys.map((key) => {
                    if (key !== CACHE_NAME) {
                        return caches.delete(key);
                    }
                })
            );
        }).then(() => self.clients.claim())
    );
});

self.addEventListener('fetch', (event) => {
    const url = new URL(event.request.url);

    // Bypass caching for dynamic Axum API routes, WebSockets, or mutations
    if (url.pathname.startsWith('/api/') || event.request.method !== 'GET') {
        return;
    }

    const isCdnAsset = url.hostname.includes('cdnjs.cloudflare.com') || url.hostname.includes('cdn.jsdelivr.net');

    // Strategy A: Cache-First for versioned static CDN assets (0ms local load)
    if (isCdnAsset) {
        event.respondWith(
            caches.match(event.request).then((cachedResponse) => {
                if (cachedResponse) {
                    // Stale-while-revalidate in background
                    fetch(event.request).then((networkResponse) => {
                        if (networkResponse && networkResponse.status === 200) {
                            caches.open(CACHE_NAME).then((cache) => {
                                cache.put(event.request, networkResponse.clone());
                            });
                        }
                    }).catch(() => {});
                    return cachedResponse;
                }
                // Not in cache yet: fetch and cache
                return fetch(event.request).then((networkResponse) => {
                    if (networkResponse && networkResponse.status === 200) {
                        const responseClone = networkResponse.clone();
                        caches.open(CACHE_NAME).then((cache) => {
                            cache.put(event.request, responseClone);
                            trimCache(CACHE_NAME, MAX_CACHE_ENTRIES);
                        });
                    }
                    return networkResponse;
                });
            })
        );
        return;
    }

    // Strategy B: Network-first with instant cache fallback for HTML shell and local routes
    event.respondWith(
        fetch(event.request)
            .then((networkResponse) => {
                if (networkResponse && networkResponse.status === 200) {
                    const responseClone = networkResponse.clone();
                    caches.open(CACHE_NAME).then((cache) => {
                        cache.put(event.request, responseClone);
                        trimCache(CACHE_NAME, MAX_CACHE_ENTRIES);
                    });
                }
                return networkResponse;
            })
            .catch(() => {
                return caches.match(event.request).then((cachedResponse) => {
                    if (cachedResponse) {
                        return cachedResponse;
                    }
                    if (event.request.mode === 'navigate') {
                        return caches.match('/');
                    }
                });
            })
    );
});
