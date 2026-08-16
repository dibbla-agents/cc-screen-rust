import { describe, expect, it } from "vitest";
import { switchRestores, viewerKey } from "./viewerState";

// Proposal 0083 — the bug this pins cost a phone user three taps and looked
// like the deep link simply didn't work: the file opened, then the arriving
// session list looked like a session switch and [0019]'s per-session memory
// quietly replaced it with nothing (and popped the tree panel over it).
describe("switchRestores — an explicit request outranks the per-session memory", () => {
  const FILE = "/home/erik/projects/planning/tasks.md";

  it("does NOT restore while the requested file is still the open one", () => {
    // The session list settling from null → "claude-foo" hits this exact call.
    expect(switchRestores({ initialPath: FILE, initialDir: null, activePath: FILE })).toBe(false);
  });

  it("does NOT restore while a requested folder is still showing its tree", () => {
    expect(switchRestores({ initialPath: null, initialDir: "/home/erik/projects", activePath: null })).toBe(
      false
    );
  });

  it("DOES restore on a real switch — the user has navigated away from the request", () => {
    // ⌃B ↑/↓ or a drawer pick: the open file is no longer the requested one, so
    // [0019]'s "follow the session" behaviour is exactly what should happen.
    expect(
      switchRestores({ initialPath: FILE, initialDir: null, activePath: "/home/erik/other.md" })
    ).toBe(true);
  });

  it("DOES restore when nothing was ever requested (the ⌃B e / tree entry)", () => {
    expect(switchRestores({ initialPath: null, initialDir: null, activePath: null })).toBe(true);
    expect(switchRestores({ initialPath: null, initialDir: null, activePath: FILE })).toBe(true);
  });

  it("DOES restore once a requested file has been closed to the tree", () => {
    // The phone's two-step close: file → tree. From there a switch should
    // behave normally again.
    expect(switchRestores({ initialPath: FILE, initialDir: null, activePath: null })).toBe(true);
  });

  it("DOES restore once a file is open under a requested FOLDER", () => {
    expect(switchRestores({ initialPath: null, initialDir: "/home/erik/projects", activePath: FILE })).toBe(
      true
    );
  });
});

describe("viewerKey", () => {
  it("keys on (machine, session) — a same-named session elsewhere is a different one", () => {
    expect(viewerKey("pine", "claude-a")).not.toBe(viewerKey("studio", "claude-a"));
    expect(viewerKey("pine", null)).not.toBe(viewerKey("pine", "claude-a"));
  });
});
