// Proposal 0017 — the shared client-side "ready" detection model.
//
// A session becomes *notifiable* on the same gated busy→waiting edge the server
// gates the OS Web Push on (0002, `engine.rs::notification_eligible`), but here
// it is recomputed on the client from two consecutive poll snapshots. This is
// the **foreground** complement: while the PWA is open and focused, OS Web Push
// is suppressed, so a non-mounted session going ready would otherwise be
// invisible. Proposal 0018 (the TUI) cites this same model rather than redefine
// it.
//
// Keep this module pure and side-effect-free so it is unit-testable from two
// snapshots (see readyEdges.test.ts). The React glue (prev-snapshot ref,
// visibility gate, toast plumbing) lives in App.tsx.

import type { Session } from "./api";

// Defaults match the server gate (engine.rs NOTIFY_MIN_WORK_SECS /
// NOTIFY_INPUT_QUIET_SECS) so the foreground toast and the background push fire
// on exactly the same edge.
export const NOTIFY_MIN_WORK_SECS = 60;
export const NOTIFY_INPUT_QUIET_SECS = 60;

// The identity used everywhere a session is keyed across machines: a same-named
// session on a different agent is a different session. Mirrors App.tsx's
// `refKey` / PaneRef identity so the mounted-exclusion set lines up.
export function sessionKey(s: { name: string; machine?: string }): string {
  return `${s.machine ?? ""}/${s.name}`;
}

// One ready edge worth a toast — just enough for the toast row (tool-color dot +
// short name) and to route the click through openSessionByName.
export interface ReadyEdge {
  name: string;
  machine: string;
  tool: string;
  short: string;
  // Operator-chosen display label (proposal 0035), when set — the toast names the
  // session by `displayName` (label || short) so a renamed session reads right.
  label?: string;
  // The session's LLM summary (proposal 0022), when available — the toast shows
  // `detail` (or `headline`) instead of the generic "ready for input".
  headline?: string;
  detail?: string;
}

// detectReadyEdges diffs the previous snapshot against the current one and
// returns the sessions that crossed the gated busy→waiting edge per §2 of the
// proposal. `mounted` is the set of sessionKey()s currently on screen in a pane
// (never toasted); `nowMs` is Date.now() (ms) — passed in so the function stays
// pure and testable.
//
// Suppression rules (mirroring the server's "first sight records state only"):
//   - a session absent from `prev` is first-seen → baseline only, no edge;
//   - a session in `mounted` is on screen → never an edge;
//   - the foreground-only rule (document.visibilityState) and the
//     first-snapshot-after-load baseline are enforced by the caller, which
//     simply doesn't call this on a hidden tab / with no prior snapshot.
export function detectReadyEdges(
  prev: Session[],
  cur: Session[],
  mounted: Set<string>,
  nowMs: number
): ReadyEdge[] {
  const now = Math.floor(nowMs / 1000);
  const prevByKey = new Map(prev.map((s) => [sessionKey(s), s]));
  // Treat negative ages as 0 — same discipline as the server's saturating_sub,
  // so clock skew can't spuriously satisfy a gate.
  const ageSince = (t: number) => Math.max(0, now - t);

  const edges: ReadyEdge[] = [];
  for (const c of cur) {
    const key = sessionKey(c);
    if (mounted.has(key)) continue; // on screen — carries its own status
    const p = prevByKey.get(key);
    if (!p) continue; // first sight — establish baseline, don't toast

    // busy → waiting edge.
    if (p.waiting || !c.waiting) continue;

    const busySince = c.busy_since ?? 0;
    const lastInput = c.last_input_at ?? 0;
    if (busySince === 0) continue; // never recorded a work start (server gate)
    if (ageSince(busySince) < NOTIFY_MIN_WORK_SECS) continue; // gate 1: worked > 1 min
    if (ageSince(lastInput) < NOTIFY_INPUT_QUIET_SECS) continue; // gate 2: user idle > 1 min

    edges.push({
      name: c.name,
      machine: c.machine ?? "",
      tool: c.tool,
      short: c.short,
      label: c.label,
      headline: c.headline,
      detail: c.detail,
    });
  }
  return edges;
}
