// The per-session viewer memory (0019) is what made proposal 0079's bug
// permanent: a dead path was restored, failed, and written straight back. These
// cover the pieces that stop that — the `cwd` field a rebase needs, the prune
// that self-cleans an unrecoverable entry, and the two-writer merge that must
// not let one writer clobber the other's field.
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  pruneViewerState,
  readViewerState,
  viewerKey,
  writeViewerState,
} from "./viewerState";

const KEY = "ccweb.viewerState.v1";

beforeEach(() => {
  localStorage.clear();
});

describe("viewerKey", () => {
  it("is (machine, session) — a same-named session on another agent is distinct", () => {
    expect(viewerKey("pine", "claude-x")).not.toEqual(viewerKey("studio", "claude-x"));
  });

  it("separates the two parts with NUL, which can appear in neither", () => {
    expect(viewerKey("pine", "claude-x")).toBe("pine\u0000claude-x");
  });

  it("tolerates a null session (no pane focused)", () => {
    expect(viewerKey("pine", null)).toBe("pine\u0000");
  });
});

describe("read/write", () => {
  it("returns null for an unknown key", () => {
    expect(readViewerState(viewerKey("pine", "nope"))).toBeNull();
  });

  it("merges the two writers' fields instead of clobbering", () => {
    const k = viewerKey("pine", "claude-x");
    // useDirTree writes expanded + cwd; EditorOverlay writes activePath.
    writeViewerState(k, { expanded: ["/home/u/proj"], cwd: "/home/u/proj" });
    writeViewerState(k, { activePath: "/home/u/proj/notes.md" });
    expect(readViewerState(k)).toEqual({
      expanded: ["/home/u/proj"],
      cwd: "/home/u/proj",
      activePath: "/home/u/proj/notes.md",
    });
  });

  it("keeps cwd optional — an entry written before 0079 still reads", () => {
    const k = viewerKey("pine", "claude-x");
    localStorage.setItem(KEY, JSON.stringify({ [k]: { expanded: [], activePath: "/home/u/a.md" } }));
    const got = readViewerState(k);
    expect(got?.activePath).toBe("/home/u/a.md");
    expect(got?.cwd).toBeUndefined();
  });

  it("survives corrupt storage by forgetting, never by throwing", () => {
    localStorage.setItem(KEY, "{not json");
    expect(readViewerState(viewerKey("pine", "claude-x"))).toBeNull();
    expect(() => writeViewerState(viewerKey("pine", "claude-x"), { activePath: "/x" })).not.toThrow();
  });

  it("degrades to 'no memory' when the write throws (quota), never to a broken editor", () => {
    const spy = vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("QuotaExceededError");
    });
    expect(() => writeViewerState(viewerKey("pine", "claude-x"), { activePath: "/x" })).not.toThrow();
    spy.mockRestore();
  });
});

describe("pruneViewerState", () => {
  it("forgets the entry so a failed path can't be written back", () => {
    const k = viewerKey("pine", "claude-x");
    writeViewerState(k, { activePath: "/home/u/proj-a/notes.md", cwd: "/home/u/proj-a" });
    pruneViewerState(k);
    expect(readViewerState(k)).toBeNull();
  });

  it("leaves other sessions alone and no-ops on an unknown key", () => {
    const a = viewerKey("pine", "a");
    const b = viewerKey("pine", "b");
    writeViewerState(a, { activePath: "/home/u/a.md" });
    writeViewerState(b, { activePath: "/home/u/b.md" });
    pruneViewerState(a);
    pruneViewerState(viewerKey("pine", "never-seen"));
    expect(readViewerState(a)).toBeNull();
    expect(readViewerState(b)?.activePath).toBe("/home/u/b.md");
  });
});

describe("MAX_SESSIONS trim", () => {
  it("drops the least-recently-touched entries past the cap", () => {
    for (let i = 0; i < 60; i++) {
      writeViewerState(viewerKey("pine", `s${i}`), { activePath: `/home/u/${i}.md` });
    }
    // The 10 oldest are gone; the newest survive.
    expect(readViewerState(viewerKey("pine", "s0"))).toBeNull();
    expect(readViewerState(viewerKey("pine", "s9"))).toBeNull();
    expect(readViewerState(viewerKey("pine", "s10"))?.activePath).toBe("/home/u/10.md");
    expect(readViewerState(viewerKey("pine", "s59"))?.activePath).toBe("/home/u/59.md");
  });

  it("re-touching an old entry moves it out of the trim window", () => {
    for (let i = 0; i < 50; i++) {
      writeViewerState(viewerKey("pine", `s${i}`), { activePath: `/home/u/${i}.md` });
    }
    writeViewerState(viewerKey("pine", "s0"), { cwd: "/home/u/proj" }); // touched → newest
    writeViewerState(viewerKey("pine", "extra"), { activePath: "/home/u/x.md" });
    expect(readViewerState(viewerKey("pine", "s0"))?.cwd).toBe("/home/u/proj");
    expect(readViewerState(viewerKey("pine", "s1"))).toBeNull(); // the new oldest
  });
});
