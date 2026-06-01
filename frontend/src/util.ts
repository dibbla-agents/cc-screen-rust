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
