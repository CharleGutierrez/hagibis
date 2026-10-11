// 🪽 Hagibis Sovereign PWA Studio Service Worker (Optimized Cache-First & Offline Monaco + Wasm Microkernel)
const CACHE_NAME = 'hgb-pwa-v4';
const MAX_CACHE_ENTRIES = 140;

const STATIC_ASSETS = [
    '/',
    '/manifest.json',
    '/manifest.webmanifest',
    '/icon.svg',
    // --- Hybrid WebAssembly Microkernel Asset ---
    '/hgb_kernel.wasm',
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
    const isWasmKernel = url.pathname === '/hgb_kernel.wasm';

    // Strategy A: Cache-First for Wasm Kernel and CDN static assets (0ms local load & 100% offline)
    if (isCdnAsset || isWasmKernel) {
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
                    });
                }
                return networkResponse;
            })
            .catch(() => {
                return caches.match(event.request).then((cachedResponse) => {
                    if (cachedResponse) {
                        return cachedResponse;
                    }
                    if (event.request.headers.get('accept')?.includes('text/html')) {
                        return caches.match('/');
                    }
                });
            })
    );
});
