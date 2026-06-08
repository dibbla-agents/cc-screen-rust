import { describe, expect, it } from "vitest";
import type { Session } from "./api";
import {
  detectReadyEdges,
  sessionKey,
  NOTIFY_MIN_WORK_SECS,
  NOTIFY_INPUT_QUIET_SECS,
} from "./readyEdges";

// A fixed "now" in unix seconds; helpers build timestamps relative to it.
const NOW_S = 1_800_000_000;
const NOW_MS = NOW_S * 1000;
const longAgo = (slack = 5) => NOW_S - (NOTIFY_MIN_WORK_SECS + slack);

// mk builds a Session with sane defaults; override only what a case cares about.
function mk(over: Partial<Session> & { name: string }): Session {
  return {
    tool: "claude",
    short: over.name,
    attached: false,
    activity: 0,
    preview: "",
    waiting: false,
    ...over,
  };
}

const NONE = new Set<string>();

describe("detectReadyEdges", () => {
  it("fires on a gated busy→waiting edge (worked >1min, user idle >1min)", () => {
    const prev = [mk({ name: "a", waiting: false })];
    const cur = [
      mk({ name: "a", waiting: true, busy_since: longAgo(), last_input_at: longAgo() }),
    ];
    const edges = detectReadyEdges(prev, cur, NONE, NOW_MS);
    expect(edges).toEqual([{ name: "a", machine: "", tool: "claude", short: "a" }]);
  });

  it("rejects gate 1: a trivial <1min turn produces no toast", () => {
    const prev = [mk({ name: "a", waiting: false })];
    const cur = [
      mk({
        name: "a",
        waiting: true,
        busy_since: NOW_S - 10, // only worked 10s
        last_input_at: longAgo(),
      }),
    ];
    expect(detectReadyEdges(prev, cur, NONE, NOW_MS)).toEqual([]);
  });

  it("rejects gate 2: the user typed within the last minute", () => {
    const prev = [mk({ name: "a", waiting: false })];
    const cur = [
      mk({
        name: "a",
        waiting: true,
        busy_since: longAgo(),
        last_input_at: NOW_S - 5, // typed 5s ago
      }),
    ];
    expect(detectReadyEdges(prev, cur, NONE, NOW_MS)).toEqual([]);
  });

  it("rejects busy_since == 0 (never recorded a work start)", () => {
    const prev = [mk({ name: "a", waiting: false })];
    const cur = [
      mk({ name: "a", waiting: true, busy_since: 0, last_input_at: longAgo() }),
    ];
    expect(detectReadyEdges(prev, cur, NONE, NOW_MS)).toEqual([]);
  });

  it("first snapshot establishes a baseline and toasts nothing", () => {
    // No previous entry for the session ⇒ first sight ⇒ no edge, even if it is
    // already waiting and otherwise gated.
    const prev: Session[] = [];
    const cur = [
      mk({ name: "a", waiting: true, busy_since: longAgo(), last_input_at: longAgo() }),
    ];
    expect(detectReadyEdges(prev, cur, NONE, NOW_MS)).toEqual([]);
  });

  it("excludes a session mounted in a pane", () => {
    const prev = [mk({ name: "a", machine: "pine", waiting: false })];
    const cur = [
      mk({
        name: "a",
        machine: "pine",
        waiting: true,
        busy_since: longAgo(),
        last_input_at: longAgo(),
      }),
    ];
    const mounted = new Set([sessionKey({ name: "a", machine: "pine" })]);
    expect(detectReadyEdges(prev, cur, mounted, NOW_MS)).toEqual([]);
  });

  it("does not fire when already waiting in the previous snapshot (no edge)", () => {
    const prev = [
      mk({ name: "a", waiting: true, busy_since: longAgo(), last_input_at: longAgo() }),
    ];
    const cur = [
      mk({ name: "a", waiting: true, busy_since: longAgo(), last_input_at: longAgo() }),
    ];
    expect(detectReadyEdges(prev, cur, NONE, NOW_MS)).toEqual([]);
  });

  it("does not fire on a waiting→busy edge (started working)", () => {
    const prev = [mk({ name: "a", waiting: true })];
    const cur = [mk({ name: "a", waiting: false })];
    expect(detectReadyEdges(prev, cur, NONE, NOW_MS)).toEqual([]);
  });

  it("treats negative ages as 0 (clock skew can't satisfy a gate)", () => {
    // busy_since / last_input_at in the future ⇒ negative age ⇒ clamped to 0 ⇒
    // both gates reject.
    const prev = [mk({ name: "a", waiting: false })];
    const cur = [
      mk({ name: "a", waiting: true, busy_since: NOW_S + 100, last_input_at: NOW_S + 100 }),
    ];
    expect(detectReadyEdges(prev, cur, NONE, NOW_MS)).toEqual([]);
  });

  it("keys by (machine, name): same name on two agents is distinct", () => {
    const prev = [
      mk({ name: "a", machine: "pine", waiting: false }),
      mk({ name: "a", machine: "studio", waiting: true, busy_since: longAgo(), last_input_at: longAgo() }),
    ];
    const cur = [
      mk({ name: "a", machine: "pine", waiting: true, busy_since: longAgo(), last_input_at: longAgo() }),
      mk({ name: "a", machine: "studio", waiting: true, busy_since: longAgo(), last_input_at: longAgo() }),
    ];
    // Only pine crossed the edge; studio was already waiting.
    const edges = detectReadyEdges(prev, cur, NONE, NOW_MS);
    expect(edges).toEqual([{ name: "a", machine: "pine", tool: "claude", short: "a" }]);
  });

  it("emits one edge per qualifying session in a multi-session snapshot", () => {
    const prev = [
      mk({ name: "a", waiting: false }),
      mk({ name: "b", waiting: false }),
    ];
    const cur = [
      mk({ name: "a", waiting: true, busy_since: longAgo(), last_input_at: longAgo() }),
      mk({ name: "b", waiting: true, busy_since: NOW_S - 5, last_input_at: longAgo() }), // gate 1 fail
    ];
    const edges = detectReadyEdges(prev, cur, NONE, NOW_MS);
    expect(edges.map((e) => e.name)).toEqual(["a"]);
  });

  it("exactly at the threshold qualifies (>= semantics, matching the server)", () => {
    const prev = [mk({ name: "a", waiting: false })];
    const cur = [
      mk({
        name: "a",
        waiting: true,
        busy_since: NOW_S - NOTIFY_MIN_WORK_SECS,
        last_input_at: NOW_S - NOTIFY_INPUT_QUIET_SECS,
      }),
    ];
    expect(detectReadyEdges(prev, cur, NONE, NOW_MS)).toHaveLength(1);
  });
});
