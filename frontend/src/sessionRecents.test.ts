import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Session } from "./api";
import {
  MAX_REMEMBERED,
  RECENTS_KEY,
  SessionRecentsStore,
  parseRecents,
  promoteInto,
  recentKey,
  recentSectionKeys,
  recordInto,
  removeFrom,
  renderCap,
  type RecentRef,
} from "./sessionRecents";

const ref = (name: string, machine = ""): RecentRef => ({ name, machine });

function sess(name: string, machine = ""): Session {
  return {
    name,
    short: name,
    tool: "cc",
    cwd: "/home/erik",
    preview: "",
    activity: 0,
    waiting: false,
    attached: false,
    machine,
  } as unknown as Session;
}

describe("recentKey", () => {
  it("keys on (machine, name) and tolerates an empty machine", () => {
    expect(recentKey(ref("api"))).toBe("/api");
    expect(recentKey(ref("api", "pine"))).toBe("pine/api");
    // Two same-named sessions on different agents are two different entries.
    expect(recentKey(ref("api", "pine"))).not.toBe(recentKey(ref("api", "studio")));
  });
});

describe("parseRecents", () => {
  it("drops garbage, de-dupes and caps", () => {
    expect(parseRecents(null)).toEqual([]);
    expect(parseRecents("nope")).toEqual([]);
    expect(parseRecents([1, null, { machine: "pine" }, { name: "" }])).toEqual([]);
    expect(parseRecents([{ name: "a" }, { name: "a", machine: "" }])).toEqual([ref("a")]);
    const many = Array.from({ length: 40 }, (_, i) => ({ name: `s${i}`, machine: "m" }));
    expect(parseRecents(many)).toHaveLength(MAX_REMEMBERED);
  });

  it("defaults a missing machine to ''", () => {
    expect(parseRecents([{ name: "a" }])).toEqual([{ name: "a", machine: "" }]);
  });
});

describe("promoteInto / recordInto / removeFrom", () => {
  it("promotes to the head and de-dupes", () => {
    const l = promoteInto(promoteInto([], ref("a")), ref("b"));
    expect(l.map((r) => r.name)).toEqual(["b", "a"]);
    expect(promoteInto(l, ref("a")).map((r) => r.name)).toEqual(["a", "b"]);
  });

  it("is identity-stable when already at the head (idempotent promote)", () => {
    const l = promoteInto([], ref("a"));
    expect(promoteInto(l, ref("a"))).toBe(l);
  });

  it("caps the stored list at 20", () => {
    let l: RecentRef[] = [];
    for (let i = 0; i < 30; i++) l = promoteInto(l, ref(`s${i}`));
    expect(l).toHaveLength(MAX_REMEMBERED);
    expect(l[0].name).toBe("s29");
  });

  it("records a background mount below the head, never above it", () => {
    const l = promoteInto([], ref("focused"));
    const after = recordInto(l, ref("bg"));
    expect(after.map((r) => r.name)).toEqual(["focused", "bg"]);
    // Idempotent in both already-present positions.
    expect(recordInto(after, ref("bg"))).toBe(after);
    expect(recordInto(after, ref("focused"))).toBe(after);
  });

  it("removes only on an explicit forget", () => {
    const l = promoteInto(promoteInto([], ref("a")), ref("b"));
    expect(removeFrom(l, ref("a")).map((r) => r.name)).toEqual(["b"]);
    expect(removeFrom(l, ref("zzz"))).toBe(l);
  });
});

describe("recentSectionKeys", () => {
  const sessions = [sess("a", "pine"), sess("b", "pine"), sess("c", "studio")];

  it("keeps MRU order, skips absent and mounted sessions, and caps", () => {
    const recents = [ref("c", "studio"), ref("gone", "pine"), ref("a", "pine"), ref("b", "pine")];
    const keys = recentSectionKeys(recents, sessions, new Set(["pine/a"]), 10);
    expect(keys).toEqual(["studio/c", "pine/b"]);
  });

  it("applies the cap after the drops", () => {
    const recents = [ref("a", "pine"), ref("b", "pine"), ref("c", "studio")];
    expect(recentSectionKeys(recents, sessions, new Set(), 2)).toEqual(["pine/a", "pine/b"]);
  });

  it("never renders an entry it cannot resolve", () => {
    expect(recentSectionKeys([ref("ghost")], sessions, new Set(), 10)).toEqual([]);
  });
});

describe("renderCap", () => {
  it("is 5 in a pane or on a short drawer, 10 otherwise", () => {
    expect(renderCap(false, 900)).toBe(10);
    expect(renderCap(false, 640)).toBe(5);
    expect(renderCap(true, 1200)).toBe(5);
  });
});

describe("SessionRecentsStore", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    localStorage.clear();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  const mk = () => new SessionRecentsStore({ dwellMs: 1000, flushMs: 2000 });

  it("promotes a focus only after the dwell, and persists after the debounce", () => {
    const s = mk();
    s.focus(ref("a", "pine"));
    vi.advanceTimersByTime(999);
    expect(s.list()).toEqual([]);
    vi.advanceTimersByTime(1);
    expect(s.list().map((r) => r.name)).toEqual(["a"]);
    expect(localStorage.getItem(RECENTS_KEY)).toBeNull(); // still inside the debounce
    vi.advanceTimersByTime(2000);
    expect(JSON.parse(localStorage.getItem(RECENTS_KEY)!)).toEqual([{ machine: "pine", name: "a" }]);
  });

  it("writes nothing for sessions passed through in under a second", () => {
    const s = mk();
    s.focus(ref("a"));
    vi.advanceTimersByTime(200);
    s.focus(ref("b"));
    vi.advanceTimersByTime(200);
    s.focus(ref("c"));
    vi.advanceTimersByTime(200);
    s.focus(null);
    vi.advanceTimersByTime(5000);
    expect(s.list()).toEqual([]);
    expect(localStorage.getItem(RECENTS_KEY)).toBeNull();
  });

  it("re-focusing the session already at the head writes nothing", () => {
    const s = mk();
    s.focus(ref("a"));
    vi.advanceTimersByTime(3000);
    const notified = vi.fn();
    s.subscribe(notified);
    s.focus(ref("a"));
    vi.advanceTimersByTime(5000);
    expect(notified).not.toHaveBeenCalled();
  });

  it("notifies subscribers when the list changes", () => {
    const s = mk();
    const seen: string[][] = [];
    s.subscribe(() => seen.push(s.list().map((r) => r.name)));
    s.focus(ref("a"));
    vi.advanceTimersByTime(1000);
    s.record(ref("bg"));
    expect(seen).toEqual([["a"], ["a", "bg"]]);
  });

  it("read-modify-writes against a store mutated behind its back (two tabs)", () => {
    const s = mk();
    s.focus(ref("mine"));
    vi.advanceTimersByTime(1000);
    // Another tab writes its own history inside our debounce window.
    localStorage.setItem(RECENTS_KEY, JSON.stringify([{ machine: "", name: "theirs" }]));
    vi.advanceTimersByTime(2000);
    expect(JSON.parse(localStorage.getItem(RECENTS_KEY)!).map((r: RecentRef) => r.name)).toEqual([
      "mine",
      "theirs",
    ]);
  });

  it("replays every mutation swallowed by one debounce window", () => {
    const s = mk();
    s.focus(ref("a"));
    vi.advanceTimersByTime(1000);
    s.record(ref("b"));
    localStorage.setItem(RECENTS_KEY, JSON.stringify([{ machine: "", name: "other" }]));
    vi.advanceTimersByTime(2000);
    expect(JSON.parse(localStorage.getItem(RECENTS_KEY)!).map((r: RecentRef) => r.name)).toEqual([
      "a",
      "b",
      "other",
    ]);
  });

  it("flushes on demand (selector open / page hide)", () => {
    const s = mk();
    s.focus(ref("a"));
    vi.advanceTimersByTime(1000);
    s.flush();
    expect(JSON.parse(localStorage.getItem(RECENTS_KEY)!).map((r: RecentRef) => r.name)).toEqual(["a"]);
  });

  it("forgets an explicitly deleted session and cancels its pending dwell", () => {
    const s = mk();
    s.focus(ref("a"));
    vi.advanceTimersByTime(1000);
    s.focus(ref("b"));
    s.forget(ref("b")); // deleted from the drawer mid-dwell
    vi.advanceTimersByTime(5000);
    expect(s.list().map((r) => r.name)).toEqual(["a"]);
    s.forget(ref("a"));
    s.flush();
    expect(JSON.parse(localStorage.getItem(RECENTS_KEY)!)).toEqual([]);
  });

  it("loads an existing history at construction and reloads on a cross-tab write", () => {
    localStorage.setItem(RECENTS_KEY, JSON.stringify([{ machine: "pine", name: "a" }]));
    const s = mk();
    expect(s.list().map((r) => r.name)).toEqual(["a"]);
    localStorage.setItem(RECENTS_KEY, JSON.stringify([{ machine: "pine", name: "z" }]));
    s.reload();
    expect(s.list().map((r) => r.name)).toEqual(["z"]);
  });

  it("degrades to an empty list on corrupt JSON", () => {
    localStorage.setItem(RECENTS_KEY, "{not json");
    expect(mk().list()).toEqual([]);
  });

  it("survives a throwing setItem (quota) with the in-memory list intact", () => {
    const storage = {
      getItem: () => null,
      setItem: () => {
        throw new Error("QuotaExceededError");
      },
    };
    const s = new SessionRecentsStore({ storage, dwellMs: 1000, flushMs: 2000 });
    s.focus(ref("a"));
    vi.advanceTimersByTime(1000);
    expect(() => vi.advanceTimersByTime(2000)).not.toThrow();
    expect(s.list().map((r) => r.name)).toEqual(["a"]);
  });

  it("works with no storage backend at all", () => {
    const s = new SessionRecentsStore({ storage: null, dwellMs: 1, flushMs: 1 });
    s.focus(ref("a"));
    vi.advanceTimersByTime(10);
    expect(s.list().map((r) => r.name)).toEqual(["a"]);
  });
});
