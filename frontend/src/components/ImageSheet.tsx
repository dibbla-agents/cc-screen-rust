import { useRef, useState } from "react";
import { toPng } from "../util";

interface Props {
  open: boolean;
  onClose: () => void;
  onSend: (png: Blob) => void;
}

// The async Clipboard API (navigator.clipboard.read) is gated to *secure
// contexts* — HTTPS, plus localhost/127.0.0.1. Plain HTTP off-loopback
// (the tailnet deployment) doesn't qualify, so the "Paste from clipboard"
// button can't work there. We detect this up front so we can swap the
// button for a hint pointing at the global paste-event workaround (which
// uses the ClipboardEvent path that *is* available on HTTP).
const clipboardReadAvailable =
  typeof window !== "undefined" &&
  window.isSecureContext &&
  typeof navigator !== "undefined" &&
  typeof navigator.clipboard?.read === "function";

// The OS paste shortcut differs by platform — and the workaround only works
// via the OS's actual paste shortcut (the one that fires a `paste` event).
const isMac =
  typeof navigator !== "undefined" && /Mac|iPad|iPhone|iPod/i.test(navigator.userAgent);
const pasteHint = isMac ? "⌘V" : "⌃V";

// Pick a screenshot (Photos / Files / Camera) or read it from the phone
// clipboard, preview it, then inject it into Claude Code as a paste.
export default function ImageSheet({ open, onClose, onSend }: Props) {
  const fileRef = useRef<HTMLInputElement>(null);
  const [png, setPng] = useState<Blob | null>(null);
  const [url, setUrl] = useState<string>("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  if (!open) return null;

  const accept = async (blob: Blob | null | undefined) => {
    setErr(null);
    if (!blob) return;
    setBusy(true);
    try {
      const p = await toPng(blob);
      setPng(p);
      setUrl((old) => {
        if (old) URL.revokeObjectURL(old);
        return URL.createObjectURL(p);
      });
    } catch {
      setErr("Couldn't read that image.");
    } finally {
      setBusy(false);
    }
  };

  const fromClipboard = async () => {
    setErr(null);
    try {
      const items = await navigator.clipboard.read();
      for (const it of items) {
        const type = it.types.find((t) => t.startsWith("image/"));
        if (type) {
          await accept(await it.getType(type));
          return;
        }
      }
      setErr("No image in the clipboard.");
    } catch {
      setErr("Clipboard blocked — use “Photo / screenshot” instead.");
    }
  };

  const reset = () => {
    if (url) URL.revokeObjectURL(url);
    setPng(null);
    setUrl("");
  };
  const close = () => {
    reset();
    setErr(null);
    onClose();
  };
  const send = () => {
    if (png) {
      onSend(png);
      close();
    }
  };

  return (
    <div className="absolute inset-0 z-40 flex flex-col justify-end">
      <div className="flex-1 bg-black/50" onClick={close} />
      <div className="rounded-t-2xl border-t border-edge bg-panel p-3 pb-safe">
        <div className="mx-auto mb-3 h-1 w-10 rounded-full bg-edge" />
        {err && <div className="mb-2 text-sm text-red-400">{err}</div>}

        {url ? (
          <img
            src={url}
            alt="preview"
            className="mb-3 max-h-60 w-full rounded-lg bg-bar object-contain"
          />
        ) : (
          <div className="mb-3 grid grid-cols-2 gap-2">
            <button
              onClick={() => fileRef.current?.click()}
              className="rounded-lg bg-bar px-4 py-6 text-sm text-slate-200 active:bg-edge"
            >
              📷 Photo / screenshot
            </button>
            {clipboardReadAvailable ? (
              <button
                onClick={fromClipboard}
                className="rounded-lg bg-bar px-4 py-6 text-sm text-slate-200 active:bg-edge"
              >
                📋 Paste from clipboard
              </button>
            ) : (
              // Plain-HTTP fallback: the async Clipboard API isn't available,
              // but the global Ctrl+V paste-event handler in App.tsx is.
              <div
                className="flex flex-col items-center justify-center rounded-lg border border-dashed border-edge bg-bar/50 px-3 py-4 text-center text-xs text-slate-400"
                title="The async Clipboard API needs HTTPS; the paste event still works on HTTP"
              >
                <span className="text-base">📋</span>
                <span className="mt-1 font-medium text-slate-300">Press {pasteHint} anywhere</span>
                <span className="mt-0.5 text-[11px] text-slate-500">
                  (clipboard API needs HTTPS)
                </span>
              </div>
            )}
          </div>
        )}

        <input
          ref={fileRef}
          type="file"
          accept="image/*"
          className="hidden"
          onChange={(e) => accept(e.target.files?.[0])}
        />

        <div className="flex gap-2">
          <button
            onClick={close}
            className="rounded-lg bg-bar px-4 py-3 text-sm text-slate-400 active:bg-edge"
          >
            Cancel
          </button>
          {url && (
            <button
              onClick={reset}
              className="rounded-lg bg-bar px-4 py-3 text-sm text-slate-300 active:bg-edge"
            >
              Change
            </button>
          )}
          <button
            onClick={send}
            disabled={!png || busy}
            className="flex-1 rounded-lg bg-accent px-4 py-3 text-sm font-semibold text-bar active:opacity-80 disabled:opacity-40"
          >
            {busy ? "…" : "Paste into terminal"}
          </button>
        </div>
      </div>
    </div>
  );
}
