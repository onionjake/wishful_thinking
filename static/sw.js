// Minimal service worker so browsers offer "Install" / "Add to Home Screen".
// It deliberately caches nothing: every request goes straight to the network.
self.addEventListener("install", function () { self.skipWaiting(); });
self.addEventListener("activate", function (e) { e.waitUntil(self.clients.claim()); });
