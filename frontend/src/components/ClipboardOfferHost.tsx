import { useEffect, useRef, useState } from "react";
import {
  limiterFor,
  OFFER_TTL_MS,
  subscribeClipboardDisabled,
  subscribeClipboardOffers,
  subscribeClipboardWrote,
  type ClipboardDisabled,
  type ClipboardOffer,
} from "../osc52Bus";
import { previewClipboardText } from "../osc52";
import { writeClipboard } from "../util";

// ClipboardOfferHost — the recovery toast for a clipboard write cc-screen
// refused to perform silently (cc-screen-saas proposal 0077 A6), plus the
// sticky banner a flooding session earns (A7).
//
// This is net-new UI rather than a reuse of App's `showToast`, for two reasons:
// that toast is a single replaceable slot rendered `pointer-events-none`, so it
// can neither host a button nor survive a second message — and both properties
// are load-bearing here.
//
// The whole design turns on one rule: **the click must not launder the
// gesture.** That a network OSC 52 is not a user gesture is the browser's
// mitigation, not a quirk to route around, so completing an attacker-influenced
// write inside a user's click is only acceptable if the click is informed:
//
//   - FROZEN. The staged text is immutable once shown; a later sequence creates
//     a new offer and can never mutate a pending one (otherwise the design is a
//     swap race — show benign, click writes malicious).
//   - INFORMED. The button names the source and the byte count, and the preview
//     renders control and bidi characters as visible escapes, as text.
//   - EXPIRING. A stage older than OFFER_TTL_MS is not completable.
//   - No auto-focus, and the card sits clear of where a click aimed elsewhere
//     lands.

function bytesLabel(n: number): string {
  if (n < 1024) return `${n} bytes`;
  if (n < 1024 * 1024) return `${Math.round(n / 1024)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

export default function ClipboardOfferHost() {
  const [offer, setOffer] = useState<ClipboardOffer | null>(null);
  const [disabled, setDisabled] = useState<ClipboardDisabled | null>(null);
  const [wrote, setWrote] = useState<string | null>(null);
  const [done, setDone] = useState(false);
  const expiry = useRef<number | null>(null);
  const wroteTimer = useRef<number | null>(null);

  useEffect(() => {
    const offWrote = subscribeClipboardWrote((w) => {
      setWrote(w.label);
      if (wroteTimer.current != null) window.clearTimeout(wroteTimer.current);
      wroteTimer.current = window.setTimeout(() => {
        wroteTimer.current = null;
        setWrote(null);
      }, 2500);
    });
    const off = subscribeClipboardOffers((o) => {
      setOffer(o);
      setDone(false);
      if (expiry.current != null) window.clearTimeout(expiry.current);
      expiry.current = window.setTimeout(() => {
        expiry.current = null;
        setOffer(null);
      }, OFFER_TTL_MS);
    });
    const offDis = subscribeClipboardDisabled((d) => setDisabled(d));
    return () => {
      off();
      offDis();
      offWrote();
      if (expiry.current != null) window.clearTimeout(expiry.current);
      if (wroteTimer.current != null) window.clearTimeout(wroteTimer.current);
    };
  }, []);

  const dismiss = () => {
    if (expiry.current != null) {
      window.clearTimeout(expiry.current);
      expiry.current = null;
    }
    setOffer(null);
  };

  const accept = (o: ClipboardOffer) => {
    // Expiry is re-checked at click time, not only by the timer: a tab that was
    // backgrounded can fire timers late.
    if (Date.now() - o.at > OFFER_TTL_MS) {
      dismiss();
      return;
    }
    writeClipboard(o.text)
      .then(() => {
        setDone(true);
        window.setTimeout(dismiss, 1200);
      })
      .catch(() => dismiss());
  };

  if (!offer && !disabled && !wrote) return null;

  return (
    <div className="pointer-events-none absolute inset-x-0 bottom-28 z-50 flex flex-col items-center gap-2 px-4">
      {wrote && (
        <div
          role="status"
          aria-live="polite"
          className="rounded-full bg-panel/95 px-4 py-2 text-xs text-slate-300 shadow-lg backdrop-blur-sm"
        >
          📋 Copied from {wrote}
        </div>
      )}
      {offer && (
        <div
          role="status"
          aria-live="polite"
          className="pointer-events-auto w-full max-w-sm rounded-2xl border border-edge bg-panel/95 p-3 text-sm shadow-lg backdrop-blur-sm"
        >
          <div className="mb-1 flex items-baseline justify-between gap-2">
            <span className="truncate text-xs font-medium text-slate-300">
              📋 {offer.label}
            </span>
            <span className="shrink-0 text-[11px] text-slate-500">
              {bytesLabel(offer.bytes)}
            </span>
          </div>
          {offer.announceOnly ? (
            <p className="text-xs leading-snug text-slate-400">
              This session tried to copy {bytesLabel(offer.bytes)} — too large to put on
              your clipboard, so nothing was copied.
            </p>
          ) : (
            <>
              <pre className="mb-2 max-h-16 overflow-hidden whitespace-pre-wrap break-all rounded-lg bg-bar px-2 py-1 font-mono text-[11px] leading-snug text-slate-400">
                {previewClipboardText(offer.text)}
              </pre>
              {offer.altered && (
                <p className="mb-2 text-[11px] leading-snug text-amber">
                  Control or direction-override characters were removed.
                </p>
              )}
            </>
          )}
          <div className="flex items-center justify-end gap-2">
            <button
              type="button"
              onClick={dismiss}
              className="min-h-[40px] rounded-full px-3 text-xs text-slate-400 hover:text-slate-200"
            >
              Dismiss
            </button>
            {!offer.announceOnly && (
              <button
                type="button"
                onClick={() => accept(offer)}
                className="min-h-[40px] rounded-full bg-accent px-4 text-xs font-medium text-bar"
              >
                {done
                  ? "Copied"
                  : `Copy ${bytesLabel(offer.bytes)} from ${offer.label}`}
              </button>
            )}
          </div>
        </div>
      )}

      {disabled && (
        <div
          role="status"
          className="pointer-events-auto flex w-full max-w-sm items-center gap-2 rounded-2xl border border-edge bg-panel/95 px-3 py-2 text-xs text-slate-300 shadow-lg backdrop-blur-sm"
        >
          <span className="min-w-0 flex-1">
            Clipboard delivery paused for <b className="font-medium">{disabled.label}</b> —
            it copied too many times in a row.
          </span>
          <button
            type="button"
            onClick={() => {
              limiterFor(disabled.key).reset();
              setDisabled(null);
            }}
            className="min-h-[36px] shrink-0 rounded-full border border-edge px-3 text-xs text-slate-200"
          >
            Re-enable
          </button>
        </div>
      )}
    </div>
  );
}
