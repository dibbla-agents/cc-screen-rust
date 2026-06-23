import { describe, it, expect } from "vitest";
import { resolveWatchDir, buildTreeFilter, type DirContents, type TreeSection } from "./dirTree";
import type { DirEntry, FileEntry } from "../api";

// resolveWatchDir maps an /api/watch frame's `dir` to the cache key to re-list.
// The bug it guards (proposal 0034): a create frame whose path shape doesn't
// exactly match the cache key is silently dropped, so new files never appear.
describe("resolveWatchDir", () => {
  const cache = new Set(["/home/u/p", "/home/u/p/foo"]);
  const has = (k: string) => cache.has(k);

  it("returns the dir when it matches a cached folder exactly", () => {
    expect(resolveWatchDir("/home/u/p/foo", has)).toBe("/home/u/p/foo");
  });

  it("normalizes a trailing slash so a '…/foo/' frame matches the '…/foo' key", () => {
    // This is the silent-drop case: without trimming, has('/home/u/p/foo/') is
    // false and the create vanishes while siblings keep rendering.
    expect(resolveWatchDir("/home/u/p/foo/", has)).toBe("/home/u/p/foo");
  });

  it("returns null for a folder we aren't showing (never-expanded → non-goal)", () => {
    expect(resolveWatchDir("/home/u/p/unseen", has)).toBeNull();
    expect(resolveWatchDir("/home/u/p/unseen/", has)).toBeNull();
  });

  it("does not strip the root slash", () => {
    const rootCache = new Set(["/"]);
    expect(resolveWatchDir("/", (k) => rootCache.has(k))).toBe("/");
  });

  it("prefers the exact match over the trimmed one", () => {
    // A pathological cache holding both shapes: the as-is key wins.
    const both = new Set(["/a/", "/a"]);
    expect(resolveWatchDir("/a/", (k) => both.has(k))).toBe("/a/");
  });
});

// buildTreeFilter narrows the LOADED tree to a query (proposal 0038, Part C):
// matched files survive, their ancestor folders stay + auto-expand, and a flat
// render-order row list is produced for keyboard nav / the "shown" count.
describe("buildTreeFilter", () => {
  const d = (path: string): DirEntry => ({ name: path.slice(path.lastIndexOf("/") + 1), path });
  const f = (path: string): FileEntry => ({
    name: path.slice(path.lastIndexOf("/") + 1),
    path,
    size: 0,
    mtime: 0,
  });
  const folder = (path: string, dirs: string[], files: string[]): [string, DirContents] => [
    path,
    { path, dirs: dirs.map(d), files: files.map(f) },
  ];

  // A small project tree:
  //   /p
  //     /p/src        → auth.ts, main.ts
  //     /p/src/util   → helpers.ts
  //     /p/docs       → readme.md
  const cache = new Map<string, DirContents>([
    folder("/p", ["/p/src", "/p/docs"], ["/p/package.json"]),
    folder("/p/src", ["/p/src/util"], ["/p/src/auth.ts", "/p/src/main.ts"]),
    folder("/p/src/util", [], ["/p/src/util/helpers.ts"]),
    folder("/p/docs", [], ["/p/docs/readme.md"]),
  ]);
  const sections: TreeSection[] = [
    { key: "project", label: "p", sub: "", icon: "●", path: "/p" },
  ];

  it("keeps a matched file and all its ancestor folders, hides the rest", () => {
    const r = buildTreeFilter(cache, sections, "auth");
    expect(r.matchedFiles.has("/p/src/auth.ts")).toBe(true);
    // ancestors of the match are visible + auto-expanded
    expect(r.visibleDirs.has("/p/src")).toBe(true);
    expect(r.expandDirs.has("/p/src")).toBe(true);
    // a non-matching sibling file is not matched
    expect(r.matchedFiles.has("/p/src/main.ts")).toBe(false);
    // an unrelated folder with no matches doesn't show
    expect(r.visibleDirs.has("/p/docs")).toBe(false);
  });

  it("produces a flat render-order row list (dirs before files, depth-first)", () => {
    const r = buildTreeFilter(cache, sections, "helpers");
    expect(r.rows.map((row) => row.path)).toEqual([
      "/p/src", // ancestor dir
      "/p/src/util", // ancestor dir
      "/p/src/util/helpers.ts", // the match
    ]);
  });

  it("matches folder names too, keeping their ancestors", () => {
    const r = buildTreeFilter(cache, sections, "docs");
    expect(r.matchedDirs.has("/p/docs")).toBe(true);
    expect(r.visibleDirs.has("/p/docs")).toBe(true);
  });

  it("is case-insensitive and fuzzy (subsequence)", () => {
    // "atts" is a subsequence of "auth.ts" → matches via fuzzyScore.
    const r = buildTreeFilter(cache, sections, "Auth");
    expect(r.matchedFiles.has("/p/src/auth.ts")).toBe(true);
  });

  it("returns empty sets when nothing matches", () => {
    const r = buildTreeFilter(cache, sections, "zzzznope");
    expect(r.matchedFiles.size).toBe(0);
    expect(r.matchedDirs.size).toBe(0);
    expect(r.rows.length).toBe(0);
  });
});
