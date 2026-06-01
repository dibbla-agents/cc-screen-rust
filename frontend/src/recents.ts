// "Recents" are the last prompts you sent, kept per-device in localStorage (the
// compose sheet writes one on every send). They're the raw history you promote
// favourites from — favourites themselves live server-side (see api.ts). Shared
// here so both the compose sheet and the favourites sheet read the same list.
const RECENTS_KEY = "ccweb.recents";
const MAX_RECENTS = 100;

export function loadRecents(): string[] {
  try {
    const v = JSON.parse(localStorage.getItem(RECENTS_KEY) || "[]");
    return Array.isArray(v) ? v : [];
  } catch {
    return [];
  }
}

// rememberRecent prepends a sent prompt (de-duped, capped) and returns the new
// list so callers can update their state in one step.
export function rememberRecent(text: string): string[] {
  const t = text.trim();
  if (!t) return loadRecents();
  const next = [t, ...loadRecents().filter((r) => r !== t)].slice(0, MAX_RECENTS);
  localStorage.setItem(RECENTS_KEY, JSON.stringify(next));
  return next;
}
