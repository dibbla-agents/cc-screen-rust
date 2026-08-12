// Pane-state model + persistence, split out from App.tsx so the migration logic
// is pure and unit-testable (no React, no component import graph).
//
// A pane holds a PaneRef ({name, machine}) rather than a bare session name: the
// owning machine is captured when the session is selected/created and travels
// with the pane, so a hub fronting several agents routes every request to the
// right machine and two machines with a same-named session never collide. See
// PWA machine-threading plan.

import type { Layout } from "./components/TileGrid";
import { paneCount } from "./components/TileGrid";
import type { PaneRef } from "./api";

export const LAST_KEY = "ccweb.lastSession"; // legacy single-session key (migrate from)
export const PANES_KEY = "ccweb.panes.v2"; // {layout, panes:[{name,machine}|null], active}
const PANES_KEY_V1 = "ccweb.panes.v1"; // pre-hub: panes were bare name strings

export interface PaneState {
  layout: Layout;
  panes: (PaneRef | null)[]; // length == paneCount(layout)
  active: number; // index into panes
  // The pane focus came from — tmux's `last-pane`, which `⌃B ;` toggles back
  // to. Additive and back-compatible in both directions: an older blob has no
  // `prev` and normalizes to 0, an older build ignores the extra key. See
  // docs/proposals/0081-pane-focus-navigation.md Part D.
  prev: number;
}

// normalizePane upgrades one persisted slot to a PaneRef. Tolerates the v1 shape
// (a bare session-name string → {name, machine:""}) and the v2 shape
// ({name, machine}); null / empty / garbage becomes an empty slot.
export function normalizePane(v: unknown): PaneRef | null {
  if (typeof v === "string") return v ? { name: v, machine: "" } : null;
  if (v && typeof v === "object") {
    const o = v as { name?: unknown; machine?: unknown };
    if (typeof o.name === "string" && o.name) {
      return { name: o.name, machine: typeof o.machine === "string" ? o.machine : "" };
    }
  }
  return null;
}

// normalizePaneState turns parsed storage JSON (v1 or v2 shape) into a valid
// PaneState: layout clamped to 1–6, panes sized/validated via paneCount(), and
// active clamped into range. Pure (no storage access) so it's unit-testable.
export function normalizePaneState(raw: unknown): PaneState {
  const s = (raw ?? {}) as {
    layout?: unknown;
    panes?: unknown;
    active?: unknown;
    prev?: unknown;
  };
  const layout = Math.max(1, Math.min(6, Math.floor(Number(s.layout) || 1))) as Layout;
  const count = paneCount(layout);
  const arr = Array.isArray(s.panes) ? (s.panes as unknown[]) : [];
  const panes = Array.from({ length: count }, (_, i) => normalizePane(arr[i]));
  const clamp = (v: unknown) =>
    Math.max(0, Math.min(count - 1, Math.floor(Number(v) || 0)));
  // `prev` (0081 Part D) clamps exactly like `active`; a blob written before it
  // existed yields 0, which needs no migration and no key bump.
  return { layout, panes, active: clamp(s.active), prev: clamp(s.prev) };
}

// applyLayout is `setLayout`'s reducer, lifted out of App.tsx so the resize
// migration is pure and unit-testable. It grows/shrinks the panes array to
// match paneCount(l). Growing fills with nulls. Shrinking: if the active pane's
// index falls outside the new range, the user's focused session is migrated
// into the last surviving slot (overwriting whatever was there) before
// truncation — so switching to single-pane while focused on pane 3 of a quad
// doesn't silently drop the session the user was looking at. The sessions in
// the other dropped slots are still alive on the agent; the drawer is the
// recovery path. `active` AND `prev` are then clamped into the new range —
// forgetting `prev` would leave ⌃B ; pointing at a truncated pane.
export function applyLayout(s: PaneState, l: Layout): PaneState {
  const newCount = paneCount(l);
  let next = s.panes.slice();
  if (s.active >= newCount && next[s.active]) {
    // Promote the focused session into the last surviving slot.
    next[newCount - 1] = next[s.active]!;
  }
  next = Array.from({ length: newCount }, (_, i) => next[i] ?? null);
  const clamp = (v: number) => Math.max(0, Math.min(newCount - 1, v));
  // Spread, don't rebuild: a literal `{ layout, panes, active }` silently drops
  // any field added to PaneState later — which is exactly how `prev` would have
  // been lost on every layout change.
  return { ...s, layout: l, panes: next, active: clamp(s.active), prev: clamp(s.prev) };
}

// loadPaneState restores the persisted layout/panes/active, migrating older
// shapes in place: the v2 blob first, then the pre-hub v1 (bare-name) blob — both
// run through normalizePaneState, which upgrades strings to refs — then the
// legacy single-session key as a last resort so existing users land where they
// were. Layout 5/6 (added later) have pane counts 2/3, so the array length is
// derived via paneCount(), not from the layout integer.
export function loadPaneState(): PaneState {
  try {
    const v2 = localStorage.getItem(PANES_KEY);
    if (v2) return normalizePaneState(JSON.parse(v2));
    const v1 = localStorage.getItem(PANES_KEY_V1);
    if (v1) return normalizePaneState(JSON.parse(v1));
  } catch {
    /* fall through to the legacy key */
  }
  const legacy = localStorage.getItem(LAST_KEY);
  return {
    layout: 1,
    panes: [legacy ? { name: legacy, machine: "" } : null],
    active: 0,
    prev: 0,
  };
}

// cycleSessionInPane returns the next/prev session *name* to mount in `paneIdx`,
// skipping sessions already mounted in other panes (the one-session-per-pane
// invariant). When the active pane is empty, ↓ starts at the first available
// session and ↑ at the last. Returns null if there's nothing to cycle to. The
// caller resolves the returned name to its owning machine. (Cycling is keyed by
// name only — a power-user convenience; on the rare cross-machine name clash it
// lands on the first match, which the grouped picker can correct.)
export function cycleSessionInPane(
  current: (PaneRef | null)[],
  paneIdx: number,
  sessions: string[],
  dir: 1 | -1
): string | null {
  const taken = new Set<string>();
  current.forEach((p, i) => {
    if (i !== paneIdx && p) taken.add(p.name);
  });
  const avail = sessions.filter((n) => !taken.has(n));
  if (avail.length === 0) return null;
  const cur = current[paneIdx]?.name ?? null;
  // Empty pane: dir +1 from "-1" lands on 0; dir -1 from "len" lands on last.
  let idx = cur ? avail.indexOf(cur) : dir > 0 ? -1 : avail.length;
  if (cur && idx < 0) idx = dir > 0 ? -1 : 0; // current dropped from list
  const next = avail[(idx + dir + avail.length) % avail.length];
  return next === cur ? null : next;
}
