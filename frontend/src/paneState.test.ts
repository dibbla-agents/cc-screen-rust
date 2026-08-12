import { describe, expect, it } from "vitest";
import {
  applyLayout,
  cycleSessionInPane,
  normalizePane,
  normalizePaneState,
  type PaneState,
} from "./paneState";
import { prefixArmed, setPrefixArmed } from "./prefix";

describe("normalizePane", () => {
  it("upgrades a v1 bare-name string to a ref with machine ''", () => {
    expect(normalizePane("claude-x")).toEqual({ name: "claude-x", machine: "" });
  });

  it("passes a v2 {name,machine} ref through", () => {
    expect(normalizePane({ name: "claude-x", machine: "laptop" })).toEqual({
      name: "claude-x",
      machine: "laptop",
    });
  });

  it("defaults a missing/non-string machine to ''", () => {
    expect(normalizePane({ name: "claude-x" })).toEqual({ name: "claude-x", machine: "" });
    expect(normalizePane({ name: "claude-x", machine: 7 })).toEqual({
      name: "claude-x",
      machine: "",
    });
  });

  it("maps empty / null / garbage to null (empty slot)", () => {
    expect(normalizePane("")).toBeNull();
    expect(normalizePane(null)).toBeNull();
    expect(normalizePane(undefined)).toBeNull();
    expect(normalizePane({ machine: "laptop" })).toBeNull(); // no name
    expect(normalizePane(42)).toBeNull();
  });
});

describe("normalizePaneState", () => {
  it("migrates a v1 blob (array of name strings) to refs", () => {
    const s = normalizePaneState({ layout: 1, panes: ["claude-x"], active: 0 });
    expect(s).toEqual({
      layout: 1,
      panes: [{ name: "claude-x", machine: "" }],
      active: 0,
      prev: 0,
    });
  });

  it("preserves a v2 blob and sizes panes to the layout", () => {
    // layout 4 → 4 panes; missing slots fill with null, extras are dropped.
    const s = normalizePaneState({
      layout: 4,
      panes: [{ name: "a", machine: "m1" }, null, { name: "b", machine: "m2" }],
      active: 2,
    });
    expect(s.layout).toBe(4);
    expect(s.panes).toEqual([
      { name: "a", machine: "m1" },
      null,
      { name: "b", machine: "m2" },
      null,
    ]);
    expect(s.active).toBe(2);
  });

  it("clamps a bogus layout and active, and tolerates a missing panes array", () => {
    const s = normalizePaneState({ layout: 99, active: 99 });
    expect(s.layout).toBe(6); // clamped to max
    expect(s.active).toBeGreaterThanOrEqual(0);
    expect(s.panes.every((p) => p === null)).toBe(true);
  });

  it("falls back to a single empty pane for total garbage", () => {
    expect(normalizePaneState(null)).toEqual({ layout: 1, panes: [null], active: 0, prev: 0 });
    expect(normalizePaneState("nonsense")).toEqual({
      layout: 1,
      panes: [null],
      active: 0,
      prev: 0,
    });
  });
});

// Proposal 0081 Part D — `prev` is what ⌃B ; toggles back to. It must survive
// a blob written before it existed (no migration, no key bump) and must never
// point outside the current layout.
describe("normalizePaneState — prev (0081 Part D)", () => {
  it("defaults a missing prev to 0 (a pre-0081 blob loads clean)", () => {
    const s = normalizePaneState({ layout: 4, panes: [], active: 2 });
    expect(s.prev).toBe(0);
  });

  it("preserves an in-range prev", () => {
    expect(normalizePaneState({ layout: 4, active: 0, prev: 3 }).prev).toBe(3);
  });

  it("clamps an out-of-range prev into the layout's pane count", () => {
    // layout 5 is *stacked* — id 5, but only 2 panes. Clamping on the id would
    // let prev be 4 (the whole point of Part A).
    expect(normalizePaneState({ layout: 5, active: 0, prev: 4 }).prev).toBe(1);
    expect(normalizePaneState({ layout: 6, active: 0, prev: 9 }).prev).toBe(2);
    expect(normalizePaneState({ layout: 4, active: 0, prev: -3 }).prev).toBe(0);
  });

  it("maps a non-numeric prev to 0", () => {
    expect(normalizePaneState({ layout: 4, prev: "two" }).prev).toBe(0);
    expect(normalizePaneState({ layout: 4, prev: null }).prev).toBe(0);
    expect(normalizePaneState({ layout: 4, prev: {} }).prev).toBe(0);
  });
});

describe("applyLayout (0081 Part D)", () => {
  const ref = (name: string) => ({ name, machine: "" });
  const state = (over: Partial<PaneState> = {}): PaneState => ({
    layout: 4,
    panes: [ref("a"), ref("b"), ref("c"), ref("d")],
    active: 0,
    prev: 0,
    ...over,
  });

  it("grows the panes array with empty slots", () => {
    const s = applyLayout({ layout: 1, panes: [ref("a")], active: 0, prev: 0 }, 4);
    expect(s.panes).toEqual([ref("a"), null, null, null]);
  });

  it("sizes by pane count, not layout id (layout 5 = 2 panes)", () => {
    expect(applyLayout(state(), 5).panes).toHaveLength(2);
    expect(applyLayout(state(), 6).panes).toHaveLength(3);
  });

  it("promotes the focused session into the last surviving slot on shrink", () => {
    const s = applyLayout(state({ active: 3 }), 1);
    expect(s.panes).toEqual([ref("d")]);
    expect(s.active).toBe(0);
  });

  it("clamps prev on shrink — ⌃B ; must never point at a truncated pane", () => {
    const s = applyLayout(state({ active: 0, prev: 3 }), 5); // quad → stacked
    expect(s.prev).toBe(1);
    expect(s.prev).toBeLessThan(s.panes.length);
  });

  it("keeps prev when it still fits", () => {
    expect(applyLayout(state({ active: 0, prev: 2 }), 6).prev).toBe(2);
  });

  it("carries unknown fields through (spreads, does not rebuild)", () => {
    // The literal-return version of this reducer silently dropped every field
    // added to PaneState after it was written. That is how `prev` would have
    // been lost on each layout change.
    const s = applyLayout({ ...state(), future: 42 } as unknown as PaneState, 2);
    expect((s as unknown as { future: number }).future).toBe(42);
  });
});

describe("cycleSessionInPane", () => {
  const ref = (name: string, machine = "") => ({ name, machine });

  it("cycles to the next session name, wrapping", () => {
    const panes = [ref("a"), null];
    expect(cycleSessionInPane(panes, 0, ["a", "b", "c"], 1)).toBe("b");
    expect(cycleSessionInPane([ref("c"), null], 0, ["a", "b", "c"], 1)).toBe("a");
  });

  it("skips sessions already mounted in other panes (one-per-pane)", () => {
    // pane 0 is empty; "b" is taken by pane 1, so ↓ from empty lands on "a".
    const panes = [null, ref("b")];
    expect(cycleSessionInPane(panes, 0, ["a", "b"], 1)).toBe("a");
  });

  it("returns null when there is nothing new to cycle to", () => {
    expect(cycleSessionInPane([ref("a")], 0, ["a"], 1)).toBeNull();
    expect(cycleSessionInPane([null], 0, [], 1)).toBeNull();
  });
});

// The ⌃B-armed flag that keeps the empty pane's switcher from acting on a chord
// key App is about to consume (0081 Part C). Order of window capture listeners
// is not something to depend on — this is the explicit answer instead.
describe("prefixArmed (0081 Part C)", () => {
  it("is false until the prefix is armed, and false again once cleared", () => {
    expect(prefixArmed()).toBe(false);
    setPrefixArmed(true);
    expect(prefixArmed()).toBe(true);
    setPrefixArmed(false);
    expect(prefixArmed()).toBe(false);
  });
});
