export function toolColor(tool: string): string {
  switch (tool) {
    case "claude":
      return "bg-claude";
    case "kimi":
      return "bg-kimi";
    case "gemini":
      return "bg-gemini";
    case "codex":
      return "bg-codex";
    case "shell":
      return "bg-shell";
    default:
      return "bg-edge";
  }
}

// toPng normalises any browser-decodable image (phone screenshots are PNG;
// photos may be HEIC/JPEG; pasted clipboard items can be anything Chrome
// decodes) to the PNG that Claude Code's clipboard read expects, by drawing
// it to a canvas and re-encoding. Shared by the image sheet and the global
// paste-event handler in App.tsx.
export async function toPng(src: Blob): Promise<Blob> {
  const bitmap = await createImageBitmap(src);
  const canvas = document.createElement("canvas");
  canvas.width = bitmap.width;
  canvas.height = bitmap.height;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("no canvas context");
  ctx.drawImage(bitmap, 0, 0);
  bitmap.close?.();
  return await new Promise<Blob>((res, rej) =>
    canvas.toBlob((b) => (b ? res(b) : rej(new Error("encode failed"))), "image/png")
  );
}

// writeClipboard puts `text` on the system clipboard. On HTTPS / localhost
// we prefer the modern async API; on the plain-HTTP tailnet deployment that
// API is gated, so we fall back to the deprecated-but-still-supported
// execCommand('copy') path. **Must be called inside a user-gesture handler**
// (keydown / click) — both paths require it, and the fallback also briefly
// steals focus to a hidden textarea, so we restore the previously-focused
// element on the way out (otherwise the user's next keystroke would land
// in the dead textarea instead of xterm).
export async function writeClipboard(text: string): Promise<void> {
  if (navigator.clipboard && window.isSecureContext) {
    try {
      await navigator.clipboard.writeText(text);
      return;
    } catch {
      /* fall through to execCommand */
    }
  }
  const prevFocus = document.activeElement as HTMLElement | null;
  const ta = document.createElement("textarea");
  ta.value = text;
  ta.setAttribute("readonly", "");
  ta.style.position = "fixed";
  ta.style.left = "-9999px";
  ta.style.top = "0";
  document.body.appendChild(ta);
  try {
    ta.focus();
    ta.select();
    ta.setSelectionRange(0, ta.value.length);
    const ok = document.execCommand("copy");
    if (!ok) throw new Error("execCommand copy returned false");
  } finally {
    document.body.removeChild(ta);
    prevFocus?.focus?.();
  }
}

// Compact "time since last activity" for the switcher rows.
export function ago(unixSeconds: number): string {
  const s = Math.max(0, Math.floor(Date.now() / 1000 - unixSeconds));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h`;
  return `${Math.floor(h / 24)}d`;
}
