import { describe, expect, it } from "vitest";
import { cycleSessionInPane, normalizePane, normalizePaneState } from "./paneState";

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
    expect(s).toEqual({ layout: 1, panes: [{ name: "claude-x", machine: "" }], active: 0 });
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
    expect(normalizePaneState(null)).toEqual({ layout: 1, panes: [null], active: 0 });
    expect(normalizePaneState("nonsense")).toEqual({ layout: 1, panes: [null], active: 0 });
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
