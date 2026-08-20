// Proposal 0086 Part B — "which machine did I mean?".
//
// The create panel used to preselect `currentSession?.machine ||
// firstOnlineMachine`, and from an empty pane that second arm means *the
// alphabetically-first-by-id online agent* (the hub sorts its roster by opaque
// machine id, `registry.rs::machines_for`) — a fact that correlates with
// nothing the user cares about. [0078] already built the recency signal this
// wants; it was only ever consumed for the `Recent` *session* section.
//
// This module is the whole derivation, and it is deliberately pure so it can be
// unit-tested without React or a DOM (the convention paneState.ts/
// sessionRecents.ts state). It reads the [0078] store's list; it never writes
// it — that store stays the single writer of `ccweb.sessionRecents.v1`.
//
// Scope guard (B5): the MRU order exists in exactly two places — the create
// panel's default machine and its `<select>`. The hub's roster order is
// unchanged, and so is every other consumer of it (the drawer's machine-grouped
// session list, the dashboard, the TUI).

import type { MachineInfo, Session } from "./api";
import type { RecentRef } from "./sessionRecents";

// machineMruOrder ranks the roster "most recently used first", in three tiers:
//
//   1. machines named by `recents`, in recents order, deduped by first
//      appearance — `recents[0]` is by construction the machine of the last
//      session the user actually dwelt in (the store's 1s dwell gate already
//      filtered out passing-through noise);
//   2. machines never focused on this client, ranked by the freshest
//      `session.activity` seen on them (a stable sort, so equal activity keeps
//      roster order);
//   3. everything else, in roster order.
//
// Then one stable partition: online before offline. Offline options are already
// `disabled` in the select, so they sink to the bottom instead of interleaving.
//
// It iterates the ROSTER, keyed by recents — so a remembered machine that is
// gone can never be preselected ([0078] A5's retain-on-absence stays in the
// store, where it belongs). Empty recents + empty sessions ⇒ the roster,
// partitioned online-first: today's behaviour.
export function machineMruOrder(
  recents: readonly RecentRef[] | undefined,
  sessions: readonly Session[] | undefined,
  machines: readonly MachineInfo[] | undefined
): MachineInfo[] {
  const roster = machines ?? [];
  if (roster.length < 2) return [...roster];

  const byId = new Map<string, MachineInfo>();
  for (const m of roster) if (!byId.has(m.machine)) byId.set(m.machine, m);

  const out: MachineInfo[] = [];
  const taken = new Set<string>();
  const take = (m: MachineInfo) => {
    taken.add(m.machine);
    out.push(m);
  };

  // Tier 1 — recency, verbatim.
  for (const r of recents ?? []) {
    const id = r?.machine ?? "";
    if (taken.has(id)) continue;
    const m = byId.get(id);
    if (m) take(m);
  }

  // Tier 2 — never focused here, but the machine has sessions: freshest wins.
  const freshest = new Map<string, number>();
  for (const s of sessions ?? []) {
    const id = s.machine ?? "";
    if (taken.has(id) || !byId.has(id)) continue;
    const a = typeof s.activity === "number" ? s.activity : 0;
    const prev = freshest.get(id);
    if (prev === undefined || a > prev) freshest.set(id, a);
  }
  const tier2 = roster.filter((m) => !taken.has(m.machine) && freshest.has(m.machine));
  // Array.prototype.sort is stable, so equal-activity machines keep roster
  // order — the same tie posture [0028] pins for the switcher's ranking.
  tier2.sort((a, b) => (freshest.get(b.machine) ?? 0) - (freshest.get(a.machine) ?? 0));
  for (const m of tier2) take(m);

  // Tier 3 — the remainder, roster order.
  for (const m of roster) if (!taken.has(m.machine)) take(m);

  return [...out.filter((m) => m.online), ...out.filter((m) => !m.online)];
}

// The machine the create panel should preselect when nothing more deliberate
// (a [0056] seed, or the pane's own live session) says otherwise: the most
// recently used machine that is actually reachable. "" when nothing is online —
// today's behaviour, and the caller's `firstOnlineMachine` fallback then agrees.
export function mruDefaultMachine(ordered: readonly MachineInfo[]): string {
  return ordered.find((m) => m.online)?.machine ?? "";
}
