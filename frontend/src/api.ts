// Thin client over the Go backend's four operations.

export interface Session {
  name: string;
  tool: string;
  short: string;
  attached: boolean;
  activity: number;
  last_input_at?: number;
  busy_since?: number;
  // Busy-window deadline. While working it's in the future; once ready it equals
  // the busy→ready transition instant and (unlike `activity`) is NOT bumped by a
  // cosmetic focus/resize repaint — so the ready timer/sort anchor to it for a
  // stable "ready for N" that doesn't reset on focus. 0/absent → fall back to
  // `activity` (proposal 0024).
  busy_until?: number;
  preview: string;
  // True when the session is ready / "your turn": not in an open, submit-armed
  // busy window. A user submit (Enter) arms busy; the agent's output sustains it;
  // it flips back to ready a grace window after output goes quiet — so cosmetic
  // repaints (focus/resize/spinner) never read as busy. Server-computed; see
  // engine.rs WORK_GRACE_SECS (proposal 0024).
  waiting: boolean;
  // The machine (agent) this session lives on, set by the hub when it aggregates
  // several agents. Absent/empty when talking to a single agent directly.
  machine?: string;
  // Whether the session launched in YOLO mode (approval prompts skipped).
  // Informational — drives a "YOLO" badge. `undefined` = unknown (pre-0005).
  skip_permissions?: boolean;
  // LLM-summarized status (proposal 0022). `headline` (≤6 words) replaces the
  // bare preview in dense surfaces; `detail` (2-3 sentences) is the tooltip /
  // status-view / push body. Absent until computed or when the feature is off —
  // every surface falls back to `preview`.
  headline?: string;
  detail?: string;
  // The session's live working directory (proposal 0025). The server already
  // computes and sends it (`handlers.rs` `live_cwd()` → `/proc/<pid>/cwd`,
  // falling back to the launch dir); it's omitted on the wire only when the
  // server genuinely can't read it. Drives the folder-breadcrumb label and the
  // tooltip's path row.
  cwd?: string;
}

// PaneRef is the identity the app stores for an open session: the session name
// plus the machine (agent) it lives on. We carry the machine rather than
// re-deriving it from the session name so a hub fronting several agents routes
// every request to the owning machine — and two machines with a same-named
// session never collide. `machine` is "" when talking to a single agent
// directly (no hub), which appends no query param anywhere downstream.
export interface PaneRef {
  name: string;
  machine: string;
}

// MachineInfo is one agent in the hub's roster (GET /api/machines). Used for the
// session-list grouping headers and the New-Session machine picker, so an
// offline or idle (zero-session) machine is still visible and targetable. A
// standalone agent has no such endpoint — fetchMachines() returns [] there.
export interface MachineInfo {
  machine: string;
  hostname: string;
  online: boolean;
}

// withMachine threads the owning agent onto a request URL — the single shared
// rule. The hub reads `?machine=` from the query on EVERY endpoint (even POSTs,
// which carry their data in the body), routing the relayed request to that
// agent. A non-empty machine is appended with the right separator (`?` or `&`
// depending on whether the URL already has a query); an empty machine
// (single-agent / no hub) appends nothing, so the URL is byte-identical to the
// pre-hub one and the standalone agent — which ignores the param anyway — is
// unaffected.
function withMachine(url: string, machine?: string): string {
  if (!machine) return url;
  const sep = url.includes("?") ? "&" : "?";
  return `${url}${sep}machine=${encodeURIComponent(machine)}`;
}

// ── Auth (opt-in password / API-token gate) ─────────────────────────────────
// The browser authenticates with a same-origin session cookie the server sets
// on login, so individual fetches and WebSockets need no extra headers — the
// cookie rides along automatically. We only (a) ask the server whether a gate
// is up and whether we're already in, and (b) notice when a request 401s (an
// expired cookie) so the app can drop back to the login screen.

export interface AuthStatus {
  authRequired: boolean;
  authed: boolean;
}

export async function getAuthStatus(): Promise<AuthStatus> {
  const r = await fetch("/api/auth");
  if (!r.ok) throw new Error(`auth: ${r.status}`);
  return r.json();
}

// login posts the password or API token; on success the server sets the 2-week
// session cookie. Returns true on success, false on a wrong secret.
export async function login(secret: string): Promise<boolean> {
  const r = await fetch("/api/login", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ secret }),
  });
  return r.ok;
}

export async function logout(): Promise<void> {
  await fetch("/api/logout", { method: "POST" });
}

let unauthorizedHandler: (() => void) | null = null;
// Register a callback fired when the heartbeat sees a 401 (cookie expired /
// logged out elsewhere). The app uses it to show the login screen again.
export function setUnauthorizedHandler(fn: (() => void) | null): void {
  unauthorizedHandler = fn;
}

export async function fetchSessions(): Promise<Session[]> {
  const r = await fetch("/api/sessions");
  if (r.status === 401) {
    unauthorizedHandler?.();
    throw new Error("unauthorized");
  }
  if (!r.ok) throw new Error(`sessions: ${r.status}`);
  return r.json();
}

// fetchMachines returns the hub's agent roster (id, hostname, online). Only the
// hub serves /api/machines; a standalone agent 404s, so we swallow any
// failure and return [] — the caller reads "[] ⇒ single machine, no hub", which
// keeps the UI ungrouped and machine-param-free for the common single-box case.
export async function fetchMachines(): Promise<MachineInfo[]> {
  try {
    const r = await fetch("/api/machines");
    if (!r.ok) return [];
    return await r.json();
  } catch {
    return [];
  }
}

// A session a reboot / tmux restart took down that the server recorded and can
// bring back, resuming the tool's prior conversation. (Restarting just the web
// daemon keeps sessions live, so this is empty in that case.)
export interface RestorableSession {
  session: string;
  tool: string;
  short: string;
  dir: string;
}

export async function fetchRestorable(machine?: string): Promise<RestorableSession[]> {
  const r = await fetch(withMachine("/api/sessions/restorable", machine));
  if (!r.ok) throw new Error(`restorable: ${r.status}`);
  return r.json();
}

export interface RestoreResult {
  restored: string[];
  failed?: Record<string, string>;
}

// restoreSessions recreates every restorable session, resuming each tool's
// conversation where possible (claude --continue, codex resume --last, …).
// Idempotent: already-live sessions are skipped.
export async function restoreSessions(machine?: string): Promise<RestoreResult> {
  const r = await fetch(withMachine("/api/sessions/restore", machine), { method: "POST" });
  if (!r.ok) throw new Error((await r.text()).trim() || `restore: ${r.status}`);
  return r.json();
}

// sendKey injects one named key (out-of-band; no focus needed). Names match
// the backend allow-list: up/down/left/right/enter/escape/tab/btab/space/
// backspace/home/end/pageup/pagedown/c-c/c-d/c-z/c-l/c-r.
export async function sendKey(session: string, key: string, machine?: string): Promise<void> {
  const r = await fetch(withMachine("/api/key", machine), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ session, key }),
  });
  if (!r.ok && r.status !== 204) throw new Error(`key: ${r.status}`);
}

// paste injects a (possibly multi-line) text block via bracketed paste, then
// optionally submits with Enter. This is the compose-sheet path.
export async function pasteText(
  session: string,
  text: string,
  enter: boolean,
  machine?: string
): Promise<void> {
  const r = await fetch(withMachine("/api/paste", machine), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ session, text, enter }),
  });
  if (!r.ok && r.status !== 204) throw new Error(`paste: ${r.status}`);
}

// clearHistory wipes the tmux scrollback for a session — the manual escape
// hatch for the re-render slideshow Claude Code leaves in scrollback whenever
// the pane is resized between clients of different widths (it writes to the
// normal buffer, so every redraw appends). The WS attach also auto-fires this
// on first connect when the client's reported cols differ from the pane's
// current width.
export async function clearHistory(session: string, machine?: string): Promise<void> {
  const r = await fetch(withMachine("/api/clear-history", machine), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ session }),
  });
  if (!r.ok && r.status !== 204) throw new Error(`clear-history: ${r.status}`);
}

// sendImage stages a PNG as the clipboard for a session and triggers Claude
// Code to paste it (the server sends the paste key; the xclip/wl-paste shim
// then fetches this image). Used for pasting phone screenshots.
export async function sendImage(session: string, png: Blob, machine?: string): Promise<void> {
  const r = await fetch(
    withMachine(`/api/clip?session=${encodeURIComponent(session)}`, machine),
    {
      method: "POST",
      headers: { "Content-Type": "image/png" },
      body: png,
    }
  );
  if (!r.ok && r.status !== 204) throw new Error(`clip: ${r.status}`);
}

// A favourite is a saved prompt, stored server-side (durable + shared across
// devices) under ~/.config/cc-screen/favorites.json. The client owns CRUD and
// PUTs the whole list back; the server validates and persists it.
export interface Favorite {
  id: string;
  text: string;
}

export async function fetchFavorites(): Promise<Favorite[]> {
  const r = await fetch("/api/favorites");
  if (!r.ok) throw new Error(`favorites: ${r.status}`);
  return r.json();
}

// saveFavorites replaces the whole list and returns the server's sanitised
// version (blanks/dupes dropped, over-long trimmed) for the client to adopt.
export async function saveFavorites(list: Favorite[]): Promise<Favorite[]> {
  const r = await fetch("/api/favorites", {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(list),
  });
  if (!r.ok) throw new Error(`favorites save: ${r.status}`);
  return r.json();
}

export interface Tool {
  cmd: string;
  prefix: string;
  extraDirs?: {
    max?: number;
  };
}

export interface DirEntry {
  name: string;
  path: string;
}

export interface DirsResp {
  path: string;
  home: string;
  atHome: boolean;
  parent: string;
  dirs: DirEntry[];
}

export async function fetchTools(machine?: string): Promise<Tool[]> {
  const r = await fetch(withMachine("/api/tools", machine));
  if (!r.ok) throw new Error(`tools: ${r.status}`);
  return r.json();
}

export async function fetchDirs(path?: string, machine?: string): Promise<DirsResp> {
  const q = path ? `?path=${encodeURIComponent(path)}` : "";
  const r = await fetch(withMachine(`/api/dirs${q}`, machine));
  if (!r.ok) throw new Error(`dirs: ${r.status}`);
  return r.json();
}

// One ranked hit from the recursive folder search (GET /api/dirs/search,
// proposal 0016). `rel` is the home-relative display path (~/development/foo);
// `depth` is how far below the search root it sits.
export interface DirSearchResult {
  path: string;
  name: string;
  rel: string;
  depth: number;
  score: number;
  mtime: number;
}

export interface DirsSearchResp {
  root: string;
  home: string;
  results: DirSearchResult[];
}

// searchDirs fuzzy-matches directories anywhere below `root` (default $HOME) on
// the chosen agent. Empty `q` returns no results — the caller falls back to
// fetchDirs + a recents shortcut. Per-agent like fetchDirs (the hub routes by
// ?machine=), so on a hub each agent searches its own $HOME.
export async function searchDirs(
  q: string,
  root?: string,
  machine?: string
): Promise<DirsSearchResp> {
  const params = new URLSearchParams();
  params.set("q", q);
  if (root) params.set("root", root);
  const r = await fetch(withMachine(`/api/dirs/search?${params.toString()}`, machine));
  if (!r.ok) throw new Error((await r.text()).trim() || `dirs search: ${r.status}`);
  return r.json();
}

export interface FileEntry {
  name: string;
  path: string;
  size: number;
  mtime: number; // unix seconds
}

export interface FilesResp {
  path: string;
  home: string;
  share: string;
  atHome: boolean;
  atShare: boolean;
  parent: string;
  dirs: DirEntry[];
  files: FileEntry[];
}

// fetchFiles lists subdirs + regular files under $HOME. Path resolution
// mirrors the backend:
//   - path given           => list that folder
//   - session given (no path) => list the session's tmux cwd (project root)
//   - neither              => list the share folder (CCWEB_SHARE_DIR or ~/cc-share/)
export async function fetchFiles(
  path?: string,
  session?: string,
  machine?: string
): Promise<FilesResp> {
  const params = new URLSearchParams();
  if (path) params.set("path", path);
  else if (session) params.set("session", session);
  const qs = params.toString();
  const r = await fetch(withMachine(`/api/files${qs ? `?${qs}` : ""}`, machine));
  if (!r.ok) throw new Error((await r.text()).trim() || `files: ${r.status}`);
  return r.json();
}

// downloadURL is the streaming download endpoint for a single file; the
// server attaches a Content-Disposition so the browser saves rather than
// renders.
export function downloadURL(path: string, machine?: string): string {
  return withMachine(`/api/download?path=${encodeURIComponent(path)}`, machine);
}

// inlineURL is the same file stream but served inline (Content-Disposition:
// inline) rather than as an attachment — the editor's PDF viewer points pdf.js
// at this so it can fetch + render the bytes (Range-supported) in place. See
// handleDownload's ?inline=1 branch.
export function inlineURL(path: string, machine?: string): string {
  return withMachine(`/api/download?inline=1&path=${encodeURIComponent(path)}`, machine);
}

// saveFileToDevice streams a file and hands it to navigator.share({files}) —
// the iOS PWA gold path: the system share sheet offers Save to Files, AirDrop,
// send to Photos. Falls back to a synthesised <a download> click for non-secure
// contexts (plain HTTP over tailnet) where canShare/share aren't available.
// Shared by the Files sheet and the PDF viewer's download button.
export async function saveFileToDevice(path: string, name: string, machine?: string): Promise<void> {
  const r = await fetch(downloadURL(path, machine));
  if (!r.ok) throw new Error(`download: ${r.status}`);
  const blob = await r.blob();
  const file = new File([blob], name, {
    type: blob.type || "application/octet-stream",
  });
  const nav = navigator as Navigator & {
    canShare?: (data: ShareData) => boolean;
  };
  if (nav.canShare?.({ files: [file] }) && nav.share) {
    try {
      await nav.share({ files: [file] });
      return;
    } catch (e) {
      if (e instanceof DOMException && e.name === "AbortError") return;
      // other share failures fall through to the download fallback
    }
  }
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

// --- File editor (markdown / text) ---
//
// The editor reads and writes text files under $HOME (same confinement as
// fetchFiles/downloadURL). Reads reject binaries and oversized files; writes
// are atomic server-side and use mtime to detect a concurrent change. See
// `web/server/editor.go`.

export interface FileReadResp {
  path: string;
  name: string;
  content: string;
  size: number;
  mtime: number; // unix seconds — echo back as baseMtime on save
}

export interface FileWriteResp {
  path: string;
  name: string;
  size: number;
  mtime: number;
}

// FileNotEditable is thrown by readFile when the server reports the file is
// binary (415). The editor catches this to fall back to download.
export class FileNotEditable extends Error {
  constructor() {
    super("file is not editable text");
    this.name = "FileNotEditable";
  }
}

// readFile loads a text file's contents for the editor. Throws FileNotEditable
// on a binary file (415), or a generic Error otherwise.
export async function readFile(path: string, machine?: string): Promise<FileReadResp> {
  const r = await fetch(withMachine(`/api/file/read?path=${encodeURIComponent(path)}`, machine));
  if (r.status === 415) throw new FileNotEditable();
  if (!r.ok) throw new Error((await r.text()).trim() || `read: ${r.status}`);
  return r.json();
}

// writeFile saves the editor's contents. Pass the baseMtime from the last read
// so the server can refuse (409) if the file changed on disk meanwhile; omit it
// (or pass 0) when creating a new file. A 409 throws FileChangedOnDisk.
export class FileChangedOnDisk extends Error {
  constructor() {
    super("file changed on disk");
    this.name = "FileChangedOnDisk";
  }
}

export async function writeFile(
  path: string,
  content: string,
  baseMtime?: number,
  machine?: string
): Promise<FileWriteResp> {
  const r = await fetch(withMachine("/api/file/write", machine), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ path, content, baseMtime: baseMtime ?? 0 }),
  });
  if (r.status === 409) throw new FileChangedOnDisk();
  if (!r.ok) throw new Error((await r.text()).trim() || `write: ${r.status}`);
  return r.json();
}

// deleteFile removes a single file under $HOME (the editor's "delete this
// file"). The server refuses directories — rmdir handles those.
export async function deleteFile(path: string, machine?: string): Promise<void> {
  const r = await fetch(withMachine("/api/file/delete", machine), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ path }),
  });
  if (!r.ok) throw new Error((await r.text()).trim() || `delete: ${r.status}`);
}

// makeDir creates a folder named `name` inside `dir` (both under $HOME).
export async function makeDir(dir: string, name: string, machine?: string): Promise<void> {
  const r = await fetch(withMachine("/api/mkdir", machine), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ dir, name }),
  });
  if (!r.ok) throw new Error((await r.text()).trim() || `mkdir: ${r.status}`);
}

// removeDir deletes a folder (under $HOME). By default only an empty folder is
// removed (non-empty -> error); pass recursive to delete the whole subtree
// (the file-tree context menu does, behind a confirm).
export async function removeDir(path: string, recursive = false, machine?: string): Promise<void> {
  const r = await fetch(withMachine("/api/rmdir", machine), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ path, recursive }),
  });
  if (!r.ok && r.status !== 204) throw new Error((await r.text()).trim() || `rmdir: ${r.status}`);
}

// renamePath renames a file or folder in place (same parent dir) to `name`.
// $HOME-confined server-side; refuses a path separator / leading dot, and a
// name that already exists (409). Returns the new {name, path}.
export async function renamePath(path: string, name: string, machine?: string): Promise<DirEntry> {
  const r = await fetch(withMachine("/api/rename", machine), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ path, name }),
  });
  if (!r.ok) throw new Error((await r.text()).trim() || `rename: ${r.status}`);
  return r.json();
}

// movePath relocates a file or folder INTO the directory `dest` (both under
// $HOME). Unlike renamePath (same-parent only), this is a cross-directory move.
// $HOME-confined + symlink-safe server-side; rejects a name collision at the
// destination (409) and moving a folder into itself/a descendant (400). Returns
// the new {name, path}.
export async function movePath(path: string, dest: string, machine?: string): Promise<DirEntry> {
  const r = await fetch(withMachine("/api/move", machine), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ path, dest }),
  });
  if (!r.ok) throw new Error((await r.text()).trim() || `move: ${r.status}`);
  return r.json();
}

// createSession launches a new cc-screen session (tool = cmd or prefix) in dir,
// named <prefix>-<name>. Returns the full session name, or throws with a
// message ("already exists" on 409) the UI can show.
export async function createSession(
  tool: string,
  name: string,
  dir: string,
  extraDirs: string[] = [],
  machine?: string,
  // Per-session launch policy (0005). Defaults to the agent's serde default so
  // omitting it reproduces today's behavior: YOLO on. (0014 retired the
  // hub-control switch — every session is editable through the hub.)
  skipPermissions = true
): Promise<PaneRef> {
  const r = await fetch(withMachine("/api/session", machine), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ tool, name, dir, extraDirs, skipPermissions }),
  });
  if (!r.ok) {
    const msg = (await r.text()).trim();
    throw new Error(msg || `session: ${r.status}`);
  }
  const { name: session } = await r.json();
  // Return the owning machine alongside the name so the caller can mount the
  // pane with its full identity (machine is "" for a single agent / no hub).
  return { name: session, machine: machine ?? "" };
}

// deleteSession ends a session. "exit" injects the agent's /exit (graceful;
// the session dies asynchronously when the agent quits); "kill" tears it down
// immediately. Callers poll fetchSessions until the session is gone.
export async function deleteSession(
  session: string,
  mode: "exit" | "kill",
  machine?: string
): Promise<void> {
  const r = await fetch(withMachine("/api/session/delete", machine), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ session, mode }),
  });
  if (!r.ok && r.status !== 202 && r.status !== 204) {
    throw new Error((await r.text()).trim() || `delete: ${r.status}`);
  }
}

// wsURL builds the terminal WebSocket URL for a session, honouring the page's
// scheme (wss under tailscale serve / https). When talking to a hub, pass the
// session's `machine` so the hub routes to the owning agent; omitted/empty for a
// single agent (unchanged URL).
export function wsURL(session: string, machine?: string): string {
  const proto = location.protocol === "https:" ? "wss" : "ws";
  let url = `${proto}://${location.host}/api/ws?session=${encodeURIComponent(session)}`;
  if (machine) {
    url += `&machine=${encodeURIComponent(machine)}`;
  }
  return url;
}

// watchURL builds the filesystem-watch WebSocket URL (real-time tree + open-file
// updates), same scheme rule as wsURL.
export function watchURL(machine?: string): string {
  const proto = location.protocol === "https:" ? "wss" : "ws";
  return withMachine(`${proto}://${location.host}/api/watch`, machine);
}

// --- Drag-and-drop upload ---
//
// Drop files (and folders, via webkitGetAsEntry) onto a terminal pane; the
// UploadSheet then picks a destination inside the session's project root and
// streams everything through these endpoints. See `web/server/upload.go` for
// the backend and `AGENTS.md` for the moving-parts overview.

// sessionRoot returns the project root (tmux #{pane_current_path}) for a
// session. The destination picker in UploadSheet uses this to anchor and
// constrain its dir browser; the server enforces the same constraint
// on the upload itself, so a tampered client can't escape.
export async function sessionRoot(
  session: string,
  machine?: string
): Promise<{ root: string; home: string }> {
  const r = await fetch(
    withMachine(`/api/session/root?session=${encodeURIComponent(session)}`, machine)
  );
  if (!r.ok) throw new Error((await r.text()).trim() || `session root: ${r.status}`);
  return r.json();
}

// checkUpload asks the server which of `names` already exist in `dir` so the
// sheet can prompt for collision resolution up front. Names are relpaths
// (e.g. "src/foo.png"), matching what the upload itself will send. Used by the
// terminal-pane UploadSheet, which always has a session (destination confined
// to that session's project root). The editor file-tree drop uploads directly
// with no preflight, so it doesn't call this.
export async function checkUpload(
  session: string,
  dir: string,
  names: string[],
  machine?: string
): Promise<{ exists: string[] }> {
  const r = await fetch(withMachine("/api/upload/check", machine), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ session, dir, names }),
  });
  if (!r.ok) throw new Error((await r.text()).trim() || `upload check: ${r.status}`);
  return r.json();
}

// UploadFile is one entry from a drop, paired with its path relative to the
// drop root (so a dropped folder preserves its tree on the server).
export interface UploadFile {
  relPath: string;
  file: File;
}

// UploadMode is the per-file collision-resolution choice. "skip" is handled
// client-side (the file is omitted from the POST entirely); the server only
// ever sees "overwrite" / "rename".
export type UploadMode = "overwrite" | "rename" | "skip";

export interface UploadResult {
  written: string[];                 // relpaths actually written (renamed names if renamed)
  renamed: Record<string, string>;   // orig -> new (mode=rename + collision)
  errors?: Record<string, string>;   // per-file failure messages (rare)
}

// uploadFiles POSTs a multipart body to /api/upload. `modes` is the
// per-file mode map (defaults server-side to "rename" if missing).
// Files with mode "skip" are dropped before sending.
//
// `session` is the terminal-pane path (destination confined to that session's
// project root); null is the editor file-tree path (confined to $HOME). See
// uploadRoot in upload.go for the server-side confinement.
//
// Uses XMLHttpRequest (not fetch) for the `progress` event — fetch's
// streaming uploads (ReadableStream body) require HTTP/2 + secure context,
// which we don't have on plain HTTP. XHR gives us total-bytes progress on
// every platform. `onProgress` receives a 0..1 fraction.
export function uploadFiles(
  session: string | null,
  dir: string,
  files: UploadFile[],
  modes: Record<string, UploadMode>,
  onProgress?: (frac: number) => void,
  machine?: string
): Promise<UploadResult> {
  const fd = new FormData();
  // Manifest first so the server sees per-file modes before any file part.
  const manifest = {
    items: files
      .filter((f) => modes[f.relPath] !== "skip")
      .map((f) => ({
        name: f.relPath,
        mode: (modes[f.relPath] ?? "rename") as Exclude<UploadMode, "skip">,
      })),
  };
  fd.append("manifest", JSON.stringify(manifest));
  for (const f of files) {
    if (modes[f.relPath] === "skip") continue;
    // The 3rd arg becomes the multipart part's `filename` — crucial: we put
    // the full relpath here so a folder drop preserves its tree. (Go's
    // mime/multipart.Part.FileName() would normally strip subpaths via
    // filepath.Base; the server parses Content-Disposition manually to keep
    // them. See web/server/upload.go partFilename.)
    fd.append("file", f.file, f.relPath);
  }
  return new Promise<UploadResult>((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open(
      "POST",
      withMachine(
        `/api/upload?session=${encodeURIComponent(session ?? "")}&dir=${encodeURIComponent(dir)}`,
        machine
      )
    );
    xhr.responseType = "json";
    xhr.upload.onprogress = (ev) => {
      if (onProgress && ev.lengthComputable) onProgress(ev.loaded / ev.total);
    };
    xhr.onerror = () => reject(new Error("network error"));
    xhr.onabort = () => reject(new Error("upload aborted"));
    xhr.onload = () => {
      if (xhr.status >= 200 && xhr.status < 300) {
        resolve(xhr.response as UploadResult);
      } else {
        const msg =
          (typeof xhr.response === "string" && xhr.response) ||
          xhr.statusText ||
          `upload: ${xhr.status}`;
        reject(new Error(msg.trim()));
      }
    };
    xhr.send(fd);
  });
}

// flattenDataTransfer walks a drop's items, expanding folders into a flat
// list of {relPath, file}. Uses the venerable webkitGetAsEntry API which
// every shipping browser supports (Chrome/Edge/Safari/Firefox); the newer
// getAsFileSystemHandle is secure-context-only and would break our plain-
// HTTP tailnet deployment. Items that aren't files/dirs (URLs, plain text
// from another tab) are ignored — we want OS file drops only.
export async function flattenDataTransfer(dt: DataTransfer): Promise<UploadFile[]> {
  const out: UploadFile[] = [];
  // Snapshot items before any async hop — `dt` is invalidated as soon as
  // the drop handler returns, and items[] is a live list.
  const items = Array.from(dt.items);
  const entries: FsEntry[] = [];
  for (const it of items) {
    // webkitGetAsEntry is typed as returning `FileSystemEntry | null` in
    // the DOM lib, but we use our own narrower type below so the recursion
    // is typed without `any`.
    const entry = (it as DataTransferItem & {
      webkitGetAsEntry?: () => FsEntry | null;
    }).webkitGetAsEntry?.();
    if (entry) {
      entries.push(entry);
    } else if (it.kind === "file") {
      // No entry support — flat-file fallback (very old browsers).
      const f = it.getAsFile();
      if (f) out.push({ relPath: f.name, file: f });
    }
  }
  await Promise.all(entries.map((e) => walkEntry(e, "", out)));
  return out;
}

// Minimal FileSystemEntry interface for the recursion — keeps the file
// walk free of `any` while staying compatible with the legacy
// (webkit-prefixed) API the entries actually implement.
interface FsEntry {
  name: string;
  isFile: boolean;
  isDirectory: boolean;
  file?: (resolve: (f: File) => void, reject?: (e: unknown) => void) => void;
  createReader?: () => {
    readEntries: (
      resolve: (es: FsEntry[]) => void,
      reject?: (e: unknown) => void
    ) => void;
  };
}

function walkEntry(entry: FsEntry, prefix: string, out: UploadFile[]): Promise<void> {
  return new Promise((resolve) => {
    if (entry.isFile && entry.file) {
      entry.file(
        (f) => {
          out.push({ relPath: prefix + entry.name, file: f });
          resolve();
        },
        () => resolve()
      );
      return;
    }
    if (entry.isDirectory && entry.createReader) {
      const reader = entry.createReader();
      const collected: FsEntry[] = [];
      // readEntries returns ~100 entries per call; keep calling until empty.
      const readBatch = () => {
        reader.readEntries(
          (batch) => {
            if (batch.length === 0) {
              Promise.all(
                collected.map((e) => walkEntry(e, prefix + entry.name + "/", out))
              ).then(() => resolve());
              return;
            }
            collected.push(...batch);
            readBatch();
          },
          () => resolve()
        );
      };
      readBatch();
      return;
    }
    resolve();
  });
}
