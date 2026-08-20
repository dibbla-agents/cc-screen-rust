import { describe, expect, it } from "vitest";
import { machineMruOrder, mruDefaultMachine } from "./machineMru";
import type { MachineInfo, Session } from "./api";
import type { RecentRef } from "./sessionRecents";

// Proposal 0086 Part B — the pure MRU derivation behind the create panel's
// machine default and its `<select>` order. No React, no DOM: the ordering is
// the whole contract.

const m = (machine: string, online = true): MachineInfo => ({
  machine,
  hostname: `${machine}.local`,
  online,
});

const s = (machine: string, name: string, activity: number): Session => ({
  name,
  tool: "claude",
  short: name,
  attached: false,
  activity,
  preview: "",
  waiting: true,
  machine,
});

const r = (machine: string, name: string): RecentRef => ({ machine, name });

const ids = (list: MachineInfo[]) => list.map((x) => x.machine);

describe("machineMruOrder (proposal 0086 B1)", () => {
  const roster = [m("alpha"), m("bravo"), m("charlie")];

  it("puts machines in recents order, ahead of the roster order", () => {
    const recents = [r("charlie", "one"), r("bravo", "two")];
    expect(ids(machineMruOrder(recents, [], roster))).toEqual(["charlie", "bravo", "alpha"]);
  });

  it("dedupes by first appearance — a machine's later recents don't demote it", () => {
    const recents = [r("charlie", "one"), r("bravo", "two"), r("charlie", "three")];
    expect(ids(machineMruOrder(recents, [], roster))).toEqual(["charlie", "bravo", "alpha"]);
  });

  it("falls back to freshest session activity for never-focused machines", () => {
    // Nothing focused here; bravo's newest session is fresher than alpha's.
    const sessions = [s("alpha", "a1", 100), s("bravo", "b1", 50), s("bravo", "b2", 900)];
    expect(ids(machineMruOrder([], sessions, roster))).toEqual(["bravo", "alpha", "charlie"]);
  });

  it("keeps recents strictly above the activity tier", () => {
    // charlie has no sessions at all but was the last one used.
    const sessions = [s("alpha", "a1", 100), s("bravo", "b1", 900)];
    const recents = [r("charlie", "one")];
    expect(ids(machineMruOrder(recents, sessions, roster))).toEqual([
      "charlie",
      "bravo",
      "alpha",
    ]);
  });

  it("breaks an activity tie by roster order (stable sort)", () => {
    const sessions = [s("bravo", "b1", 7), s("alpha", "a1", 7)];
    expect(ids(machineMruOrder([], sessions, roster))).toEqual(["alpha", "bravo", "charlie"]);
  });

  it("partitions online before offline, keeping the MRU order inside each half", () => {
    const mixed = [m("alpha"), m("bravo", false), m("charlie")];
    const recents = [r("bravo", "one"), r("charlie", "two")];
    expect(ids(machineMruOrder(recents, [], mixed))).toEqual(["charlie", "alpha", "bravo"]);
  });

  it("ignores recents (and sessions) naming a machine that is not in the roster", () => {
    const recents = [r("ghost", "one"), r("bravo", "two")];
    const sessions = [s("ghost", "g1", 9999)];
    expect(ids(machineMruOrder(recents, sessions, roster))).toEqual([
      "bravo",
      "alpha",
      "charlie",
    ]);
  });

  it("degrades to the roster, online-first, with no recents and no sessions", () => {
    const mixed = [m("alpha", false), m("bravo"), m("charlie")];
    expect(ids(machineMruOrder([], [], mixed))).toEqual(["bravo", "charlie", "alpha"]);
  });

  it("returns every roster machine exactly once, whatever the inputs", () => {
    const recents = [r("charlie", "one"), r("ghost", "x"), r("charlie", "two")];
    const out = machineMruOrder(recents, [s("bravo", "b", 5)], roster);
    expect(out).toHaveLength(roster.length);
    expect(new Set(ids(out)).size).toBe(roster.length);
  });

  it("is a no-op on a single-machine deployment (and on an empty roster)", () => {
    expect(ids(machineMruOrder([r("solo", "x")], [], [m("solo")]))).toEqual(["solo"]);
    expect(machineMruOrder([], [], [])).toEqual([]);
    expect(machineMruOrder(undefined, undefined, undefined)).toEqual([]);
  });

  it("treats the direct-agent empty machine id as a real key", () => {
    const direct = [m(""), m("bravo")];
    expect(ids(machineMruOrder([r("", "local")], [], direct))).toEqual(["", "bravo"]);
  });
});

describe("mruDefaultMachine (proposal 0086 B2)", () => {
  it("is the first ONLINE machine of the ordered list, never an offline head", () => {
    expect(mruDefaultMachine([m("alpha", false), m("bravo")])).toBe("bravo");
  });

  it("is empty when nothing is online — today's behaviour", () => {
    expect(mruDefaultMachine([m("alpha", false)])).toBe("");
    expect(mruDefaultMachine([])).toBe("");
  });
});
