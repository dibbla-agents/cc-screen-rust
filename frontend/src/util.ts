import type { Session } from "./api";

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

// machineAccent — a stable accent for a machine id, derived from a hash so the
// colour is deterministic across reloads and clients with no server state.
// Same machine → same hue everywhere; panes from the same box read as a group.
// Empty machine (single-agent / no hub) → null: there is nothing to
// disambiguate, so callers leave the bar neutral (no spine, no hostname).
//   spine — the short vertical machine "spine" bar
//   text  — the hostname tint (lighter, for ≥4.5:1 contrast on the dark bar)
//   tint  — an optional faint background wash for the active pane
export function machineAccent(
  machine: string
): { spine: string; text: string; tint: string } | null {
  if (!machine) return null;
  let h = 0;
  for (let i = 0; i < machine.length; i++) h = (h * 31 + machine.charCodeAt(i)) >>> 0;
  const hue = h % 360;
  // Fixed S/L so every machine reads at the same intensity on the dark bar.
  return {
    spine: `hsl(${hue} 62% 55%)`,
    text: `hsl(${hue} 70% 74%)`,
    tint: `hsl(${hue} 55% 50% / 0.12)`,
  };
}

// sessionAccent — the curated per-session mark palette (proposal 0029). Tokens
// are stable ids stored on the session; the rendered HSL is owned here so the
// shade stays tuned to the dark `bar` and clear of the reserved status hues
// (cyan accent ~199° / amber ~43° / green / red). Fixed S/L → every mark reads at
// one calm intensity. `null`/absent/unknown-token = no mark. Returns the border,
// the switcher-swatch, and a faint wash for emphasis.
//
// IMPORTANT: keep the token set in lockstep with the agent's
// `SESSION_COLOR_TOKENS` (crates/protocol/src/lib.rs) — the agent validates a
// SetColor against it. An unknown token here renders unmarked (forward-compat).
const SESSION_COLORS: Record<string, number> = {
  rose: 350,
  magenta: 320,
  violet: 270,
  indigo: 230,
  teal: 175,
  lime: 95,
  orange: 25,
  slate: 210,
};
export const SESSION_COLOR_TOKENS = Object.keys(SESSION_COLORS);

export function sessionAccent(
  color?: string
): { border: string; swatch: string; wash: string; bar: string } | null {
  if (!color) return null;
  const hue = SESSION_COLORS[color];
  if (hue === undefined) return null; // unknown token (forward-compat) → unmarked
  return {
    border: `hsl(${hue} 60% 58%)`, // the pane border / agent-view spine
    swatch: `hsl(${hue} 62% 60%)`, // the switcher-row swatch dot
    wash: `hsl(${hue} 55% 50% / 0.10)`, // optional faint wash
    // The desktop pane identity-bar fill — a clearly-coloured but muted-dark
    // panel the light bar text (slate-100/200) still reads on.
    bar: `hsl(${hue} 45% 26%)`,
  };
}

// nextSessionColor — pick a *different* random token than `current` (so a re-roll
// always visibly changes). Wired to the mark button + the ⌃B c chord.
export function nextSessionColor(current?: string): string {
  const pool = SESSION_COLOR_TOKENS.filter((t) => t !== current);
  return pool[Math.floor(Math.random() * pool.length)]!;
}

// Max display-label length (proposal 0035). Keep in lockstep with the agent's
// `MAX_SESSION_LABEL_LEN` (crates/protocol/src/lib.rs): the agent rejects a
// longer label with a 400, so the input caps here to keep the UI in agreement.
export const MAX_SESSION_LABEL_LEN = 60;

// displayName — the session's display name (proposal 0035): the operator-chosen
// label if set, else the slug `short`. The single rule applied at every
// name-render site (switcher rows, pane identity bar, status view, tooltips,
// upload target, search), so a label shows consistently everywhere while the
// identity `short` stays the routing/persistence key underneath.
export function displayName(s: Pick<Session, "label" | "short">): string {
  return s.label?.trim() || s.short;
}

// dirCrumb — the last two segments of an absolute path: the leaf directory and
// its parent (proposal 0025). Drives the two-segment folder breadcrumb label, a
// far better disambiguator than the bare leaf for sessions auto-named after the
// dir basename (`…/projectA/frontend` vs `…/projectB/frontend`).
//   "/home/erik"                            → { parent: "home", leaf: "erik" }
//   "/home/erik/development/cc-screen-rust" → { parent: "development", leaf: "cc-screen-rust" }
//   "/home"                                 → { parent: "",   leaf: "home" }
//   "" / "/"                                → null  (nothing usable → caller falls back to `short`)
export function dirCrumb(cwd?: string): { parent: string; leaf: string } | null {
  if (!cwd) return null;
  const segs = cwd.split("/").filter(Boolean);
  if (segs.length === 0) return null;
  return {
    leaf: segs[segs.length - 1]!,
    parent: segs.length >= 2 ? segs[segs.length - 2]! : "",
  };
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

// One dot, three meanings — shared by the header dot and every switcher row so
// the signal reads the same everywhere:
//   • error   (red)    — the connection is believed broken (WS dropped). Only a
//                        *closed* socket counts; a still-`connecting` one is just
//                        establishing and must not flash red on every attach.
//   • running (amber)  — in an open, submit-armed busy window (`waiting === false`).
//   • ready   (green)  — not in a busy window; your turn
//                        (`waiting === true`; see the server's WORK_GRACE_SECS).
// `conn` is the per-session WebSocket state when we have one (a session open in a
// pane); omit it for rows we aren't attached to — those can't have a connection
// problem, so they fall straight through to running/ready.
export type AgentStatus = "error" | "running" | "ready";

export function agentStatus(
  waiting: boolean,
  conn?: "connecting" | "open" | "closed"
): AgentStatus {
  if (conn === "closed") return "error";
  return waiting ? "ready" : "running";
}

// Tailwind classes for a status dot. Running pulses to read as "live"; ready and
// error are solid. NB: use `bg-amber` (the custom #f5b942), not `bg-amber-400` —
// the config overrides the default amber scale, so `amber-<shade>` is a dead class.
export function statusDot(status: AgentStatus): string {
  switch (status) {
    case "error":
      return "bg-red-500";
    case "running":
      return "bg-amber animate-pulse";
    case "ready":
      return "bg-emerald-400";
  }
}

export function statusTitle(status: AgentStatus): string {
  return status === "error"
    ? "connection problem"
    : status === "running"
    ? "working"
    : "ready for input";
}

// fuzzyScore — case-insensitive subsequence match of `query` against `text`,
// returning a higher number for a better match (contiguous runs, word-start
// hits, and a head-of-string match all add) or `null` when `query` is not a
// subsequence at all. Mirrors the agent-side scorer in `dirsearch.rs` so the
// in-sidebar list filter (proposal 0016, Part A) ranks like the folder search.
// An empty query scores 0 (everything matches), so the caller keeps today's
// resting order when nothing is typed.
export function fuzzyScore(query: string, text: string): number | null {
  const q = query.toLowerCase();
  if (q.length === 0) return 0;
  const h = text.toLowerCase();
  let hi = 0;
  let score = 0;
  let last = -2;
  let run = 0;
  for (const nc of q) {
    let pos = -1;
    for (let j = hi; j < h.length; j++) {
      if (h[j] === nc) {
        pos = j;
        break;
      }
    }
    if (pos < 0) return null;
    score += 2;
    if (pos === 0) score += 6;
    if (pos === 0 || "/-_. ".includes(h[pos - 1]!)) score += 12;
    if (pos === last + 1) {
      run += 1;
      score += 4 * run;
    } else {
      run = 0;
    }
    last = pos;
    hi = pos + 1;
  }
  return score;
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

// The unix-seconds anchor of a session's *current* state — the moment it last
// transitioned. Elapsed-in-state = now - stateAnchor(s); it climbs from ~0 at
// the transition. Used by the status view (0023) and the switcher for both the
// timer and the sort key (sorting on the anchor, not the live elapsed, keeps row
// order stable while the number ticks).
//   ready   (waiting=true):  since it went busy→ready  → busy_until, else activity
//   working (waiting=false): since this turn began     → busy_since, else activity
// The ready anchor is busy_until (the busy→ready instant), NOT activity: under
// input-gated busy (0024) a cosmetic focus/resize repaint still bumps `activity`
// but never moves busy_until, so the "ready for N" counter no longer resets — and
// a focused session no longer jumps to the top of the attention-ordered lists.
export function stateAnchor(s: Session): number {
  if (s.waiting) return s.busy_until && s.busy_until > 0 ? s.busy_until : s.activity;
  return s.busy_since && s.busy_since > 0 ? s.busy_since : s.activity;
}
