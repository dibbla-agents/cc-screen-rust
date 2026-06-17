import { describe, it, expect } from "vitest";
import { resolveWatchDir } from "./dirTree";

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
