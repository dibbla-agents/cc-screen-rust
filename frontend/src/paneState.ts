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
  const s = (raw ?? {}) as { layout?: unknown; panes?: unknown; active?: unknown };
  const layout = Math.max(1, Math.min(6, Math.floor(Number(s.layout) || 1))) as Layout;
  const count = paneCount(layout);
  const arr = Array.isArray(s.panes) ? (s.panes as unknown[]) : [];
  const panes = Array.from({ length: count }, (_, i) => normalizePane(arr[i]));
  const active = Math.max(0, Math.min(count - 1, Math.floor(Number(s.active) || 0)));
  return { layout, panes, active };
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
  return { layout: 1, panes: [legacy ? { name: legacy, machine: "" } : null], active: 0 };
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
