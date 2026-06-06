// Web Push handlers, importScripts'd into the Workbox-generated service worker
// (see vite.config.ts → workbox.importScripts). Plain JS so it ships as a static
// asset with no build step. The payload shape is set by the server (src/push.rs):
// { title, body, session, tag }.

self.addEventListener("push", (event) => {
  let data = {};
  try {
    data = event.data ? event.data.json() : {};
  } catch (e) {
    data = { body: event.data && event.data.text() };
  }
  const title = data.title || "cc-screen";
  event.waitUntil(
    self.registration.showNotification(title, {
      body: data.body || "",
      // Same tag per session → a new "finished" buzz replaces the old one
      // instead of stacking.
      tag: data.tag || data.session || "cc-screen",
      renotify: true,
      data: { session: data.session || "" },
      icon: "/icon-192.png",
      badge: "/favicon.png",
    })
  );
});

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  const session = (event.notification.data && event.notification.data.session) || "";
  const url = new URL(session ? "/?session=" + encodeURIComponent(session) : "/", self.location.origin).href;
  event.waitUntil(
    (async () => {
      // Focus an existing window if the PWA is already open and ask it to switch
      // sessions; otherwise open a deep-linked window.
      const wins = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
      for (const w of wins) {
        if (session && "postMessage" in w) {
          try {
            w.postMessage({ type: "open-session", session });
          } catch (e) {
            /* ignore */
          }
        } else if (session && "navigate" in w) {
          try {
            const next = await w.navigate(url);
            if (next && "focus" in next) await next.focus();
            return;
          } catch (e) {
            /* ignore */
          }
        }
        if ("focus" in w) {
          try {
            await w.focus();
          } catch (e) {
            /* ignore */
          }
          return;
        }
      }
      if (self.clients.openWindow) await self.clients.openWindow(url);
    })()
  );
});
