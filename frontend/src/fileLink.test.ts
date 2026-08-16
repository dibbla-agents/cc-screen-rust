import { describe, expect, it } from "vitest";
import {
  fileLinkPath,
  fileLinkUrl,
  joinHome,
  parseFileLink,
  parseLinkToken,
  relFromHome,
} from "./fileLink";

describe("parseFileLink", () => {
  it("parses a file link with a machine", () => {
    expect(parseFileLink("/file/studio/projects/personal-planning/tasks.md")).toEqual({
      machine: "studio",
      relPath: "projects/personal-planning/tasks.md",
      isDir: false,
    });
  });

  it("treats a trailing slash as the folder form", () => {
    expect(parseFileLink("/file/studio/projects/personal-planning/")).toEqual({
      machine: "studio",
      relPath: "projects/personal-planning",
      isDir: true,
    });
  });

  it("maps the `-` machine segment to the default machine", () => {
    expect(parseFileLink("/file/-/notes.md")).toEqual({
      machine: "",
      relPath: "notes.md",
      isDir: false,
    });
  });

  it("treats a bare machine as that machine's home folder", () => {
    expect(parseFileLink("/file/studio")).toEqual({ machine: "studio", relPath: "", isDir: true });
    expect(parseFileLink("/file/studio/")).toEqual({ machine: "studio", relPath: "", isDir: true });
  });

  it("percent-decodes every segment", () => {
    expect(parseFileLink("/file/mac%20studio/my%20docs/a%2Bb.md")).toEqual({
      machine: "mac studio",
      relPath: "my docs/a+b.md",
      isDir: false,
    });
  });

  it("returns null for anything that is not a file link", () => {
    expect(parseFileLink("/")).toBeNull();
    expect(parseFileLink("/files/x")).toBeNull();
    expect(parseFileLink("/invite/abc")).toBeNull();
    expect(parseFileLink("/file/")).toBeNull();
  });

  it("refuses traversal segments rather than sanitising them", () => {
    expect(parseFileLink("/file/studio/../../etc/passwd")).toBeNull();
    expect(parseFileLink("/file/studio/a/./b.md")).toBeNull();
  });

  it("returns null on an undecodable percent sequence", () => {
    expect(parseFileLink("/file/studio/%E0%A4%A.md")).toBeNull();
  });
});

describe("fileLinkPath", () => {
  it("round-trips through parseFileLink", () => {
    for (const [machine, rel, isDir] of [
      ["studio", "projects/tasks.md", false],
      ["", "notes.md", false],
      ["mac studio", "my docs", true],
      ["studio", "", true],
    ] as const) {
      expect(parseFileLink(fileLinkPath(machine, rel, isDir))).toEqual({
        machine,
        relPath: rel,
        isDir,
      });
    }
  });

  it("encodes each segment but keeps the separators", () => {
    expect(fileLinkPath("studio", "my docs/a b.md", false)).toBe("/file/studio/my%20docs/a%20b.md");
  });

  it("uses `-` for the default machine", () => {
    expect(fileLinkPath("", "a.md", false)).toBe("/file/-/a.md");
  });

  it("builds an absolute URL without doubling the slash", () => {
    expect(fileLinkUrl("https://app.ccscreen.dev/", "studio", "a.md", false)).toBe(
      "https://app.ccscreen.dev/file/studio/a.md"
    );
  });
});

describe("relFromHome / joinHome", () => {
  it("strips the home prefix", () => {
    expect(relFromHome("/home/erik", "/home/erik/projects/a.md")).toBe("projects/a.md");
    expect(relFromHome("/home/erik", "/home/erik")).toBe("");
  });

  it("returns null for a path outside home", () => {
    expect(relFromHome("/home/erik", "/etc/passwd")).toBeNull();
    // A sibling that merely shares the prefix is NOT under it.
    expect(relFromHome("/home/erik", "/home/erikson/a.md")).toBeNull();
    expect(relFromHome("", "/home/erik/a.md")).toBeNull();
  });

  it("round-trips with joinHome", () => {
    const abs = "/home/erik/projects/personal-planning/tasks.md";
    const rel = relFromHome("/home/erik", abs);
    expect(rel).not.toBeNull();
    expect(joinHome("/home/erik", rel as string)).toBe(abs);
    expect(joinHome("/home/erik/", "")).toBe("/home/erik");
  });
});

describe("parseLinkToken", () => {
  it("accepts a base64url token", () => {
    expect(parseLinkToken("/s/abcDEF-123_xyzabcDEF456")).toBe("abcDEF-123_xyzabcDEF456");
    expect(parseLinkToken("/s/abcDEF-123_xyzabcDEF456/")).toBe("abcDEF-123_xyzabcDEF456");
  });

  it("rejects the wrong shape before any request is made", () => {
    expect(parseLinkToken("/s/")).toBeNull();
    expect(parseLinkToken("/s/short")).toBeNull();
    expect(parseLinkToken("/s/has spaces and+slashes")).toBeNull();
    expect(parseLinkToken("/file/studio/a.md")).toBeNull();
  });
});
