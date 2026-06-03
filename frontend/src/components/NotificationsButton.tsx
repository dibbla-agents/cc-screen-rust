import { useEffect, useState } from "react";
import { BellIcon } from "../icons";
import { disablePush, enablePush, pushEnabled, pushSupported, testPush } from "../push";

// Bell toggle for Web Push ("buzz my phone when an agent finishes"). Self-
// contained: manages its own subscribed/busy/error state and talks to push.ts
// directly, so it drops into any toolbar with just a className for chrome.
// Enabling also fires a test buzz so the user immediately confirms it works.
// Renders nothing where push isn't supported (e.g. an un-installed iOS PWA).
export default function NotificationsButton({ className }: { className?: string }) {
  const [supported] = useState(() => pushSupported());
  const [on, setOn] = useState(false);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    if (!supported) return;
    pushEnabled().then(setOn).catch(() => {});
  }, [supported]);

  if (!supported) return null;

  const toggle = async () => {
    if (busy) return;
    setBusy(true);
    setErr(null);
    try {
      if (on) {
        await disablePush();
        setOn(false);
      } else {
        await enablePush();
        setOn(true);
        await testPush(); // immediate confirmation buzz
      }
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
      setTimeout(() => setErr(null), 4000);
    } finally {
      setBusy(false);
    }
  };

  const title = err
    ? `Notifications: ${err}`
    : on
    ? "Notifications on — tap to turn off"
    : "Notify me when an agent finishes (sends a test buzz)";

  return (
    <button onClick={toggle} disabled={busy} aria-label="Toggle notifications" title={title} className={className}>
      <BellIcon
        className={`h-4 w-4 ${err ? "text-red-400" : on ? "text-accent" : ""} ${busy ? "animate-pulse" : ""}`}
        filled={on && !err}
      />
    </button>
  );
}
