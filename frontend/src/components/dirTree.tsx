import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type DragEvent as ReactDragEvent,
  type MouseEvent as ReactMouseEvent,
  type TouchEvent as ReactTouchEvent,
  type ReactNode,
} from "react";
import { fetchFiles, type DirEntry, type FileEntry, type FilesResp } from "../api";
import { useFileWatch } from "./useFileWatch";
import { readViewerState, writeViewerState, viewerKey } from "./viewerState";
import { fuzzyScore, fuzzyMatchPositions, useDebouncedValue } from "../util";

// Shared $HOME directory-tree machinery, used by both the phone Files sheet
// (download browser) and the desktop editor's file tree. The data layer (cache,
// lazy per-folder fetch, expand state, the share/project/home sections) lives in
// the useDirTree hook; FolderChildren renders one folder's contents and recurses
// into expanded subfolders. Consumers supply what a file tap does (download vs
// open-in-editor) via onFile + an optional trailing accessory.

// One folder's contents, cached after a successful /api/files fetch.
export interface DirContents {
  path: string;
  dirs: DirEntry[];
  files: FileEntry[];
}

export function fmtSize(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

export function fmtAge(unix: number): string {
  const s = Math.max(0, Date.now() / 1000 - unix);
  if (s < 60) return `${Math.round(s)}s`;
  if (s < 3600) return `${Math.round(s / 60)}m`;
  if (s < 86400) return `${Math.round(s / 3600)}h`;
  return `${Math.round(s / 86400)}d`;
}

export function errMsg(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

// Extensions the editor opens on tap. Markdown is the headline use; plain text
// and common config/code files come along for the ride (the editor highlights
// code via CodeMirror's language-data). Anything else (images, archives,
// binaries) keeps the download/share behaviour. Extensionless files are treated
// as text (READMEs, LICENSE, Dockerfile, …) — the server's binary guard still
// catches anything that isn't actually text.
const EDITABLE_EXTS = new Set([
  "md", "markdown", "mdx", "txt", "text", "rst", "org", "log",
  "json", "jsonc", "json5", "yaml", "yml", "toml", "ini", "conf", "cfg", "env",
  "csv", "tsv", "xml", "svg", "html", "htm", "css", "scss",
  "js", "jsx", "ts", "tsx", "mjs", "cjs", "go", "rs", "py", "rb", "php",
  "java", "kt", "c", "h", "cc", "cpp", "hpp", "cs", "swift", "sh", "bash",
  "zsh", "fish", "sql", "lua", "vim", "dockerfile", "makefile", "gitignore",
]);

export function isEditableFile(name: string): boolean {
  const lower = name.toLowerCase();
  const dot = lower.lastIndexOf(".");
  if (dot <= 0) {
    // No extension (.env, .gitignore, README, LICENSE, Makefile, …): treat as
    // text. dot === 0 catches dotfiles whose whole name is the "extension".
    return true;
  }
  return EDITABLE_EXTS.has(lower.slice(dot + 1));
}

// isMarkdownFile decides whether to use the live-preview markdown rendering.
export function isMarkdownFile(name: string): boolean {
  const lower = name.toLowerCase();
  return lower.endsWith(".md") || lower.endsWith(".markdown") || lower.endsWith(".mdx");
}

// isPdfFile decides whether the editor opens a file in the read-only PDF viewer
// (pdf.js) instead of the text editor. PDFs are binary, so they never go through
// the text read/write path — see EditorOverlay's "pdf" status.
export function isPdfFile(name: string): boolean {
  return name.toLowerCase().endsWith(".pdf");
}

// canOpenInEditor is the routing predicate the file trees use to decide whether
// a tap opens the editor overlay (text editor or PDF viewer) vs. falling through
// to download. It's the union of the editable-text set and PDFs.
export function canOpenInEditor(name: string): boolean {
  return isEditableFile(name) || isPdfFile(name);
}

export interface TreeSection {
  key: string;
  label: string;
  sub: string;
  icon: string;
  path: string;
  bySession?: string;
}

// Per-consumer tuning for the shared tree. The one shipped caller — the editor
// (EditorTree, via EditorOverlay) — is project-first (opens rooted at the
// session's working tree, bare vault-style root name). The defaults below
// (share-first, auto-open share) are the original standalone-Files-sheet
// behaviour, kept for any caller that passes no opts.
export interface DirTreeOpts {
  // Top-to-bottom section order by key. Default: ["share","project","home"].
  order?: string[];
  // Which section auto-expands on open. "share" opens the dropbox (default);
  // "project" lands in the session's working tree; "none" opens nothing.
  autoExpand?: "share" | "project" | "none";
  // Drop the "Project: " prefix so the project root reads as a bare name.
  bareProjectLabel?: boolean;
}

// Map an /api/watch frame's `dir` to the cache key whose folder we should
// re-list, or null when we aren't showing that folder. The agent emits the
// lexical parent path (watch.rs route()); the tree caches by the listing's
// resp.path (files.rs). They normally match exactly, but a trailing-slash
// mismatch on one side would silently fail the cache lookup and DROP the change
// frame — while the already-cached siblings keep rendering, which reads as
// "new files never appear" (proposal 0034). Try the path as-is, then with a
// trailing slash trimmed, so a "…/foo/" frame still matches a "…/foo" key.
export function resolveWatchDir(dir: string, has: (key: string) => boolean): string | null {
  if (has(dir)) return dir;
  const trimmed = dir.length > 1 && dir.endsWith("/") ? dir.slice(0, -1) : dir;
  if (trimmed !== dir && has(trimmed)) return trimmed;
  return null;
}

// useDirTree owns the cache/expand/loading state and the lazy fetches. It
// bootstraps on `open` (a /api/files listing gives share + home; one section
// auto-expands per `opts.autoExpand`), and exposes `toggle` for both section
// headers and nested folders. `sections` derives the share / project / home
// roots in `opts.order`. Pass a stable (module-level) opts object — `order`
// is a memo dependency, so a fresh array each render would thrash it.
export function useDirTree(
  open: boolean,
  currentSession: string | null,
  opts?: DirTreeOpts,
  // The machine (agent) whose $HOME this tree browses. All listings/watch route
  // there. "" for a single agent. Changing it re-roots the tree via the
  // bootstrap effect (keyed on machine).
  machine = ""
) {
  const order = opts?.order;
  const autoExpand = opts?.autoExpand ?? "share";
  const bareProjectLabel = opts?.bareProjectLabel ?? false;
  const [cache, setCache] = useState<Map<string, DirContents>>(new Map());
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [loading, setLoading] = useState<Set<string>>(new Set());
  const [sharePath, setSharePath] = useState("");
  const [homePath, setHomePath] = useState("");
  const [projectPath, setProjectPath] = useState("");
  const [errs, setErrs] = useState<Record<string, string>>({});

  // ── Type-to-filter the tree in place (proposal 0038, Part C) ──────────────
  // A raw input the consumer drives; the debounced value (shared 120ms settle)
  // gates the filtered view at ≥3 chars. The narrowing is computed as an OVERLAY
  // (filter.expandDirs) over the persisted expansion, never mutating it — so
  // clearing the filter restores the exact prior tree (filterActive flips false
  // → effectiveExpanded falls back to `expanded`). Distinct from [0027]'s flat
  // jump-to-file list: this keeps the hierarchy and only sees already-loaded
  // nodes (the lazy-load caveat; the consumer offers a "Search all files →"
  // handoff to [0027] for the project-wide answer).
  const [filterQuery, setFilterQuery] = useState("");
  const debouncedFilter = useDebouncedValue(filterQuery.trim(), 120);
  const filterActive = debouncedFilter.length >= 3;

  // Per-session expansion memory (proposal 0019 follow-up). `expandedRef` mirrors
  // the live `expanded` set so the save points (switch-out, close) read the
  // freshest value without depending on it; `prevKeyRef` is the machine+session
  // entry the current expansion belongs to, so a switch saves it under the OLD
  // key before re-anchoring under the new one. See viewerState.ts.
  const expandedRef = useRef<Set<string>>(expanded);
  expandedRef.current = expanded;
  const prevKeyRef = useRef<string | null>(null);

  // Real-time filesystem watch (shared with the editor's open-file watcher via
  // the returned `watch` handle). Active only while the tree is `open`.
  const watch = useFileWatch(open, machine);

  const merge = useCallback((resp: FilesResp) => {
    setCache((m) => {
      const n = new Map(m);
      n.set(resp.path, { path: resp.path, dirs: resp.dirs, files: resp.files });
      return n;
    });
    setSharePath(resp.share || "");
    setHomePath(resp.home || "");
  }, []);

  const loadByPath = useCallback(
    async (path: string): Promise<FilesResp> => {
      setLoading((s) => new Set(s).add(path));
      try {
        const resp = await fetchFiles(path, undefined, machine);
        merge(resp);
        return resp;
      } finally {
        setLoading((s) => {
          const n = new Set(s);
          n.delete(path);
          return n;
        });
      }
    },
    [merge, machine]
  );

  const loadBySession = useCallback(
    async (session: string): Promise<FilesResp> => {
      const k = `session:${session}`;
      setLoading((s) => new Set(s).add(k));
      try {
        const resp = await fetchFiles(undefined, session, machine);
        merge(resp);
        return resp;
      } finally {
        setLoading((s) => {
          const n = new Set(s);
          n.delete(k);
          return n;
        });
      }
    },
    [merge, machine]
  );

  // Re-fetch a folder already in the cache (e.g. after creating a file in it),
  // keeping it expanded.
  const refresh = useCallback(
    async (path: string) => {
      try {
        await loadByPath(path);
      } catch (e) {
        setErrs((p) => ({ ...p, refresh: errMsg(e) }));
      }
    },
    [loadByPath]
  );

  // Ensure `path` is expanded AND its contents loaded/refreshed — unlike
  // `toggle`, which flips. Used after a drop-upload so the destination folder
  // opens to reveal the newly-written files. Only the target itself needs
  // opening: its ancestors are already expanded (a folder row is droppable only
  // when its parent is open), and loadByPath both seeds a never-opened folder
  // and refreshes one already in the cache.
  const expand = useCallback(
    async (path: string) => {
      if (!path) return;
      setExpanded((s) => (s.has(path) ? s : new Set(s).add(path)));
      try {
        await loadByPath(path);
      } catch (e) {
        setErrs((p) => ({ ...p, expand: errMsg(e) }));
      }
    },
    [loadByPath]
  );

  useEffect(() => {
    if (!open) return;
    // Save the OUTGOING session's expansion before we wipe + re-anchor, so a
    // switch (or a browse-machine change) remembers what was open. Read the live
    // set off the ref — `expanded` in this closure is still the previous render's
    // value, which is exactly the outgoing state we want to keep.
    const curKey = viewerKey(machine, currentSession);
    if (prevKeyRef.current && prevKeyRef.current !== curKey) {
      writeViewerState(prevKeyRef.current, { expanded: [...expandedRef.current] });
    }
    prevKeyRef.current = curKey;
    // Restore this session's remembered open folders (best-effort: a folder that
    // no longer exists just renders nothing). Captured before the wipe so the
    // async loads below can fan it back out.
    const remembered = currentSession ? readViewerState(curKey)?.expanded ?? [] : [];
    // Re-fetch each remembered folder (except the already-loaded root) so its and
    // its children's rows repaint. Swallow per-folder failures — a folder that's
    // since been deleted just stays empty rather than rejecting.
    const loadRemembered = (rootPath: string) => {
      for (const p of remembered) {
        if (p && p !== rootPath) void loadByPath(p).catch(() => {});
      }
    };

    setCache(new Map());
    setExpanded(new Set());
    setLoading(new Set());
    setErrs({});
    setProjectPath("");
    setSharePath("");
    setHomePath("");
    // Project-first (editor): load the session's cwd — that listing also
    // carries share + home for the other section headers, so one fetch seeds
    // everything — and open the project plus whatever was remembered. Falls
    // through to the base listing when there's no session to anchor on.
    if (autoExpand === "project" && currentSession) {
      loadBySession(currentSession)
        .then((r) => {
          setProjectPath(r.path);
          setExpanded(new Set([r.path, ...remembered]));
          loadRemembered(r.path);
        })
        .catch((e) => setErrs((p) => ({ ...p, project: errMsg(e) })));
      return;
    }
    loadByPath("")
      .then((r) => {
        const base = autoExpand === "share" ? [r.path] : [];
        setExpanded(new Set([...base, ...remembered]));
        loadRemembered(r.path);
      })
      .catch((e) => setErrs((p) => ({ ...p, share: errMsg(e) })));
  }, [open, autoExpand, currentSession, machine, loadByPath, loadBySession]);

  // Persist the expansion so it survives a reopen + reload (the switch-out save
  // above only fires on a session change). A graceful close runs the cleanup;
  // a hard reload / tab-close does NOT run React cleanups, so also flush on
  // `pagehide` (covers reload/navigation, incl. mobile) and `visibilitychange`.
  useEffect(() => {
    if (!open) return;
    const flush = () => {
      if (prevKeyRef.current) {
        writeViewerState(prevKeyRef.current, { expanded: [...expandedRef.current] });
      }
    };
    const onVis = () => { if (document.visibilityState === "hidden") flush(); };
    window.addEventListener("pagehide", flush);
    document.addEventListener("visibilitychange", onVis);
    return () => {
      flush();
      window.removeEventListener("pagehide", flush);
      document.removeEventListener("visibilitychange", onVis);
    };
  }, [open]);

  // Keep the live filesystem watch tracking exactly the expanded folders: watch
  // what's on screen, nothing more. Diff each `expanded` change against the
  // previous set and subscribe/unsubscribe the delta (ref-counted in the hook).
  const prevExpanded = useRef<Set<string>>(new Set());
  useEffect(() => {
    const prev = prevExpanded.current;
    for (const p of expanded) if (p && !prev.has(p)) watch.subscribe(p);
    for (const p of prev) if (p && !expanded.has(p)) watch.unsubscribe(p);
    prevExpanded.current = new Set(expanded);
  }, [expanded, watch]);

  // When a watched folder changes on disk (the agent created/renamed/deleted a
  // file in it), re-fetch its listing so the tree reflects it live. A re-list
  // returns the folder's full truth, so creates, deletes, and renames all
  // surface uniformly — the create half is no longer the weak one (0034).
  // Registered once; reads cache/refresh through refs so it never re-binds.
  //
  // We only refresh a folder we're already showing (it's in the cache). A create
  // in a folder the user has never expanded is a documented non-goal: it isn't
  // watched, and it appears the moment the user expands that folder (toggle →
  // loadByPath). resolveWatchDir hardens the cache lookup against a trailing-
  // slash path-shape mismatch that would otherwise drop the frame silently.
  const cacheRef = useRef(cache);
  cacheRef.current = cache;
  const refreshRef = useRef(refresh);
  refreshRef.current = refresh;
  useEffect(
    () =>
      watch.addListener((dir) => {
        const key = resolveWatchDir(dir, (k) => cacheRef.current.has(k));
        if (key) void refreshRef.current(key);
      }),
    [watch]
  );

  const toggle = useCallback(
    async (path: string, opts?: { sectionErrKey?: string; bySession?: string }) => {
      if (expanded.has(path) && path) {
        setExpanded((s) => {
          const n = new Set(s);
          n.delete(path);
          return n;
        });
        return;
      }
      if (path && cache.has(path)) {
        setExpanded((s) => new Set(s).add(path));
        return;
      }
      try {
        const resp = opts?.bySession
          ? await loadBySession(opts.bySession)
          : await loadByPath(path);
        setExpanded((s) => new Set(s).add(resp.path));
        if (opts?.bySession) setProjectPath(resp.path);
        if (opts?.sectionErrKey) {
          setErrs((p) => {
            const { [opts.sectionErrKey!]: _drop, ...rest } = p;
            return rest;
          });
        }
      } catch (e) {
        if (opts?.sectionErrKey) setErrs((p) => ({ ...p, [opts.sectionErrKey!]: errMsg(e) }));
      }
    },
    [expanded, cache, loadByPath, loadBySession]
  );

  const rel = useCallback(
    (p: string) => {
      if (!homePath) return p;
      if (p === homePath) return "~";
      if (p.startsWith(homePath + "/")) return "~" + p.slice(homePath.length);
      return p;
    },
    [homePath]
  );

  const sections = useMemo<TreeSection[]>(() => {
    const byKey: Record<string, TreeSection> = {};
    if (sharePath) {
      byKey.share = { key: "share", label: "Share folder", sub: rel(sharePath), icon: "⇱", path: sharePath };
    }
    if (currentSession) {
      const friendly = currentSession.replace(/^[^-]+-/, "");
      byKey.project = {
        key: "project",
        label: bareProjectLabel ? friendly : `Project: ${friendly}`,
        sub: projectPath ? rel(projectPath) : "session cwd",
        icon: "●",
        path: projectPath,
        bySession: currentSession,
      };
    }
    if (homePath) {
      byKey.home = { key: "home", label: "Home", sub: "~", icon: "🏠", path: homePath };
    }
    const ordering = order ?? ["share", "project", "home"];
    return ordering.map((k) => byKey[k]).filter(Boolean);
  }, [sharePath, homePath, currentSession, projectPath, rel, order, bareProjectLabel]);

  // Drop the filter whenever the tree re-roots (session/machine switch) or the
  // viewer closes, so a stale query never hides a freshly-loaded tree.
  useEffect(() => {
    setFilterQuery("");
  }, [currentSession, machine, open]);

  // The narrowed view: which dirs/files to show, which ancestors to auto-expand,
  // and a flat render-order row list (for the keyboard cursor + the "shown"
  // count). Operates over the LOADED cache only (lazy-load caveat). Null when
  // the filter is inactive (<3 chars) — the tree then renders normally.
  const filter = useMemo(
    () => (filterActive ? buildTreeFilter(cache, sections, debouncedFilter) : null),
    [filterActive, debouncedFilter, cache, sections]
  );

  // The expansion the tree should RENDER with: while filtering, force-open the
  // ancestors of matches over the persisted set (an overlay, so clearing the
  // filter restores the original). Otherwise the persisted set as-is.
  const effectiveExpanded = useMemo(
    () => (filter ? new Set<string>([...expanded, ...filter.expandDirs]) : expanded),
    [filter, expanded]
  );

  return {
    cache,
    expanded,
    // The expansion to render with (filter overlay folded in). Use this for
    // open-state; `expanded` stays the persisted truth (watch subscriptions).
    effectiveExpanded,
    loading,
    sharePath,
    homePath,
    projectPath,
    errs,
    setErrs,
    toggle,
    refresh,
    expand,
    rel,
    sections,
    watch,
    // Type-to-filter (0038, Part C): the raw input + its narrowed view (null
    // when inactive). The consumer renders the filter field and reads `filter`.
    filterQuery,
    setFilterQuery,
    filterActive,
    filter,
  };
}

// What a tree row reports when right-clicked / long-pressed. The editor maps it
// to menu actions (new/rename/delete/download/open).
export interface TreeCtxInfo {
  path: string;
  name: string;
  isDir: boolean;
}
// Minimal event shape shared by a real MouseEvent and the synthetic object a
// long-press builds — both carry coordinates + preventDefault.
type CtxEvt = { clientX: number; clientY: number; preventDefault: () => void };

export interface FolderChildrenProps {
  path: string;
  depth: number;
  cache: Map<string, DirContents>;
  expanded: Set<string>;
  loading: Set<string>;
  onToggle: (path: string) => void;
  onFile: (f: FileEntry) => void;
  // When set, each file row carries a download button (saves the file to the
  // device). It's a separate control from the row's open tap, so any file —
  // including ones you can't open in the editor — can be downloaded.
  onDownload?: (f: FileEntry) => void;
  // The file path currently being downloaded (its button shows a spinner).
  downloadingPath?: string | null;
  // Right-click (desktop) / long-press (touch) on a row → open a context menu.
  // The consumer (the editor) maps the reported target to CRUD actions.
  onContextMenu?: (e: CtxEvt, info: TreeCtxInfo) => void;
  // When set, every folder row becomes an OS-file drop target: dragging files
  // onto it highlights the row and dropping forwards (folderPath, dataTransfer)
  // to the consumer, which uploads into that folder. Desktop only in practice
  // (phones can't drag-and-drop), but harmless to wire on touch.
  onDropFiles?: (dir: string, dt: DataTransfer) => void;
  // When set, tree nodes become draggable and folder rows / listings become
  // move drop targets: dragging a node onto a folder calls onMoveNode(src,
  // destDir) to relocate it there (proposal 0012). Desktop only in practice
  // (HTML5 DnD doesn't fire on touch — phones use the context-menu "Move to…").
  onMoveNode?: (src: string, destDir: string) => void;
  // Whether file rows are disabled (e.g. a download in flight).
  fileDisabled?: boolean;
  // Highlight the row for this file path (the open file in the editor).
  activePath?: string | null;
  // Compact = the editor's tight, icon-light tree (Notion/Obsidian feel): no
  // row borders, no size/age columns, smaller type, chevron-only folders. Off =
  // the phone Files sheet's roomier download browser (emoji + size + age).
  compact?: boolean;
  // Touch = a phone-sized variant of the compact tree: same chevron-only look,
  // but larger type and taller rows so it's finger-friendly and matches the
  // Files sheet's size. Only meaningful together with compact (the editor's
  // in-overlay tree on a phone); ignored by the already-roomy non-compact tree.
  touch?: boolean;
  // Type-to-filter view (proposal 0038, Part C). When set, rows not in the
  // visible set are hidden and the matched substring in each shown name is
  // highlighted. Null/undefined = render the full tree.
  filter?: TreeFilter | null;
  // The keyboard-cursored row path (filter mode), drawn with the shared
  // selection-highlight language.
  cursorPath?: string | null;
}

// The narrowed-view shape useDirTree exposes (proposal 0038, Part C).
export interface TreeFilter {
  query: string;
  matchedFiles: Set<string>;
  matchedDirs: Set<string>;
  visibleDirs: Set<string>;
  expandDirs: Set<string>;
  rows: { path: string; name: string; isDir: boolean }[];
}

// buildTreeFilter narrows the LOADED tree to `query` (proposal 0038, Part C):
// it collects matching files/dirs, keeps + auto-expands their ancestors, and
// produces a flat render-order row list. Pure over (cache, sections, query) so
// it's unit-testable; the hook memoises it. Ancestors are walked via dirnameOf
// up to (not including) "/", so paths above a section root are simply never
// rendered (the renderer only descends from roots).
export function buildTreeFilter(
  cache: Map<string, DirContents>,
  sections: TreeSection[],
  query: string
): TreeFilter {
  const matchedFiles = new Set<string>();
  const matchedDirs = new Set<string>();
  const keepDirs = new Set<string>(); // ancestors of matches → show + expand
  const addAncestors = (p: string) => {
    let cur = dirnameOf(p);
    while (cur && cur !== "/") {
      if (keepDirs.has(cur)) break;
      keepDirs.add(cur);
      cur = dirnameOf(cur);
    }
  };
  for (const contents of cache.values()) {
    for (const f of contents.files) {
      if (fuzzyScore(query, f.name) !== null) {
        matchedFiles.add(f.path);
        addAncestors(f.path);
      }
    }
    for (const d of contents.dirs) {
      if (fuzzyScore(query, d.name) !== null) {
        matchedDirs.add(d.path);
        addAncestors(d.path);
      }
    }
  }
  const visibleDirs = new Set<string>([...keepDirs, ...matchedDirs]);
  const expandDirs = keepDirs;
  // Walk from each section root in render order (dirs first, then files —
  // mirrors FolderChildren) so the flat list matches what the user sees.
  const rows: { path: string; name: string; isDir: boolean }[] = [];
  const walk = (dirPath: string) => {
    const c = cache.get(dirPath);
    if (!c) return;
    for (const d of c.dirs) {
      if (!visibleDirs.has(d.path)) continue;
      rows.push({ path: d.path, name: d.name, isDir: true });
      if (expandDirs.has(d.path)) walk(d.path);
    }
    for (const f of c.files) {
      if (matchedFiles.has(f.path)) rows.push({ path: f.path, name: f.name, isDir: false });
    }
  };
  for (const sec of sections) if (sec.path) walk(sec.path);
  return { query, matchedFiles, matchedDirs, visibleDirs, expandDirs, rows };
}

// HighlightedName wraps the fuzzy-matched characters of `name` in an accent
// <mark> so the eye lands on *why* a filtered row is shown (friendlier than VS
// Code, which only dims non-matches). No query → the bare name.
export function HighlightedName({ name, query }: { name: string; query?: string | null }) {
  if (!query) return <>{name}</>;
  const pos = fuzzyMatchPositions(query, name);
  if (!pos || pos.length === 0) return <>{name}</>;
  const hit = new Set(pos);
  const nodes: ReactNode[] = [];
  let i = 0;
  while (i < name.length) {
    const on = hit.has(i);
    let j = i;
    while (j < name.length && hit.has(j) === on) j++;
    const chunk = name.slice(i, j);
    nodes.push(
      on ? (
        <mark key={i} className="bg-transparent font-medium text-accent">
          {chunk}
        </mark>
      ) : (
        <span key={i}>{chunk}</span>
      )
    );
    i = j;
  }
  return <>{nodes}</>;
}

// Download glyph for the per-file download button (tray with a down-arrow).
function DownloadGlyph() {
  return (
    <svg viewBox="0 0 24 24" width="15" height="15" aria-hidden="true">
      <path
        d="M12 3v10m0 0l-4-4m4 4l4-4M5 17v2a1 1 0 001 1h12a1 1 0 001-1v-2"
        fill="none"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

// A small rotating chevron for folder rows (compact tree).
function Caret({ open }: { open: boolean }) {
  return (
    <svg
      viewBox="0 0 24 24"
      width="11"
      height="11"
      className={`shrink-0 text-slate-500 transition-transform duration-150 ${open ? "rotate-90" : ""}`}
      aria-hidden="true"
    >
      <path d="M9 6l6 6-6 6" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}

// FolderChildren renders the dirs+files of one cached folder, recursing into
// expanded subfolders. Depth drives left-indent so nesting is visible without
// dominating the row.
// useTreeContextHandlers wires right-click (desktop) + long-press (touch) to the
// same `onContextMenu`. `ctxHandlers(info)` spreads onto a row; a 450ms hold
// without movement fires the menu, and `swallowLongPress()` lets the row's
// onClick discard the tap that follows so the press doesn't also open/toggle it.
// Shared by FolderChildren's rows and EditorTree's section headers.
export function useTreeContextHandlers(
  onContextMenu?: (e: CtxEvt, info: TreeCtxInfo) => void
) {
  const lpTimer = useRef<number | null>(null);
  const lpFired = useRef(false);
  const clearLP = () => {
    if (lpTimer.current !== null) {
      clearTimeout(lpTimer.current);
      lpTimer.current = null;
    }
  };
  const ctxHandlers = (info: TreeCtxInfo) =>
    onContextMenu
      ? {
          onContextMenu: (e: ReactMouseEvent) => onContextMenu(e, info),
          onTouchStart: (e: ReactTouchEvent) => {
            lpFired.current = false;
            const { clientX, clientY } = e.touches[0];
            clearLP();
            lpTimer.current = window.setTimeout(() => {
              lpFired.current = true;
              onContextMenu({ clientX, clientY, preventDefault: () => {} }, info);
            }, 450);
          },
          onTouchMove: clearLP,
          onTouchEnd: clearLP,
          onTouchCancel: clearLP,
        }
      : {};
  const swallowLongPress = () => {
    if (lpFired.current) {
      lpFired.current = false;
      return true;
    }
    return false;
  };
  return { ctxHandlers, swallowLongPress };
}

// useFolderDrop turns one tree element (a folder row or a section header) into
// an OS-file drop target. `dir` is the absolute folder a drop lands in;
// `onDropFiles` is the consumer's handler. Returns the highlight flag + the
// handlers to spread onto the element.
//
// Two non-obvious bits, both mirroring the per-pane drop wiring in TileGrid
// (see AGENTS.md "Drag-and-drop file upload"):
//   1. enter/leave counter — dragleave fires every time the cursor crosses a
//      child node (the row's icon, label span, accessory), so a naive boolean
//      flickers. We count enters minus leaves and only drop the highlight at 0.
//   2. stopPropagation on every phase — folder rows nest, and the editor's tree
//      sits inside the App-level window drop guard; stopping propagation keeps
//      the *innermost* folder the sole target and prevents the guard (or an
//      ancestor row) from also reacting. We still preventDefault so the browser
//      doesn't navigate to the dropped file.
// Gated to OS file drags (dataTransfer carries "Files") so in-app drags pass
// through untouched. useState/useRef run unconditionally to satisfy the rules
// of hooks; the no-op branch returns empty handlers when drop is disabled.
export type DragHandlers = Partial<{
  onDragEnter: (e: ReactDragEvent) => void;
  onDragOver: (e: ReactDragEvent) => void;
  onDragLeave: (e: ReactDragEvent) => void;
  onDrop: (e: ReactDragEvent) => void;
}>;

// ── Internal node move (drag a tree node onto a folder) — proposal 0012 ──────
// A custom MIME tags in-app node drags so they stay distinct from OS-file drags
// (which carry "Files" and still route to upload). `draggedNodePath` mirrors the
// drag source's path at module scope because dataTransfer.getData() is write-only
// until the drop fires — drop targets need the source path DURING dragover to
// validate same-parent/descendant targets (dim / no-drop cursor).
export const MOVE_MIME = "application/x-ccscreen-path";
let draggedNodePath: string | null = null;

function dirnameOf(p: string): string {
  const i = p.lastIndexOf("/");
  return i <= 0 ? "/" : p.slice(0, i);
}

// A move of `src` into `destDir` is allowed unless it's a no-op (src already in
// destDir) or illegal (destDir is src itself or a descendant of it — mirrors the
// backend's `dst.starts_with(src)` guard).
function moveAllowed(src: string | null, destDir: string): boolean {
  if (!src) return false;
  if (dirnameOf(src) === destDir) return false; // already there
  if (destDir === src || destDir.startsWith(src + "/")) return false; // into self/descendant
  return true;
}

// Drag-source handlers for a movable tree row. Wired only when the consumer
// supplies an onMoveNode handler (so the download-only Files sheet stays inert).
export function nodeDragProps(path: string): {
  draggable: boolean;
  onDragStart: (e: ReactDragEvent) => void;
  onDragEnd: () => void;
} {
  return {
    draggable: true,
    onDragStart: (e) => {
      e.stopPropagation();
      draggedNodePath = path;
      e.dataTransfer.setData(MOVE_MIME, path);
      e.dataTransfer.effectAllowed = "move";
    },
    onDragEnd: () => {
      draggedNodePath = null;
    },
  };
}

// useFolderDrop turns one folder element into a drop target for BOTH OS-file
// uploads (onDropFiles, "Files" drags → copy) and in-app node moves (onMoveNode,
// MOVE_MIME drags → move). For a move drag we always claim the innermost folder
// (stopPropagation) so an invalid inner target can't bubble up to a valid
// grandparent, but only highlight + accept the drop on a valid target.
export function useFolderDrop(
  dir: string | null | undefined,
  onDropFiles?: (dir: string, dt: DataTransfer) => void,
  onMoveNode?: (src: string, destDir: string) => void
): { over: boolean; dropHandlers: DragHandlers } {
  const [over, setOver] = useState(false);
  const depth = useRef(0);
  if ((!onDropFiles && !onMoveNode) || !dir) return { over: false, dropHandlers: {} };
  const types = (e: ReactDragEvent) => Array.from(e.dataTransfer.types || []);
  const isFileDrag = (e: ReactDragEvent) => !!onDropFiles && types(e).includes("Files");
  const isMoveDrag = (e: ReactDragEvent) => !!onMoveNode && types(e).includes(MOVE_MIME);
  const moveOk = () => moveAllowed(draggedNodePath, dir);
  return {
    over,
    dropHandlers: {
      onDragEnter: (e) => {
        if (isMoveDrag(e)) {
          e.stopPropagation(); // claim the innermost folder regardless of validity
          if (!moveOk()) return; // invalid target: no highlight
          e.preventDefault();
          depth.current += 1;
          setOver(true);
          return;
        }
        if (!isFileDrag(e)) return;
        e.preventDefault();
        e.stopPropagation();
        depth.current += 1;
        setOver(true);
      },
      onDragOver: (e) => {
        if (isMoveDrag(e)) {
          e.stopPropagation();
          if (!moveOk()) {
            e.dataTransfer.dropEffect = "none"; // show the no-drop cursor
            return;
          }
          e.preventDefault(); // make this a legal drop target
          e.dataTransfer.dropEffect = "move";
          return;
        }
        if (!isFileDrag(e)) return;
        e.preventDefault();
        e.stopPropagation();
        e.dataTransfer.dropEffect = "copy";
      },
      onDragLeave: (e) => {
        if (isMoveDrag(e)) {
          e.stopPropagation();
          if (!moveOk()) return;
          depth.current = Math.max(0, depth.current - 1);
          if (depth.current === 0) setOver(false);
          return;
        }
        if (!isFileDrag(e)) return;
        e.stopPropagation();
        depth.current = Math.max(0, depth.current - 1);
        if (depth.current === 0) setOver(false);
      },
      onDrop: (e) => {
        if (isMoveDrag(e)) {
          e.stopPropagation();
          if (!moveOk()) return;
          e.preventDefault();
          depth.current = 0;
          setOver(false);
          const src = e.dataTransfer.getData(MOVE_MIME) || draggedNodePath || "";
          if (src) onMoveNode!(src, dir);
          return;
        }
        if (!isFileDrag(e)) return;
        e.preventDefault();
        e.stopPropagation();
        depth.current = 0;
        setOver(false);
        onDropFiles!(dir, e.dataTransfer);
      },
    },
  };
}

// DirRow renders one folder row (the toggle button + drop target). Split out of
// FolderChildren's map so each row can own its useFolderDrop hook instance; the
// recursion into expanded children stays in FolderChildren.
function DirRow({
  d,
  isOpen,
  isLoading,
  rowCls,
  pad,
  compact,
  onToggle,
  swallowLongPress,
  ctxHandlers,
  onDropFiles,
  onMoveNode,
  filterQuery,
  cursored,
}: {
  d: DirEntry;
  isOpen: boolean;
  isLoading: boolean;
  rowCls: string;
  pad: { paddingLeft: string };
  compact?: boolean;
  onToggle: (path: string) => void;
  swallowLongPress: () => boolean;
  ctxHandlers: ReturnType<typeof useTreeContextHandlers>["ctxHandlers"];
  onDropFiles?: (dir: string, dt: DataTransfer) => void;
  onMoveNode?: (src: string, destDir: string) => void;
  // Filter mode (0038): highlight the matched substring + draw the row cursor.
  filterQuery?: string | null;
  cursored?: boolean;
}) {
  const { over, dropHandlers } = useFolderDrop(d.path, onDropFiles, onMoveNode);
  const cursorCls = cursored ? "bg-edge/70 ring-1 ring-inset ring-accent/40" : "";
  return (
    <button
      onClick={() => {
        if (swallowLongPress()) return;
        onToggle(d.path);
      }}
      className={`${rowCls} ${over ? "bg-accent/15 ring-1 ring-inset ring-accent/60" : cursorCls}`}
      style={pad}
      {...(onMoveNode ? nodeDragProps(d.path) : {})}
      {...ctxHandlers({ path: d.path, name: d.name, isDir: true })}
      {...dropHandlers}
    >
      {compact ? (
        <Caret open={isOpen} />
      ) : (
        <>
          <span className="inline-block w-3 shrink-0 text-slate-500" aria-hidden="true">
            {isOpen ? "▼" : "▶"}
          </span>
          <span className="shrink-0 text-slate-500">📁</span>
        </>
      )}
      <span className={`min-w-0 flex-1 truncate ${compact ? "text-slate-300" : "text-slate-100"}`}>
        <HighlightedName name={d.name} query={filterQuery} />
      </span>
      {isLoading && <span className="text-xs text-slate-500">…</span>}
    </button>
  );
}

export function FolderChildren(props: FolderChildrenProps) {
  const { path, depth, cache, expanded, loading, onToggle, onFile, onDownload, downloadingPath, onContextMenu, onDropFiles, onMoveNode, fileDisabled, activePath, compact, touch, filter, cursorPath } = props;
  const { ctxHandlers, swallowLongPress } = useTreeContextHandlers(onContextMenu);
  const fq = filter ? filter.query : null;
  // This whole block (the folder's listed contents) is a drop target for the
  // folder itself, so a drop on a *file* row or the gaps between rows lands in
  // this folder — not only a drop on the folder's own row. Nested subfolder
  // rows (DirRow) and their expanded contents (the recursive FolderChildren
  // below) are inner drop zones that win via stopPropagation, so the innermost
  // folder under the cursor is always the target.
  const { over, dropHandlers } = useFolderDrop(path, onDropFiles, onMoveNode);

  const data = cache.get(path);
  if (!data) return null;

  // Compact rows indent tighter and align files under folder labels. The touch
  // variant keeps the compact look but with larger type + taller rows for a
  // phone (so the in-editor tree matches the roomier Files sheet).
  const step = compact ? (touch ? 16 : 13) : 16;
  const base = compact ? (touch ? 12 : 8) : 28;
  const pad = { paddingLeft: `${depth * step + base}px` };
  const rowCls = compact
    ? touch
      ? "flex w-full items-center gap-2 rounded-md py-2 pr-2 text-left text-[15px] leading-snug active:bg-edge/40"
      : "flex w-full items-center gap-1.5 rounded-md py-[3px] pr-2 text-left text-[13px] leading-snug hover:bg-edge/40"
    : "flex w-full items-center gap-2 border-t border-edge/20 py-2 pr-3 text-left active:bg-panel";

  return (
    <div
      className={over ? "rounded-md bg-accent/10 ring-1 ring-inset ring-accent/30" : undefined}
      {...dropHandlers}
    >
      {data.dirs.map((d) => {
        // Filter mode: hide dirs that aren't a match or an ancestor of one.
        if (filter && !filter.visibleDirs.has(d.path)) return null;
        const isOpen = expanded.has(d.path);
        const isLoading = loading.has(d.path);
        return (
          <div key={d.path}>
            <DirRow
              d={d}
              isOpen={isOpen}
              isLoading={isLoading}
              rowCls={rowCls}
              pad={pad}
              compact={compact}
              onToggle={onToggle}
              swallowLongPress={swallowLongPress}
              ctxHandlers={ctxHandlers}
              onDropFiles={onDropFiles}
              onMoveNode={onMoveNode}
              filterQuery={fq}
              cursored={cursorPath === d.path}
            />
            {isOpen && <FolderChildren {...props} path={d.path} depth={depth + 1} />}
          </div>
        );
      })}
      {data.files.map((f) => {
        // Filter mode: only matched files survive.
        if (filter && !filter.matchedFiles.has(f.path)) return null;
        const isActive = activePath === f.path;
        const isCursored = cursorPath === f.path;
        const isDownloading = downloadingPath === f.path;
        // The row splits into an open-button (flex-1) and an optional download
        // button so the download tap isn't swallowed by the open handler (a
        // button can't nest inside a button). Folder rows above stay a single
        // toggle button. The active-file highlight lives on the wrapper so the
        // whole row (incl. the download control) reads as selected.
        return (
          <div
            key={f.path}
            className={`flex items-stretch ${compact ? "rounded-md" : "border-t border-edge/20"} ${
              isActive
                ? "bg-accent/10 shadow-[inset_2px_0_0_#38bdf8]"
                : isCursored
                ? "bg-edge/70 ring-1 ring-inset ring-accent/40 rounded-md"
                : ""
            }`}
          >
            <button
              onClick={() => {
                if (swallowLongPress()) return;
                onFile(f);
              }}
              disabled={fileDisabled}
              className={`${rowCls.replace("w-full", "min-w-0 flex-1")} disabled:opacity-60`}
              style={pad}
              {...(onMoveNode ? nodeDragProps(f.path) : {})}
              {...ctxHandlers({ path: f.path, name: f.name, isDir: false })}
            >
              {/* Spacer aligns file labels under folder labels (past the caret). */}
              <span className={`inline-block shrink-0 ${compact ? "w-[11px]" : "w-3"}`} aria-hidden="true" />
              {!compact && <span className="shrink-0 text-slate-500">📄</span>}
              <span
                className={`min-w-0 flex-1 truncate ${
                  isActive ? "font-medium text-accent" : compact ? "text-slate-400" : "text-slate-100"
                }`}
              >
                <HighlightedName name={f.name} query={fq} />
              </span>
              {!compact && (
                <>
                  <span className="shrink-0 text-xs tabular-nums text-slate-500">{fmtSize(f.size)}</span>
                  <span className="shrink-0 text-xs tabular-nums text-slate-500">{fmtAge(f.mtime)}</span>
                </>
              )}
            </button>
            {onDownload && (
              <button
                onClick={() => onDownload(f)}
                disabled={fileDisabled || isDownloading}
                className="flex shrink-0 items-center px-2.5 text-slate-500 hover:text-sky-400 active:text-sky-400 disabled:opacity-50"
                aria-label={`Download ${f.name}`}
                title="Download to device"
              >
                {isDownloading ? <span className="text-xs">…</span> : <DownloadGlyph />}
              </button>
            )}
          </div>
        );
      })}
    </div>
  );
}
