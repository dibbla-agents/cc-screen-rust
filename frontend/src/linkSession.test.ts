import { describe, expect, it } from "vitest";
import { isWithin, sessionForPath } from "./linkSession";
import { recentKey } from "./sessionRecents";

const HOME = "/Users/erik";
const s = (name: string, cwd: string | undefined, machine = "mac-studio") => ({ name, cwd, machine });

describe("isWithin", () => {
  it("matches whole path segments only", () => {
    expect(isWithin("/a/proj", "/a/proj/README.md")).toBe(true);
    expect(isWithin("/a/proj", "/a/proj")).toBe(true);
    expect(isWithin("/a/proj/", "/a/proj/x")).toBe(true);
    expect(isWithin("/a/proj", "/a/project/README.md")).toBe(false);
    expect(isWithin("/a/proj", "/a")).toBe(false);
    expect(isWithin("", "/a")).toBe(false);
  });
});

describe("sessionForPath", () => {
  const file = `${HOME}/work-projects/261027_platform-ux/README.md`;

  it("picks the session whose folder holds the file, not the focused one", () => {
    const sessions = [
      s("claude-260923_linkedin-leads", `${HOME}/work-projects/260923_linkedin-leads`),
      s("claude-261027_platform-ux", `${HOME}/work-projects/261027_platform-ux`),
    ];
    expect(sessionForPath(sessions, "mac-studio", file, HOME)?.name).toBe("claude-261027_platform-ux");
  });

  it("prefers the deepest folder", () => {
    const sessions = [
      s("claude-work", `${HOME}/work-projects`),
      s("claude-ux", `${HOME}/work-projects/261027_platform-ux`),
      s("claude-wide", `${HOME}/work-projects/`),
    ];
    expect(sessionForPath(sessions, "mac-studio", file, HOME)?.name).toBe("claude-ux");
  });

  it("finds nothing rather than guessing", () => {
    const sessions = [
      s("claude-other", `${HOME}/work-projects/260923_linkedin-leads`),
      s("claude-prefix", `${HOME}/work-projects/261027_platform`), // a prefix, not a parent
      s("no-cwd", undefined),
    ];
    expect(sessionForPath(sessions, "mac-studio", file, HOME)).toBeNull();
  });

  it("never counts a session sitting in $HOME", () => {
    expect(sessionForPath([s("shell-home", HOME), s("shell-home2", `${HOME}/`)], "mac-studio", file, HOME)).toBeNull();
  });

  it("only looks at the link's machine", () => {
    const sessions = [s("claude-ux", `${HOME}/work-projects/261027_platform-ux`, "pine")];
    expect(sessionForPath(sessions, "mac-studio", file, HOME)).toBeNull();
    // `-` links (machine "") match a single agent's sessions, which carry no machine.
    expect(sessionForPath([{ name: "claude-ux", cwd: `${HOME}/work-projects/261027_platform-ux` }], "", file, HOME)?.name)
      .toBe("claude-ux");
  });

  it("breaks a tie by most recently focused, then by name", () => {
    const dir = `${HOME}/work-projects/261027_platform-ux`;
    const sessions = [s("codex-ux", dir), s("claude-ux", dir), s("opencode-ux", dir)];
    expect(sessionForPath(sessions, "mac-studio", file, HOME)?.name).toBe("claude-ux");
    const recents = [recentKey(s("opencode-ux", dir)), recentKey(s("codex-ux", dir))];
    expect(sessionForPath(sessions, "mac-studio", file, HOME, recents)?.name).toBe("opencode-ux");
  });

  it("works for a folder link too", () => {
    const sessions = [s("claude-ux", `${HOME}/work-projects/261027_platform-ux`)];
    expect(sessionForPath(sessions, "mac-studio", `${HOME}/work-projects/261027_platform-ux`, HOME)?.name)
      .toBe("claude-ux");
    expect(sessionForPath(sessions, "mac-studio", `${HOME}/work-projects`, HOME)).toBeNull();
  });
});
