// Proposal 0083 — a URL for a file.
//
// The app has never had URLs: opening a file never touched the address bar, so
// nothing about the file browser was bookmarkable. This module is the whole URL
// grammar, kept away from App.tsx so it can be unit-tested without a DOM:
//
//   /file/<machine>/<home-relative-path>     a file, opened in the editor
//   /file/<machine>/<home-relative-path>/    a FOLDER, opened in the tree
//   /file/-/<home-relative-path>             the default/only machine
//   /s/<token>                               a read-only link grant (Part C)
//
// Two rules are load-bearing:
//
//   * A URL identifies a file as **(machine, home-relative path)**, never a
//     bare path. A path is only meaningful on the machine whose tree produced
//     it ([0044]'s no-cross-machine-rewriting rule), and session identity
//     already learned this lesson the hard way ([0078]: `?session=` carries no
//     machine and is the corpus's named anti-pattern).
//   * The path is **home-relative**. Everything browsable lives under the
//     agent's `$HOME` (`src/confine.rs`), it is what the tree already shows the
//     user (`rel()` in dirTree.tsx), and `home` comes back on every
//     `GET /api/files` response — so relative is both fully general and the
//     form that survives a different `$HOME` on the other end.
//
// The trailing slash is the file/folder discriminator, so consuming a link
// needs no stat round-trip. A hand-typed URL that gets it wrong just lands in
// the healing path (App.tsx) and shows the tree.

export interface FileLink {
  /** The user-facing machine label; "" for the default/only machine (`-`). */
  machine: string;
  /** Home-relative, `/`-separated, already percent-decoded. "" = the home root. */
  relPath: string;
  /** True when the URL ended in `/` — open the tree at this folder. */
  isDir: boolean;
}

/**
 * What the open editor is pointing at, reported up to App so the address bar
 * can mirror it (Part B). `path` wins when a file is open; `dir` is the folder
 * form the tree is showing (a folder deep link, or the parent a phone stepped
 * back to); `home` is the browse machine's `$HOME`, without which no URL can be
 * built at all.
 */
export interface EditorLocation {
  path: string | null;
  dir: string | null;
  machine: string;
  home: string;
}

/** The machine segment for a link. `-` is the wire form of "no machine". */
const machineSeg = (machine: string) => (machine ? encodeURIComponent(machine) : "-");

/**
 * Parse a pathname into a file deep link, or null when it isn't one.
 *
 * Deliberately total: every malformed shape returns null rather than throwing,
 * because this runs at module scope on whatever the browser happened to load.
 * A percent-sequence that isn't valid UTF-8 (`decodeURIComponent` throws) is one
 * of those shapes.
 */
export function parseFileLink(pathname: string): FileLink | null {
  if (!pathname.startsWith("/file/")) return null;
  const rest = pathname.slice("/file/".length);
  if (!rest) return null;
  const isDir = rest.endsWith("/");
  const segs = rest.split("/").filter((s) => s.length > 0);
  if (segs.length === 0) return null;
  try {
    const machineRaw = decodeURIComponent(segs[0]);
    const machine = machineRaw === "-" ? "" : machineRaw;
    const parts = segs.slice(1).map(decodeURIComponent);
    // `.` / `..` are meaningless here and would be traversal if a caller ever
    // joined them onto $HOME without cleaning. Refuse rather than sanitise: a
    // link that means nothing should heal to the tree, not to a guess.
    if (parts.some((p) => p === "." || p === "..")) return null;
    // A bare `/file/<machine>` (no trailing slash, no path) still means "that
    // machine's home" — treat it as the folder form.
    return { machine, relPath: parts.join("/"), isDir: isDir || parts.length === 0 };
  } catch {
    return null;
  }
}

/** Build the canonical pathname for a (machine, home-relative path) pair. */
export function fileLinkPath(machine: string, relPath: string, isDir: boolean): string {
  const parts = relPath.split("/").filter((s) => s.length > 0);
  const encoded = parts.map(encodeURIComponent).join("/");
  const tail = encoded ? `/${encoded}${isDir ? "/" : ""}` : "/";
  return `/file/${machineSeg(machine)}${tail}`;
}

/** The absolute URL a *Copy link* action puts on the clipboard. */
export function fileLinkUrl(origin: string, machine: string, relPath: string, isDir: boolean): string {
  return `${origin.replace(/\/+$/, "")}${fileLinkPath(machine, relPath, isDir)}`;
}

/**
 * `abs` expressed relative to `home`, or null when it isn't under it. Null is
 * the honest answer for a path outside `$HOME` — there is no URL for it, and
 * inventing one would produce a link that 404s on the other end.
 */
export function relFromHome(home: string, abs: string): string | null {
  if (!home || !abs) return null;
  if (abs === home) return "";
  const root = home.endsWith("/") ? home : home + "/";
  if (!abs.startsWith(root)) return null;
  return abs.slice(root.length).replace(/\/+$/, "");
}

/** The absolute path on the agent for a home-relative link path. */
export function joinHome(home: string, relPath: string): string {
  const root = home.replace(/\/+$/, "");
  return relPath ? `${root}/${relPath}` : root;
}

/**
 * Parse `/s/<token>` — the read-only link grant page (Part C).
 *
 * The token alphabet is `generate_token`'s base64url-no-pad over 32 bytes, so a
 * shape check here is free and keeps an obviously-bogus URL from ever becoming
 * a request. The server re-checks; this is not the boundary.
 */
export function parseLinkToken(pathname: string): string | null {
  if (!pathname.startsWith("/s/")) return null;
  const token = pathname.slice("/s/".length).replace(/\/+$/, "");
  if (!token || !/^[A-Za-z0-9_-]{16,128}$/.test(token)) return null;
  return token;
}
