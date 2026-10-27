// Which live session a cold-opened file link belongs to.
//
// A `/file/<machine>/<path>` link names a file, not a session — so before this
// the viewer simply showed whatever session the restored grid had focused, and
// a README from one project opened beside another project's agent and tree.
// The link's session is now derived from the path: the session whose working
// directory holds the file. No match means no session — never a guess.
//
// Pure (no React, no fetch) so the whole rule is unit-tested.

import { recentKey } from "./sessionRecents";

export interface LinkCandidate {
  name: string;
  machine?: string;
  cwd?: string;
}

const trimSlash = (p: string) => (p.length > 1 ? p.replace(/\/+$/, "") : p);

/** True when `path` is `dir` itself or lies under it — whole segments only. */
export function isWithin(dir: string, path: string): boolean {
  const d = trimSlash(dir);
  const p = trimSlash(path);
  if (!d || !p) return false;
  if (p === d) return true;
  return p.startsWith(d === "/" ? "/" : `${d}/`);
}

/**
 * The session a link to `abs` (on `machine`, whose `$HOME` is `home`) should
 * open beside, or null.
 *
 * - Only sessions on the link's machine whose cwd contains the path count.
 * - A session sitting in `$HOME` itself never counts: it would "contain" every
 *   file the link could name, which is no signal at all.
 * - The deepest cwd wins (`work-projects/x` beats `work-projects`).
 * - Ties (Claude and Codex in one folder) go to the most recently focused,
 *   per [0078]'s recents list (`recentKey`s, most recent first), then by name
 *   so the answer is stable.
 */
export function sessionForPath<T extends LinkCandidate>(
  sessions: readonly T[],
  machine: string,
  abs: string,
  home: string,
  recentKeys: readonly string[] = []
): T | null {
  const h = trimSlash(home);
  const rank = (s: T) => {
    const i = recentKeys.indexOf(recentKey(s));
    return i < 0 ? Number.MAX_SAFE_INTEGER : i;
  };
  let best: T | null = null;
  let bestDepth = -1;
  for (const s of sessions) {
    if ((s.machine ?? "") !== machine || !s.cwd) continue;
    const cwd = trimSlash(s.cwd);
    if (cwd === h || !isWithin(cwd, abs)) continue;
    const depth = cwd.length;
    const better =
      depth > bestDepth ||
      (depth === bestDepth &&
        best !== null &&
        (rank(s) < rank(best) || (rank(s) === rank(best) && s.name < best.name)));
    if (better) {
      best = s;
      bestDepth = depth;
    }
  }
  return best;
}
