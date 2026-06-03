// Web Push enrolment for the PWA: subscribe this device so the server can buzz
// it when an agent finishes its turn (busy→waiting). The server side lives in
// src/push.rs; the service-worker handlers in public/push-sw.js.
//
// iOS only delivers Web Push to a PWA added to the Home Screen (16.4+), and the
// permission prompt must come from a user gesture — so this is driven by the
// bell button (NotificationsButton), never on load.

export function pushSupported(): boolean {
  return (
    typeof navigator !== "undefined" &&
    "serviceWorker" in navigator &&
    typeof window !== "undefined" &&
    "PushManager" in window &&
    "Notification" in window
  );
}

// Is this device currently subscribed (has a live PushSubscription)?
export async function pushEnabled(): Promise<boolean> {
  if (!pushSupported()) return false;
  try {
    const reg = await navigator.serviceWorker.ready;
    return (await reg.pushManager.getSubscription()) != null;
  } catch {
    return false;
  }
}

// VAPID application server keys arrive base64url; PushManager.subscribe wants the
// raw bytes.
function urlBase64ToUint8Array(base64: string): Uint8Array<ArrayBuffer> {
  const padding = "=".repeat((4 - (base64.length % 4)) % 4);
  const b64 = (base64 + padding).replace(/-/g, "+").replace(/_/g, "/");
  const raw = atob(b64);
  // Back it with a concrete ArrayBuffer so the type is Uint8Array<ArrayBuffer>
  // (what BufferSource/applicationServerKey expects), not Uint8Array<ArrayBufferLike>.
  const out = new Uint8Array(new ArrayBuffer(raw.length));
  for (let i = 0; i < raw.length; i++) out[i] = raw.charCodeAt(i);
  return out;
}

// Request permission, subscribe via PushManager with the server's VAPID key, and
// register the subscription server-side. Throws with a UI-showable message on a
// hard failure (permission denied, no support, network).
export async function enablePush(): Promise<void> {
  if (!pushSupported()) throw new Error("notifications aren't supported here");
  const perm = await Notification.requestPermission();
  if (perm !== "granted") throw new Error("notification permission denied");

  const reg = await navigator.serviceWorker.ready;
  let sub = await reg.pushManager.getSubscription();
  if (!sub) {
    const keyRes = await fetch("/api/push/key");
    if (!keyRes.ok) throw new Error(`push key: ${keyRes.status}`);
    const { key } = (await keyRes.json()) as { key: string };
    sub = await reg.pushManager.subscribe({
      userVisibleOnly: true,
      applicationServerKey: urlBase64ToUint8Array(key),
    });
  }

  const json = sub.toJSON();
  const res = await fetch("/api/push/subscribe", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ endpoint: sub.endpoint, keys: json.keys }),
  });
  if (!res.ok && res.status !== 204) throw new Error(`subscribe: ${res.status}`);
}

// Unsubscribe this device (tell the server to forget it, then drop the local
// subscription).
export async function disablePush(): Promise<void> {
  if (!pushSupported()) return;
  const reg = await navigator.serviceWorker.ready;
  const sub = await reg.pushManager.getSubscription();
  if (!sub) return;
  await fetch("/api/push/unsubscribe", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ endpoint: sub.endpoint }),
  }).catch(() => {});
  await sub.unsubscribe().catch(() => {});
}

// Ask the server to fire a buzz now (used to confirm the wiring on enable).
export async function testPush(): Promise<void> {
  const res = await fetch("/api/push/test", { method: "POST" });
  if (!res.ok && res.status !== 204) throw new Error(`test: ${res.status}`);
}
