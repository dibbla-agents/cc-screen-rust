// Proposal 0078 — "where was I two sessions ago?".
//
// The selector's resting order answers "what needs my attention?" (0023/0024).
// This module is the other axis: the sessions you were most recently *working
// in*, most-recent first, so the top of the list is stable enough to build
// muscle memory on. It is deliberately NOT attention-ordered — a section that
// re-sorts when an agent finishes is a section you cannot learn.
//
// Kept pure/side-effect-free apart from the one storage seam so it is unit
// testable without React (see sessionRecents.test.ts), following the
// split-out-of-App.tsx convention paneState.ts states.
//
// Identity is `(machine, name)` — never `label`/`short` (0035: the slug is the
// routing key), and `machine` is "" for a client talking to a single agent
// directly, so an empty machine is a valid key rather than a missing one.

import { sessionKey } from "./readyEdges";
import type { Session } from "./api";

export const RECENTS_KEY = "ccweb.sessionRecents.v1"; // [{machine,name}], MRU first
export const MAX_REMEMBERED = 20; // stored
export const MAX_RENDERED = 10; // rendered on a tall drawer
export const MAX_RENDERED_SHORT = 5; // short drawer / pane-embedded (0078 B8, Mobile)
export const SHORT_DRAWER_PX = 700; // below this the drawer renders the small cap

// Dwell before a focused session is promoted: passing *through* a pane (⌃B ↑/↓,
// a click on the way somewhere else) must not rewrite the top of the list.
export const DWELL_MS = 1000;
// Trailing debounce on persistence. The in-memory list is authoritative; a
// crash loses at most one window, which is acceptable for a navigation hint.
export const FLUSH_MS = 2000;

export interface RecentRef {
  machine: string;
  name: string;
}

// The stored key. Same shape as readyEdges' sessionKey / App's refKey, reused
// rather than re-minted so the mounted-exclusion set lines up exactly.
export function recentKey(r: { name: string; machine?: string }): string {
  return sessionKey(r);
}

// parseRecents accepts anything and returns a valid list: non-objects, missing
// names and duplicates are dropped, and the cap is applied. A corrupt blob
// degrades to "no Recent section", never to a broken selector.
export function parseRecents(raw: unknown): RecentRef[] {
  if (!Array.isArray(raw)) return [];
  const out: RecentRef[] = [];
  const seen = new Set<string>();
  for (const v of raw) {
    if (!v || typeof v !== "object") continue;
    const o = v as { name?: unknown; machine?: unknown };
    if (typeof o.name !== "string" || !o.name) continue;
    const ref = { name: o.name, machine: typeof o.machine === "string" ? o.machine : "" };
    const k = recentKey(ref);
    if (seen.has(k)) continue;
    seen.add(k);
    out.push(ref);
    if (out.length >= MAX_REMEMBERED) break;
  }
  return out;
}

// promoteInto moves `ref` to the head (pure). Already-head is returned
// unchanged *by identity*, so callers can cheaply skip the write.
export function promoteInto(list: RecentRef[], ref: RecentRef): RecentRef[] {
  const k = recentKey(ref);
  if (list.length > 0 && recentKey(list[0]) === k) return list;
  return [{ machine: ref.machine, name: ref.name }, ...list.filter((r) => recentKey(r) !== k)].slice(
    0,
    MAX_REMEMBERED
  );
}

// recordInto inserts `ref` just *below* the head — a mount into a background
// pane is a real view event but must never outrank the session you are actually
// focused on (0078 A2). Already present at head or at position 1 → unchanged.
export function recordInto(list: RecentRef[], ref: RecentRef): RecentRef[] {
  const k = recentKey(ref);
  if (list.length === 0) return [{ machine: ref.machine, name: ref.name }];
  if (recentKey(list[0]) === k) return list;
  if (list.length > 1 && recentKey(list[1]) === k) return list;
  const rest = list.slice(1).filter((r) => recentKey(r) !== k);
  return [list[0], { machine: ref.machine, name: ref.name }, ...rest].slice(0, MAX_REMEMBERED);
}

// removeFrom drops an entry (pure). Only an explicit user-initiated kill/delete
// calls this: absence from a poll is NEVER a reason to forget (0078 A5) —
// absence is indistinguishable from an offline machine or a network blip.
export function removeFrom(list: RecentRef[], ref: RecentRef): RecentRef[] {
  const k = recentKey(ref);
  const next = list.filter((r) => recentKey(r) !== k);
  return next.length === list.length ? list : next;
}

// recentSectionKeys is the render pipeline, ordered and total (0078 A4): take
// the stored list (MRU) → drop entries absent from the current poll → drop
// entries mounted in any pane → take the first `cap`.
//
// Returns keys rather than Sessions so the caller can freeze membership when
// the selector opens (0078 A12) and still resolve fresh Session objects (status
// dot, badges, `ago`) on every poll tick.
export function recentSectionKeys(
  recents: RecentRef[],
  sessions: Session[],
  mounted: Set<string>,
  cap: number
): string[] {
  const live = new Set(sessions.map((s) => sessionKey(s)));
  const out: string[] = [];
  for (const r of recents) {
    const k = recentKey(r);
    if (!live.has(k)) continue; // absent from this poll — skipped, not forgotten
    if (mounted.has(k)) continue; // already on screen — the top of the list is what you'd switch TO
    out.push(k);
    if (out.length >= cap) break;
  }
  return out;
}

// The render cap: 10 on a tall drawer, 5 in a pane-embedded switcher or on a
// short viewport, where a header plus ten rows would push every machine group
// below the fold (0078 Mobile).
export function renderCap(pane: boolean, viewportPx: number): number {
  return pane || viewportPx < SHORT_DRAWER_PX ? MAX_RENDERED_SHORT : MAX_RENDERED;
}

// ── The store ───────────────────────────────────────────────────────────────

export interface StorageLike {
  getItem(k: string): string | null;
  setItem(k: string, v: string): void;
}

interface Opts {
  storage?: StorageLike | null;
  dwellMs?: number;
  flushMs?: number;
  setTimer?: (fn: () => void, ms: number) => number;
  clearTimer?: (id: number) => void;
}

// SessionRecentsStore holds the authoritative in-memory list and owns the two
// timing rules: the dwell gate on focus (A3 i) and the trailing debounce on
// persistence (A3 ii). Every actual write is a read-modify-write against the
// backing store (A11), so two tabs on one origin interleave into one coherent
// history instead of clobbering each other.
export class SessionRecentsStore {
  private storage: StorageLike | null;
  private dwellMs: number;
  private flushMs: number;
  private setTimer: (fn: () => void, ms: number) => number;
  private clearTimer: (id: number) => void;
  private mem: RecentRef[];
  private dwellTimer: number | null = null;
  private dwellKey: string | null = null;
  private flushTimer: number | null = null;
  private dirty: ((list: RecentRef[]) => RecentRef[]) | null = null;
  private subs = new Set<() => void>();

  constructor(opts: Opts = {}) {
    this.storage =
      opts.storage === undefined
        ? typeof localStorage !== "undefined"
          ? localStorage
          : null
        : opts.storage;
    this.dwellMs = opts.dwellMs ?? DWELL_MS;
    this.flushMs = opts.flushMs ?? FLUSH_MS;
    this.setTimer = opts.setTimer ?? ((fn, ms) => setTimeout(fn, ms) as unknown as number);
    this.clearTimer = opts.clearTimer ?? ((id) => clearTimeout(id));
    this.mem = this.read();
  }

  list(): RecentRef[] {
    return this.mem;
  }

  // focus starts the dwell gate for `ref`; a different target (or null, i.e. an
  // empty pane) cancels a pending promotion. Re-focusing the session already at
  // the head is a no-op at every layer.
  focus(ref: RecentRef | null): void {
    const k = ref ? recentKey(ref) : null;
    if (k === this.dwellKey) return;
    this.cancelDwell();
    if (!ref || !k) return;
    if (this.mem.length > 0 && recentKey(this.mem[0]) === k) return;
    this.dwellKey = k;
    this.dwellTimer = this.setTimer(() => {
      this.dwellTimer = null;
      this.dwellKey = null;
      this.apply((l) => promoteInto(l, ref));
    }, this.dwellMs);
  }

  // record is the background-mount edge: written at once (the user picked it
  // deliberately) but never above the focused pane's session.
  record(ref: RecentRef): void {
    this.apply((l) => recordInto(l, ref));
  }

  // forget is the ONLY prune edge besides the 20-cap: an explicit kill/delete.
  forget(ref: RecentRef): void {
    if (this.dwellKey === recentKey(ref)) this.cancelDwell();
    this.apply((l) => removeFrom(l, ref));
  }

  // reload re-reads the backing store — used by the cross-tab `storage` event so
  // two tabs don't render divergent sections.
  reload(): void {
    const next = this.read();
    this.mem = next;
    this.notify();
  }

  // flush persists any pending change now (selector open, page hide).
  flush(): void {
    if (this.flushTimer !== null) {
      this.clearTimer(this.flushTimer);
      this.flushTimer = null;
    }
    const mut = this.dirty;
    this.dirty = null;
    if (!mut || !this.storage) return;
    try {
      // Read-modify-write: another tab may have promoted since we last read.
      const merged = mut(this.read());
      this.storage.setItem(RECENTS_KEY, JSON.stringify(merged));
      this.mem = merged;
    } catch {
      /* quota / unwritable / disabled storage — the section just won't persist */
    }
  }

  subscribe(fn: () => void): () => void {
    this.subs.add(fn);
    return () => this.subs.delete(fn);
  }

  dispose(): void {
    this.cancelDwell();
    this.flush();
    this.subs.clear();
  }

  private cancelDwell() {
    if (this.dwellTimer !== null) this.clearTimer(this.dwellTimer);
    this.dwellTimer = null;
    this.dwellKey = null;
  }

  // apply mutates the in-memory list immediately (it is authoritative) and
  // schedules the trailing-debounced persist. A no-op mutation writes nothing
  // and notifies nobody.
  private apply(mut: (l: RecentRef[]) => RecentRef[]) {
    const next = mut(this.mem);
    if (next === this.mem) return;
    this.mem = next;
    // Compose pending mutations so a debounce window that swallowed two
    // promotes still replays both against whatever the store holds at flush.
    const prev = this.dirty;
    this.dirty = prev ? (l) => mut(prev(l)) : mut;
    if (this.flushTimer !== null) this.clearTimer(this.flushTimer);
    this.flushTimer = this.setTimer(() => {
      this.flushTimer = null;
      this.flush();
    }, this.flushMs);
    this.notify();
  }

  private notify() {
    for (const fn of this.subs) fn();
  }

  private read(): RecentRef[] {
    if (!this.storage) return [];
    try {
      const raw = this.storage.getItem(RECENTS_KEY);
      return raw ? parseRecents(JSON.parse(raw)) : [];
    } catch {
      return []; // corrupt / unreadable — today's selector, no error surfaced
    }
  }
}
