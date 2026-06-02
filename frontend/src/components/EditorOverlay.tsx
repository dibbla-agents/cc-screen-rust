import {
  lazy,
  Suspense,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
} from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { writeClipboard } from "../util";
import {
  readFile,
  writeFile,
  deleteFile,
  saveFileToDevice,
  makeDir,
  removeDir,
  renamePath,
  flattenDataTransfer,
  uploadFiles,
  FileNotEditable,
  FileChangedOnDisk,
  type FileEntry,
} from "../api";
import ContextMenu, { type CtxTarget } from "./ContextMenu";
import { errMsg, isMarkdownFile, isPdfFile, useDirTree, type DirTreeOpts, type TreeCtxInfo } from "./dirTree";
import MarkdownEditor from "./MarkdownEditor";
import EditorTree from "./EditorTree";
import AgentMirror, { type ConnState } from "./AgentMirror";

// pdf.js is heavy and only needed for PDFs — keep it (and its worker) out of the
// editor's chunk, loaded only when a PDF is first opened.
const PdfViewer = lazy(() => import("./PdfViewer"));
import {
  XIcon,
  FilePlusIcon,
  BookIcon,
  PencilIcon,
  SidebarIcon,
  FileEditIcon,
  TrashIcon,
  MoreIcon,
  DownloadIcon,
  TerminalIcon,
  KeyboardIcon,
} from "../icons";

// A faint top-down glow so the centered writing column sits on a surface with
// depth rather than a flat fill — subtle enough to stay out of the way.
const SURFACE_BG =
  "radial-gradient(130% 70% at 50% -10%, rgba(56,189,248,0.06), transparent 55%)";

interface Props {
  open: boolean;
  // File to open when the overlay opens (from a Files-sheet tap). Null on a
  // desktop Ctrl+B e, where the user picks from the tree.
  initialPath: string | null;
  // Active session — anchors the tree's "project" section at its tmux cwd, and
  // is the agent shown in the right-hand mirror column.
  session: string | null;
  isDesktop: boolean;
  onClose: () => void;
  // Authoritative grid size of the active session's terminal (the active grid
  // pane's xterm cols/rows). The mirror renders at exactly this grid so it
  // never reports a size and can't shrink the shared, width-locked PTY. Falls
  // back to 80×24 when the grid term isn't ready.
  agentCols?: number;
  agentRows?: number;
  // Terminal font size — the upper bound for the mirror's auto-fitted font.
  termFontSize?: number;
}

// "ready" = a text file is loaded and editable; "pdf" = the active file is a PDF
// shown read-only in the pdf.js viewer (no text read/write happens for it).
type Status = "empty" | "loading" | "ready" | "error" | "noteditable" | "pdf";

// Editor font size (px), shared across markdown live-preview + reading view via
// the --cc-editor-font CSS variable. Persisted so it survives reopen/reload.
const FONT_KEY = "ccweb.editorFontSize";
const FONT_MIN = 11;
const FONT_MAX = 28;
const FONT_DEFAULT = 15;
const clampFont = (n: number) => Math.max(FONT_MIN, Math.min(FONT_MAX, n));
function loadFontSize(): number {
  const n = parseInt(localStorage.getItem(FONT_KEY) || "", 10);
  return Number.isFinite(n) ? clampFont(n) : FONT_DEFAULT;
}

// Live-save (autosave) preference — ON by default. Persisted so it sticks.
const LIVE_KEY = "ccweb.editorLiveSave";
const LIVE_DEBOUNCE_MS = 700;
const loadLiveSave = () => localStorage.getItem(LIVE_KEY) !== "0";

// Desktop file-tree sidebar width (px). Drag the splitter on its right edge to
// resize; persisted so it survives reopen/reload. Default matches the old fixed
// Tailwind w-64 (16rem).
const TREE_W_KEY = "ccweb.editorTreeWidth";
const TREE_W_MIN = 180;
const TREE_W_MAX = 600;
const TREE_W_DEFAULT = 256;
const clampTreeW = (n: number) => Math.max(TREE_W_MIN, Math.min(TREE_W_MAX, n));
function loadTreeWidth(): number {
  const n = parseInt(localStorage.getItem(TREE_W_KEY) || "", 10);
  return Number.isFinite(n) ? clampTreeW(n) : TREE_W_DEFAULT;
}

// Desktop agent-mirror column width (px) + open/closed state. The right column
// shows the active session's live agent; drag the splitter on its LEFT edge to
// resize (the mirror image of the tree splitter). Both persisted.
const AGENT_W_KEY = "ccweb.editorAgentWidth";
const AGENT_W_MIN = 280;
const AGENT_W_MAX = 760;
const AGENT_W_DEFAULT = 420;
const clampAgentW = (n: number) => Math.max(AGENT_W_MIN, Math.min(AGENT_W_MAX, n));
function loadAgentWidth(): number {
  const n = parseInt(localStorage.getItem(AGENT_W_KEY) || "", 10);
  return Number.isFinite(n) ? clampAgentW(n) : AGENT_W_DEFAULT;
}
const AGENT_OPEN_KEY = "ccweb.editorAgentOpen";
const loadAgentOpen = () => localStorage.getItem(AGENT_OPEN_KEY) !== "0";

const basename = (p: string) => p.slice(p.lastIndexOf("/") + 1);

// The editor reads the tree as a working-tree (vault) view, not a download
// browser: the project root sits on top under its bare name and opens on its
// own when the overlay mounts; Home and the share dropbox stay below, closed.
// Module-level so the reference is stable across renders (it's a memo dep in
// useDirTree).
const EDITOR_TREE_OPTS: DirTreeOpts = {
  order: ["project", "home", "share"],
  autoExpand: "project",
  bareProjectLabel: true,
};
const dirname = (p: string) => {
  const i = p.lastIndexOf("/");
  return i <= 0 ? "/" : p.slice(0, i);
};

// EditorOverlay — the singleton, full-screen markdown/text editor. It is NOT
// per-pane: it covers the entire screen (over the tiled terminals on desktop,
// over the single terminal on phone), with its own toolbar. Live-preview
// markdown editing via CodeMirror; a left file tree on desktop. Saves are
// $HOME-confined and mtime-guarded server-side (see editor.go).
export default function EditorOverlay({
  open,
  initialPath,
  session,
  isDesktop,
  onClose,
  agentCols = 0,
  agentRows = 0,
  termFontSize = 14,
}: Props) {
  const [activePath, setActivePath] = useState<string | null>(initialPath);
  const [content, setContent] = useState("");
  const [loaded, setLoaded] = useState(""); // last-saved/loaded content (for dirty)
  const [baseMtime, setBaseMtime] = useState(0);
  const [name, setName] = useState("");
  const [status, setStatus] = useState<Status>("empty");
  const [error, setError] = useState("");
  const [reading, setReading] = useState(false);
  const [fontSize, setFontSize] = useState(loadFontSize);
  const [liveSave, setLiveSave] = useState(loadLiveSave);
  const [conflict, setConflict] = useState(false);
  const [saving, setSaving] = useState(false);
  // New-file input bar.
  const [newOpen, setNewOpen] = useState(false);
  const [newName, setNewName] = useState("");
  // Delete-confirmation bar.
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [deleting, setDeleting] = useState(false);
  // Desktop file tree visibility (collapsible to maximise the writing surface)
  // + its drag-resizable width. resizingTree drives the global col-resize
  // cursor / select-none while the splitter is held.
  const [treeOpen, setTreeOpen] = useState(true);
  const [treeWidth, setTreeWidth] = useState(loadTreeWidth);
  const [resizingTree, setResizingTree] = useState(false);
  const treeRef = useRef<HTMLDivElement | null>(null);
  // Right-hand agent mirror: open/closed + drag-resizable width (mirror of the
  // tree), its connection state for the header dot, and `agentControl` — phase
  // 2's keyboard takeover. agentControlRef lets the capture-phase keydown
  // handler below go inert (so Esc/Ctrl+S reach the focused terminal, and the
  // editor's own shortcuts don't leak to the agent) without re-binding.
  const [agentOpen, setAgentOpen] = useState(loadAgentOpen);
  const [agentWidth, setAgentWidth] = useState(loadAgentWidth);
  const [resizingAgent, setResizingAgent] = useState(false);
  const [agentConn, setAgentConn] = useState<ConnState>("connecting");
  const [agentControl, setAgentControl] = useState(false);
  // Bumped by the splitter's double-click to re-fit the mirror's column count to
  // the current width (between bumps a drag only zooms the font — see
  // AgentMirror).
  const [agentRecalibrate, setAgentRecalibrate] = useState(0);
  const agentRef = useRef<HTMLDivElement | null>(null);
  const agentControlRef = useRef(false);
  useEffect(() => { agentControlRef.current = agentControl; }, [agentControl]);
  // Releasing control whenever focus returns to the writing surface or the tree
  // keeps the rule simple: the agent owns the keyboard only while you're
  // pointed at it. Engaged explicitly via the column's keyboard toggle.
  const releaseControl = useCallback(() => {
    if (agentControlRef.current) setAgentControl(false);
  }, []);
  // Phone equivalents: the desktop sidebar has no room on a phone, so file
  // navigation lives in a slide-over panel (treePanelOpen) summoned from the
  // toolbar, and the secondary toolbar actions (font, auto-save, new, delete)
  // collapse into an overflow "⋯" menu (menuOpen).
  const [treePanelOpen, setTreePanelOpen] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);
  // Soft-keyboard-aware height (iOS): the overlay is fixed/full-screen, so it
  // must track visualViewport like the rest of the app does.
  const [vh, setVh] = useState<number | null>(null);

  // The file tree backing both the desktop sidebar and the phone slide-over
  // panel. Enabled on both now (it's also what gives the phone a real folder to
  // anchor new files in and the ~-abbreviated status-bar path).
  const tree = useDirTree(open, session, EDITOR_TREE_OPTS);

  // Drag-and-drop upload INTO a tree folder. A drop on a folder row, a folder's
  // file listing, or a section header (wired through EditorTree → FolderChildren
  // → useFolderDrop) lands here with the target dir + DataTransfer. We upload
  // straight away — no intermediate sheet — flattening the drop (folders walked
  // via webkitGetAsEntry), then auto-expanding the destination so the new files
  // show. The upload is session-less ($HOME-confined — see uploadRoot in
  // upload.go), matching every other editor file op; collisions are resolved by
  // the server's safe default (rename, never overwrite). A small toast reports
  // progress + the result. statusTimer auto-dismisses the finished toast.
  const [uploadStatus, setUploadStatus] = useState<{
    total: number;
    dir: string;
    progress: number;
    done: boolean;
    errors: number;
  } | null>(null);
  const statusTimer = useRef<number | null>(null);
  useEffect(
    () => () => {
      if (statusTimer.current != null) window.clearTimeout(statusTimer.current);
    },
    []
  );
  const onTreeDropFiles = useCallback(
    async (dir: string, dt: DataTransfer) => {
      setError("");
      let list;
      try {
        list = await flattenDataTransfer(dt);
      } catch (e) {
        setError(errMsg(e));
        return;
      }
      if (list.length === 0) {
        setError("Nothing to upload — the drop carried no files.");
        return;
      }
      if (statusTimer.current != null) {
        window.clearTimeout(statusTimer.current);
        statusTimer.current = null;
      }
      setUploadStatus({ total: list.length, dir, progress: 0, done: false, errors: 0 });
      try {
        const r = await uploadFiles(null, dir, list, {}, (frac) =>
          setUploadStatus((s) => (s && !s.done ? { ...s, progress: frac } : s))
        );
        const errs = r.errors ? Object.keys(r.errors).length : 0;
        setUploadStatus({ total: list.length, dir, progress: 1, done: true, errors: errs });
        await tree.expand(dir); // open the destination so the new files show
        statusTimer.current = window.setTimeout(() => {
          setUploadStatus(null);
          statusTimer.current = null;
        }, 2800);
      } catch (e) {
        setError(errMsg(e));
        setUploadStatus(null);
      }
    },
    [tree]
  );

  // Per-row download (saves to the device via navigator.share / <a download>).
  // This is what makes the editor the single file view: you can open/edit a
  // file AND download any file from the same tree.
  const [downloading, setDownloading] = useState<string | null>(null);
  const onDownload = useCallback(async (f: FileEntry) => {
    setDownloading(f.path);
    setError("");
    try {
      await saveFileToDevice(f.path, f.name);
    } catch (e) {
      setError(errMsg(e));
    } finally {
      setDownloading(null);
    }
  }, []);

  // ── Right-click / long-press file-tree context menu ──────────────────────
  // The tree (EditorTree → FolderChildren) reports a target + cursor; we open a
  // ContextMenu and run the CRUD here (this component owns the API calls, the
  // tree cache and `activePath`). Section roots (share/project/home) can't be
  // renamed/deleted — flag them so the menu hides those items.
  const [ctx, setCtx] = useState<{ x: number; y: number; target: CtxTarget } | null>(null);
  const onTreeContextMenu = useCallback(
    (e: { clientX: number; clientY: number; preventDefault: () => void }, info: TreeCtxInfo) => {
      e.preventDefault();
      const isRoot =
        info.isDir &&
        (info.path === tree.sharePath || info.path === tree.projectPath || info.path === tree.homePath);
      setCtx({
        x: e.clientX,
        y: e.clientY,
        target: info.isDir
          ? { kind: "dir", path: info.path, name: info.name, root: isRoot }
          : { kind: "file", path: info.path, name: info.name },
      });
    },
    [tree.sharePath, tree.projectPath, tree.homePath]
  );

  // CRUD actions the menu invokes. They await the API (errors surface in the
  // menu), then refresh the affected folder and keep the open file in sync.
  const ctxNewFile = useCallback(
    async (dir: string, n: string) => {
      const p = `${dir}/${n}`;
      await writeFile(p, "");
      await tree.refresh(dir);
      setActivePath(p); // open the new file
      setTreePanelOpen(false); // phone: reveal it (no-op on desktop)
    },
    [tree]
  );
  const ctxNewFolder = useCallback(
    async (dir: string, n: string) => {
      await makeDir(dir, n);
      await tree.refresh(dir);
    },
    [tree]
  );
  const ctxRename = useCallback(
    async (p: string, n: string) => {
      const { path: np } = await renamePath(p, n);
      await tree.refresh(dirname(p));
      // Keep the open file pointing at its new path (covers renaming the file
      // itself or any ancestor folder of it).
      setActivePath((cur) => {
        if (cur === p) return np;
        if (cur && cur.startsWith(p + "/")) return np + cur.slice(p.length);
        return cur;
      });
    },
    [tree]
  );
  const ctxDeleteFile = useCallback(
    async (p: string) => {
      await deleteFile(p);
      await tree.refresh(dirname(p));
      setActivePath((cur) => (cur === p ? null : cur));
    },
    [tree]
  );
  const ctxDeleteFolder = useCallback(
    async (p: string) => {
      await removeDir(p, true);
      await tree.refresh(dirname(p));
      setActivePath((cur) => (cur === p || (cur && cur.startsWith(p + "/")) ? null : cur));
    },
    [tree]
  );

  const dirty = status === "ready" && content !== loaded;
  const isMd = isMarkdownFile(name);

  // Refs so the filesystem-watch listener (registered once) reads fresh values.
  const activePathRef = useRef(activePath);
  activePathRef.current = activePath;
  const dirtyRef = useRef(dirty);
  dirtyRef.current = dirty;
  const statusRef = useRef(status);
  statusRef.current = status;
  // Suppress watch echoes from our own writes. mtime is 1s-granular (so an
  // external edit in the same second can't be told apart by mtime); a short
  // time window after each write is the robust guard.
  const ignoreWatchUntil = useRef(0);

  // Live document stats for the status bar. Words/reading-time are the useful
  // numbers for prose; code files get lines instead.
  const stats = useMemo(() => {
    const chars = content.length;
    const lines = content ? content.split("\n").length : 0;
    const words = (content.match(/\S+/g) || []).length;
    const mins = Math.max(1, Math.round(words / 200));
    return { chars, lines, words, mins };
  }, [content]);

  // Path shown in the status bar, abbreviated to ~ when we know $HOME.
  const relDir = useMemo(() => {
    if (!activePath) return "";
    const d = dirname(activePath);
    const home = tree.homePath;
    if (home && d === home) return "~";
    if (home && d.startsWith(home + "/")) return "~" + d.slice(home.length);
    return d;
  }, [activePath, tree.homePath]);

  useEffect(() => {
    localStorage.setItem(FONT_KEY, String(fontSize));
  }, [fontSize]);
  const bumpFont = useCallback((d: number) => setFontSize((f) => clampFont(f + d)), []);

  useEffect(() => {
    localStorage.setItem(LIVE_KEY, liveSave ? "1" : "0");
  }, [liveSave]);

  useEffect(() => {
    localStorage.setItem(TREE_W_KEY, String(treeWidth));
  }, [treeWidth]);

  useEffect(() => {
    localStorage.setItem(AGENT_W_KEY, String(agentWidth));
  }, [agentWidth]);

  useEffect(() => {
    localStorage.setItem(AGENT_OPEN_KEY, agentOpen ? "1" : "0");
  }, [agentOpen]);

  // Desktop sidebar resize: drag the splitter on the tree's right edge. We track
  // the pointer on window (so the drag keeps up even when the cursor outruns the
  // thin handle) and set width = cursor-x minus the sidebar's left edge, clamped.
  const startTreeResize = useCallback((e: ReactPointerEvent) => {
    e.preventDefault();
    const left = treeRef.current?.getBoundingClientRect().left ?? 0;
    setResizingTree(true);
    const onMove = (ev: PointerEvent) => setTreeWidth(clampTreeW(ev.clientX - left));
    const onUp = () => {
      setResizingTree(false);
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
  }, []);

  // Agent column resize: the mirror image of the tree splitter — its handle is
  // on the column's LEFT edge and the width grows as the cursor moves left, so
  // we anchor on the column's (fixed) right edge and subtract the cursor x.
  const startAgentResize = useCallback((e: ReactPointerEvent) => {
    e.preventDefault();
    const right = agentRef.current?.getBoundingClientRect().right ?? window.innerWidth;
    setResizingAgent(true);
    const onMove = (ev: PointerEvent) => setAgentWidth(clampAgentW(right - ev.clientX));
    const onUp = () => {
      setResizingAgent(false);
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
  }, []);

  // visualViewport tracking.
  useEffect(() => {
    if (!open) return;
    const vv = window.visualViewport;
    if (!vv) return;
    const apply = () => setVh(vv.height);
    apply();
    vv.addEventListener("resize", apply);
    vv.addEventListener("scroll", apply);
    return () => {
      vv.removeEventListener("resize", apply);
      vv.removeEventListener("scroll", apply);
    };
  }, [open]);

  // When the overlay opens (or the requested file changes), point at it.
  useEffect(() => {
    if (open) setActivePath(initialPath);
  }, [open, initialPath]);

  // Opened with no file (the ⬇ Files entry / Ctrl+B e) on a phone? Land directly
  // on the file browser rather than the empty state — this overlay IS the file
  // view now, so opening it should show the tree.
  useEffect(() => {
    if (open && !initialPath && !isDesktop) setTreePanelOpen(true);
  }, [open, initialPath, isDesktop]);

  // Load the active file.
  useEffect(() => {
    if (!open) return;
    setConfirmDelete(false);
    if (!activePath) {
      setStatus("empty");
      setContent("");
      setLoaded("");
      setName("");
      setError("");
      setConflict(false);
      return;
    }
    // PDFs are binary — skip the text read entirely and hand the path to the
    // pdf.js viewer. The text-editor toolbar controls all gate on status
    // "ready", so they self-hide in this mode.
    if (isPdfFile(activePath)) {
      setName(basename(activePath));
      setStatus("pdf");
      setContent("");
      setLoaded("");
      setError("");
      setConflict(false);
      setReading(false);
      return;
    }
    let cancelled = false;
    setStatus("loading");
    setError("");
    setConflict(false);
    readFile(activePath)
      .then((r) => {
        if (cancelled) return;
        setContent(r.content);
        setLoaded(r.content);
        setBaseMtime(r.mtime);
        setName(r.name);
        setStatus("ready");
        setReading(false);
      })
      .catch((e) => {
        if (cancelled) return;
        if (e instanceof FileNotEditable) {
          setName(basename(activePath));
          setStatus("noteditable");
        } else {
          setError(errMsg(e));
          setStatus("error");
        }
      });
    return () => {
      cancelled = true;
    };
  }, [open, activePath]);

  const doSave = useCallback(
    async (force = false) => {
      if (!activePath || status !== "ready" || saving) return;
      setSaving(true);
      setError("");
      try {
        const r = await writeFile(activePath, content, force ? 0 : baseMtime);
        setLoaded(content);
        setBaseMtime(r.mtime);
        setConflict(false);
        ignoreWatchUntil.current = Date.now() + 900; // don't reload our own write
      } catch (e) {
        if (e instanceof FileChangedOnDisk) {
          setConflict(true);
        } else {
          setError(errMsg(e));
        }
      } finally {
        setSaving(false);
      }
    },
    [activePath, content, baseMtime, status, saving]
  );

  // Live save: debounce a write after edits settle. Skipped while a conflict is
  // unresolved (otherwise it would 409 in a loop) or a save is already running.
  useEffect(() => {
    if (!liveSave || status !== "ready" || !dirty || conflict || saving) return;
    const t = setTimeout(() => void doSave(), LIVE_DEBOUNCE_MS);
    return () => clearTimeout(t);
  }, [liveSave, status, dirty, conflict, saving, content, doSave]);

  // Silently re-read the open file from disk (clean-buffer case).
  const reloadFromDisk = useCallback(async () => {
    const p = activePathRef.current;
    if (!p) return;
    try {
      const r = await readFile(p);
      setContent(r.content);
      setLoaded(r.content);
      setBaseMtime(r.mtime);
      setName(r.name);
      setConflict(false);
    } catch {
      // Vanished/unreadable mid-edit; the tree reflects any deletion and a save
      // would surface a real error.
    }
  }, []);

  // Real-time open-file reflection: watch the file's directory so an external
  // edit (the agent rewriting it) shows immediately — live-reload a clean
  // buffer, or raise the existing conflict banner when you have unsaved edits
  // (never clobber them). Subscription is ref-counted and shared with the tree.
  useEffect(() => {
    if (!open || !activePath) return;
    const dir = dirname(activePath);
    tree.watch.subscribe(dir);
    return () => tree.watch.unsubscribe(dir);
  }, [open, activePath, tree.watch]);

  useEffect(() => {
    if (!open) return;
    return tree.watch.addListener((evDir, paths) => {
      const p = activePathRef.current;
      if (!p || statusRef.current !== "ready") return;
      if (evDir !== dirname(p)) return; // only the open file's directory
      if (Date.now() < ignoreWatchUntil.current) return; // our own write echo
      const base = p.slice(p.lastIndexOf("/") + 1);
      if (!paths.some((x) => x === p || x.endsWith("/" + base) || x === base)) return;
      if (dirtyRef.current) setConflict(true);
      else void reloadFromDisk();
    });
  }, [open, tree.watch, reloadFromDisk]);

  const requestClose = useCallback(() => {
    // In live-save mode a settled doc is already on disk; only warn if a write
    // is still pending (dirty) — which the debounce will usually have flushed.
    if (dirty && !window.confirm("Discard unsaved changes?")) return;
    onClose();
  }, [dirty, onClose]);

  // Phone: closing a file drops back to the file tree (not all the way out to
  // the terminal). It clears the open file and brings up the tree as a
  // full-screen browser; a second close from there exits the overlay. Same
  // unsaved-changes guard as requestClose.
  const closeFileToTree = useCallback(() => {
    if (dirty && !window.confirm("Discard unsaved changes?")) return;
    setActivePath(null);
    setTreePanelOpen(true);
  }, [dirty]);

  // The toolbar's leading button: on a phone with a file open it steps back to
  // the file tree; otherwise it closes the overlay.
  const onCloseButton = useCallback(() => {
    if (!isDesktop && activePath) closeFileToTree();
    else requestClose();
  }, [isDesktop, activePath, closeFileToTree, requestClose]);

  // Capture-phase keyboard: Esc closes (guard on unsaved), Mod-S saves. Capture
  // so it beats CodeMirror's own handlers and the global Ctrl+B prefix; the App
  // handler early-returns while the overlay is open (editorOpenRef guard).
  const doSaveRef = useRef(doSave);
  const closeRef = useRef(requestClose);
  doSaveRef.current = doSave;
  closeRef.current = requestClose;
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      // Phase 2: while the agent column holds the keyboard, the editor's
      // shortcuts go inert so every key (including Esc / Ctrl+S) reaches the
      // focused terminal instead of leaking into the editor.
      if (agentControlRef.current) return;
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        // Peel transient surfaces off one at a time before closing the overlay.
        if (ctx) {
          setCtx(null);
          return;
        }
        if (menuOpen) {
          setMenuOpen(false);
          return;
        }
        if (treePanelOpen) {
          setTreePanelOpen(false);
          return;
        }
        if (confirmDelete) {
          setConfirmDelete(false);
          return;
        }
        if (newOpen) {
          setNewOpen(false);
          return;
        }
        closeRef.current();
      } else if ((e.metaKey || e.ctrlKey) && (e.key === "s" || e.key === "S")) {
        e.preventDefault();
        e.stopPropagation();
        void doSaveRef.current();
      }
    };
    window.addEventListener("keydown", onKey, { capture: true });
    return () => window.removeEventListener("keydown", onKey, { capture: true });
  }, [open, newOpen, confirmDelete, menuOpen, treePanelOpen, ctx]);

  const reload = useCallback(() => {
    // Force a re-read by nudging activePath through null.
    const p = activePath;
    setActivePath(null);
    setTimeout(() => setActivePath(p), 0);
  }, [activePath]);

  const createNew = useCallback(async () => {
    const n = newName.trim();
    if (!n) return;
    const baseDir = activePath
      ? dirname(activePath)
      : tree.projectPath || tree.sharePath || tree.homePath;
    if (!baseDir) {
      setError("no folder to create the file in");
      return;
    }
    const path = `${baseDir}/${n}`;
    try {
      await writeFile(path, "");
      setNewOpen(false);
      setNewName("");
      setActivePath(path);
      await tree.refresh(baseDir);
    } catch (e) {
      setError(errMsg(e));
    }
  }, [newName, activePath, tree]);

  const doDelete = useCallback(async () => {
    if (!activePath || deleting) return;
    setDeleting(true);
    setError("");
    const gone = activePath;
    try {
      await deleteFile(gone);
      setConfirmDelete(false);
      setActivePath(null); // → empty state
      if (!isDesktop) setTreePanelOpen(true); // phone: land back on the file tree
      await tree.refresh(dirname(gone)); // drop it from the tree
    } catch (e) {
      setError(errMsg(e));
    } finally {
      setDeleting(false);
    }
  }, [activePath, deleting, isDesktop, tree]);

  if (!open) return null;

  const ghostBtn =
    "flex h-9 w-9 shrink-0 items-center justify-center rounded-lg text-slate-400 transition-colors hover:bg-panel hover:text-slate-100 active:bg-edge";
  const saveLabel = saving ? "Saving…" : dirty ? "Save" : "Saved";
  const showFooter = status === "ready";

  return (
    <div
      className={`fixed inset-0 z-[60] flex flex-col bg-bar text-slate-200 ${
        resizingTree || resizingAgent ? "cursor-col-resize select-none" : ""
      }`}
      style={vh ? { height: `${vh}px` } : undefined}
    >
      {/* ── Toolbar ───────────────────────────────────────────────────────── */}
      {/* relative z-40: the toolbar's `backdrop-blur` makes it a stacking
          context, so the phone "⋯" overflow menu nested inside it (z-70) would
          otherwise paint *under* the later body sibling and be untappable.
          Lifting the whole toolbar layer above the body fixes that; the phone
          tree panel already sits at root-level z-[70], so it still covers the
          toolbar when open. */}
      <div className="relative z-40 flex items-center gap-1.5 border-b border-edge bg-bar/95 px-2.5 py-2 pt-safe backdrop-blur">
        <button
          onClick={onCloseButton}
          className={ghostBtn}
          title={!isDesktop && activePath ? "Back to files" : "Close (Esc)"}
          aria-label={!isDesktop && activePath ? "Back to files" : "Close editor"}
        >
          <XIcon className="h-[18px] w-[18px]" />
        </button>

        {isDesktop ? (
          <>
            <button
              onClick={() => setTreeOpen((v) => !v)}
              className={`${ghostBtn} ${treeOpen ? "text-accent" : ""}`}
              title={treeOpen ? "Hide file tree" : "Show file tree"}
              aria-label="Toggle file tree"
            >
              <SidebarIcon className="h-[18px] w-[18px]" />
            </button>
            <button
              onClick={() => setAgentOpen((v) => !v)}
              className={`${ghostBtn} ${agentOpen ? "text-accent" : ""}`}
              title={agentOpen ? "Hide live agent" : "Show live agent"}
              aria-label="Toggle live agent view"
            >
              <TerminalIcon className="h-[18px] w-[18px]" />
            </button>
          </>
        ) : (
          // Phone: no room for a persistent sidebar, so this summons the
          // slide-over file tree (the sidebar's mobile twin).
          <button
            onClick={() => setTreePanelOpen(true)}
            className="flex h-9 shrink-0 items-center gap-1.5 rounded-lg px-2.5 text-sm text-slate-300 transition-colors hover:bg-panel active:bg-edge"
            title="Browse files"
            aria-label="Browse files"
          >
            <SidebarIcon className="h-[18px] w-[18px]" />
            <span>Files</span>
          </button>
        )}

        {/* Filename + dirty state */}
        <div className="ml-1 flex min-w-0 flex-1 items-baseline gap-1.5">
          <span
            className={`h-1.5 w-1.5 shrink-0 rounded-full transition-colors ${
              dirty ? "bg-amber" : "bg-transparent"
            }`}
            title={dirty ? "Unsaved changes" : undefined}
            aria-hidden="true"
          />
          <span className="truncate text-sm font-semibold tracking-tight text-slate-100">
            {name || "Editor"}
          </span>
        </div>

        {/* Download the open file. Shown for both the text editor and the PDF
            viewer (which gates its other controls off), on desktop and phone —
            so "save this file to my device" is always one tap from whatever's
            open, mirroring the per-row ⬇ in the tree. */}
        {(status === "ready" || status === "pdf") && activePath && (
          <button
            onClick={() => onDownload({ name, path: activePath, size: 0, mtime: 0 })}
            disabled={downloading === activePath}
            className={`${ghostBtn} disabled:opacity-50`}
            title="Download to device"
            aria-label="Download file"
          >
            {downloading === activePath ? (
              <span className="text-xs">…</span>
            ) : (
              <DownloadIcon className="h-[18px] w-[18px]" />
            )}
          </button>
        )}

        {/* Trailing controls. Desktop lays everything out inline; phone keeps
            only the reading toggle + manual Save in the bar and folds the rest
            (font, auto-save, new, delete) into a "⋯" overflow menu so the bar
            never overflows a narrow screen. */}
        {isDesktop ? (
          <>
            {status === "ready" && (
              <>
                {/* Font stepper — a refined segmented control */}
                <div className="flex shrink-0 items-center rounded-lg bg-panel/70 ring-1 ring-inset ring-edge">
                  <button
                    onClick={() => bumpFont(-1)}
                    disabled={fontSize <= FONT_MIN}
                    className="flex h-9 items-center rounded-l-lg px-2.5 text-xs text-slate-300 hover:bg-edge disabled:opacity-30"
                    title="Smaller text"
                    aria-label="Decrease font size"
                  >
                    A<span className="text-[9px]">−</span>
                  </button>
                  <span
                    className="min-w-[2ch] text-center text-[11px] tabular-nums text-slate-500"
                    title="Editor font size"
                  >
                    {fontSize}
                  </span>
                  <button
                    onClick={() => bumpFont(1)}
                    disabled={fontSize >= FONT_MAX}
                    className="flex h-9 items-center rounded-r-lg px-2.5 text-sm text-slate-300 hover:bg-edge disabled:opacity-30"
                    title="Larger text"
                    aria-label="Increase font size"
                  >
                    A<span className="text-[11px]">+</span>
                  </button>
                </div>

                {isMd && (
                  <button
                    onClick={() => setReading((v) => !v)}
                    className={`${ghostBtn} ${reading ? "bg-panel text-accent" : ""}`}
                    title="Toggle reading view"
                    aria-label={reading ? "Switch to edit view" : "Switch to reading view"}
                  >
                    {reading ? <PencilIcon className="h-[17px] w-[17px]" /> : <BookIcon className="h-[18px] w-[18px]" />}
                  </button>
                )}

                {/* Auto-save toggle — on by default. Off = manual ⌘/Ctrl+S. */}
                <button
                  onClick={() => setLiveSave((v) => !v)}
                  className="flex h-9 shrink-0 items-center gap-1.5 rounded-lg px-2.5 text-xs font-medium ring-1 ring-inset ring-edge transition-colors hover:bg-edge"
                  title={
                    liveSave
                      ? "Auto-save on — changes save as you type. Click for manual save (⌘/Ctrl+S)."
                      : "Manual save — press ⌘/Ctrl+S. Click to turn auto-save on."
                  }
                  aria-label="Toggle auto-save"
                >
                  <span
                    className={`h-1.5 w-1.5 rounded-full ${liveSave ? "bg-accent" : "ring-1 ring-slate-500"}`}
                    aria-hidden="true"
                  />
                  <span className={liveSave ? "text-accent" : "text-slate-400"}>
                    {liveSave ? "Auto" : "Manual"}
                  </span>
                </button>
              </>
            )}

            {status === "ready" && activePath && (
              <button
                onClick={() => setConfirmDelete(true)}
                className={`${ghostBtn} hover:bg-red-500/10 hover:text-red-400 ${
                  confirmDelete ? "bg-red-500/10 text-red-400" : ""
                }`}
                title="Delete file"
                aria-label="Delete file"
              >
                <TrashIcon className="h-[18px] w-[18px]" />
              </button>
            )}

            <button
              onClick={() => {
                setNewOpen((v) => !v);
                setNewName("");
              }}
              className={`${ghostBtn} ${newOpen ? "bg-panel text-accent" : ""}`}
              title="New file"
              aria-label="New file"
            >
              <FilePlusIcon className="h-[18px] w-[18px]" />
            </button>

            {status === "ready" && !liveSave && (
              <button
                onClick={() => void doSave()}
                disabled={!dirty || saving}
                className="ml-0.5 flex h-9 shrink-0 items-center rounded-lg bg-accent px-3.5 text-sm font-semibold text-bar transition-opacity hover:opacity-90 disabled:bg-panel disabled:text-slate-500"
                title="Save (⌘/Ctrl+S)"
              >
                {saveLabel}
              </button>
            )}
          </>
        ) : (
          <>
            {status === "ready" && isMd && (
              <button
                onClick={() => setReading((v) => !v)}
                className={`${ghostBtn} ${reading ? "bg-panel text-accent" : ""}`}
                title="Toggle reading view"
                aria-label={reading ? "Switch to edit view" : "Switch to reading view"}
              >
                {reading ? <PencilIcon className="h-[17px] w-[17px]" /> : <BookIcon className="h-[18px] w-[18px]" />}
              </button>
            )}

            {status === "ready" && !liveSave && (
              <button
                onClick={() => void doSave()}
                disabled={!dirty || saving}
                className="flex h-9 shrink-0 items-center rounded-lg bg-accent px-3.5 text-sm font-semibold text-bar transition-opacity hover:opacity-90 disabled:bg-panel disabled:text-slate-500"
                title="Save (⌘/Ctrl+S)"
              >
                {saveLabel}
              </button>
            )}

            {status === "ready" && (
              <div className="relative shrink-0">
                <button
                  onClick={() => setMenuOpen((v) => !v)}
                  className={`${ghostBtn} ${menuOpen ? "bg-panel text-accent" : ""}`}
                  title="More actions"
                  aria-label="More actions"
                  aria-haspopup="menu"
                  aria-expanded={menuOpen}
                >
                  <MoreIcon className="h-[18px] w-[18px]" />
                </button>

                {menuOpen && (
                  <>
                    {/* Outside-tap catcher — dismisses the menu. */}
                    <div
                      className="fixed inset-0 z-[65]"
                      onClick={() => setMenuOpen(false)}
                      aria-hidden="true"
                    />
                    <div
                      className="absolute right-0 top-full z-[70] mt-2 w-56 overflow-hidden rounded-xl border border-edge bg-bar shadow-xl"
                      role="menu"
                    >
                      {/* Text size */}
                      <div className="flex items-center justify-between gap-2 px-3 py-2.5">
                        <span className="text-xs text-slate-400">Text size</span>
                        <div className="flex items-center rounded-lg bg-panel/70 ring-1 ring-inset ring-edge">
                          <button
                            onClick={() => bumpFont(-1)}
                            disabled={fontSize <= FONT_MIN}
                            className="flex h-8 items-center rounded-l-lg px-2.5 text-xs text-slate-300 hover:bg-edge disabled:opacity-30"
                            aria-label="Decrease font size"
                          >
                            A<span className="text-[9px]">−</span>
                          </button>
                          <span className="min-w-[2ch] text-center text-[11px] tabular-nums text-slate-500">
                            {fontSize}
                          </span>
                          <button
                            onClick={() => bumpFont(1)}
                            disabled={fontSize >= FONT_MAX}
                            className="flex h-8 items-center rounded-r-lg px-2.5 text-sm text-slate-300 hover:bg-edge disabled:opacity-30"
                            aria-label="Increase font size"
                          >
                            A<span className="text-[11px]">+</span>
                          </button>
                        </div>
                      </div>

                      {/* Auto-save toggle */}
                      <button
                        onClick={() => setLiveSave((v) => !v)}
                        className="flex w-full items-center justify-between gap-2 border-t border-edge/60 px-3 py-3 text-sm text-slate-200 active:bg-panel"
                        role="menuitemcheckbox"
                        aria-checked={liveSave}
                      >
                        <span className="flex items-center gap-2">
                          <span
                            className={`h-1.5 w-1.5 rounded-full ${liveSave ? "bg-accent" : "ring-1 ring-slate-500"}`}
                            aria-hidden="true"
                          />
                          Auto-save
                        </span>
                        <span className={`text-xs font-medium ${liveSave ? "text-accent" : "text-slate-500"}`}>
                          {liveSave ? "On" : "Off"}
                        </span>
                      </button>

                      {/* New file */}
                      <button
                        onClick={() => {
                          setMenuOpen(false);
                          setNewName("");
                          setNewOpen(true);
                        }}
                        className="flex w-full items-center gap-2 border-t border-edge/60 px-3 py-3 text-sm text-slate-200 active:bg-panel"
                        role="menuitem"
                      >
                        <FilePlusIcon className="h-[17px] w-[17px]" />
                        New file
                      </button>

                      {/* Delete */}
                      {activePath && (
                        <button
                          onClick={() => {
                            setMenuOpen(false);
                            setConfirmDelete(true);
                          }}
                          className="flex w-full items-center gap-2 border-t border-edge/60 px-3 py-3 text-sm text-red-400 active:bg-red-500/10"
                          role="menuitem"
                        >
                          <TrashIcon className="h-[17px] w-[17px]" />
                          Delete file
                        </button>
                      )}
                    </div>
                  </>
                )}
              </div>
            )}
          </>
        )}
      </div>

      {newOpen && (
        <div className="flex items-center gap-2 border-b border-edge bg-panel/40 px-3 py-2">
          <span className="shrink-0 text-xs text-slate-500">New file in</span>
          <code className="max-w-[40%] truncate text-xs text-slate-400">
            {(activePath ? dirname(activePath) : tree.projectPath || tree.sharePath || tree.homePath) || "?"}
          </code>
          <input
            autoFocus
            value={newName}
            onChange={(e) => setNewName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") void createNew();
            }}
            placeholder="notes.md"
            className="min-w-0 flex-1 rounded-md border border-edge bg-bar px-2.5 py-1.5 text-sm text-slate-100 outline-none focus:border-accent"
          />
          <button
            onClick={() => void createNew()}
            className="shrink-0 rounded-md bg-accent px-3 py-1.5 text-sm font-semibold text-bar active:opacity-80"
          >
            Create
          </button>
        </div>
      )}

      {confirmDelete && (
        <div className="flex items-center gap-3 border-b border-edge bg-red-500/10 px-3 py-2 text-sm text-red-300">
          <span className="min-w-0 flex-1 truncate">
            Delete <span className="font-semibold text-red-200">{name}</span>? This can’t be undone.
          </span>
          <button
            onClick={() => setConfirmDelete(false)}
            className="shrink-0 rounded-md bg-panel px-3 py-1.5 text-slate-200 hover:bg-edge"
          >
            Cancel
          </button>
          <button
            onClick={() => void doDelete()}
            disabled={deleting}
            className="shrink-0 rounded-md bg-red-500 px-3 py-1.5 font-semibold text-white hover:bg-red-600 disabled:opacity-50"
          >
            {deleting ? "Deleting…" : "Delete"}
          </button>
        </div>
      )}

      {conflict && (
        <div className="flex items-center gap-3 border-b border-edge bg-amber/10 px-3 py-2 text-sm text-amber">
          <span className="flex-1">File changed on disk since you opened it.</span>
          <button onClick={reload} className="rounded bg-panel px-2.5 py-1 text-slate-200 active:bg-edge">
            Reload
          </button>
          <button
            onClick={() => void doSave(true)}
            className="rounded bg-panel px-2.5 py-1 text-slate-200 active:bg-edge"
          >
            Overwrite
          </button>
        </div>
      )}

      {error && (
        <div className="border-b border-edge/40 bg-red-500/5 px-3 py-2 text-sm text-red-400">{error}</div>
      )}

      {/* ── Body: optional tree (desktop) + writing surface ───────────────── */}
      <div className="flex min-h-0 flex-1">
        {isDesktop && treeOpen && (
          <div
            ref={treeRef}
            onPointerDownCapture={releaseControl}
            className="relative shrink-0"
            style={{ width: `${treeWidth}px` }}
          >
            <EditorTree
              tree={tree}
              onOpenFile={setActivePath}
              onDownload={onDownload}
              downloadingPath={downloading}
              onContextMenu={onTreeContextMenu}
              onDropFiles={onTreeDropFiles}
              activePath={activePath}
            />
            {/* Resize splitter — straddles the tree's right border. The hit area
                (w-2) is wider than the visible hairline for an easy grab; the
                line lights up on hover and stays lit while dragging. */}
            <div
              onPointerDown={startTreeResize}
              onDoubleClick={() => setTreeWidth(TREE_W_DEFAULT)}
              role="separator"
              aria-orientation="vertical"
              aria-label="Resize file tree (double-click to reset)"
              className="group absolute inset-y-0 -right-1 z-10 flex w-2 cursor-col-resize touch-none justify-center"
            >
              <span
                className={`w-px transition-colors ${
                  resizingTree ? "bg-accent" : "bg-transparent group-hover:bg-accent/60"
                }`}
              />
            </div>
          </div>
        )}
        {/* Center column: the writing surface + (when a file is open) the
            status bar. Wrapping these two means the footer only consumes height
            HERE — the tree and the agent mirror keep their full height, so
            opening a file never relayouts (and re-fits the font of) the agent
            column. */}
        <div className="flex min-w-0 flex-1 flex-col">
          <div
            onPointerDownCapture={releaseControl}
            className="min-h-0 flex-1 overflow-hidden"
            style={{ "--cc-editor-font": `${fontSize}px`, background: SURFACE_BG } as CSSProperties}
          >
          {status === "loading" && (
            <div className="flex h-full items-center justify-center text-sm text-slate-500">Loading…</div>
          )}
          {status === "empty" && (
            <EmptyState
              isDesktop={isDesktop}
              onNew={() => setNewOpen(true)}
              onBrowse={() => setTreePanelOpen(true)}
            />
          )}
          {status === "noteditable" && (
            <div className="flex h-full flex-col items-center justify-center gap-3 px-8 text-center text-sm text-slate-500">
              <FileEditIcon className="h-7 w-7 opacity-40" />
              <span>
                <span className="text-slate-300">{name}</span> isn’t a text file.
              </span>
              {activePath && (
                <button
                  onClick={() =>
                    onDownload({ name, path: activePath, size: 0, mtime: 0 })
                  }
                  disabled={downloading === activePath}
                  className="rounded-md border border-edge bg-bar px-3 py-1.5 text-xs text-slate-200 hover:text-slate-100 disabled:opacity-50"
                >
                  {downloading === activePath ? "Downloading…" : "Download to device"}
                </button>
              )}
            </div>
          )}
          {status === "pdf" && activePath && (
            <Suspense
              fallback={
                <div className="flex h-full items-center justify-center text-sm text-slate-500">
                  Loading PDF…
                </div>
              }
            >
              <PdfViewer path={activePath} name={name} />
            </Suspense>
          )}
          {status === "ready" &&
            (reading && isMd ? (
              <ReadingView content={content} />
            ) : (
              <MarkdownEditor
                value={content}
                onChange={setContent}
                filename={name}
                markdown={isMd}
                onSave={() => void doSave()}
              />
            ))}
          </div>

          {/* ── Status bar (lives in the center column, under the editor only,
              so it never shortens the tree or the agent mirror) ───────────── */}
          {showFooter && (
            <div className="flex items-center gap-3 border-t border-edge bg-bar/95 px-3 py-1 text-[11px] text-slate-500 pb-safe">
              <span className="min-w-0 flex-1 truncate font-mono">{relDir}</span>
              <span className="shrink-0 tabular-nums">
                {isMd
                  ? `${stats.words.toLocaleString()} ${stats.words === 1 ? "word" : "words"} · ${stats.mins} min read`
                  : `${stats.lines.toLocaleString()} ${stats.lines === 1 ? "line" : "lines"} · ${stats.chars.toLocaleString()} chars`}
              </span>
              <span
                className={`shrink-0 font-medium ${
                  saving ? "text-accent" : dirty ? "text-amber" : "text-slate-600"
                }`}
              >
                {saving ? "Saving…" : dirty ? "Unsaved" : "Saved"}
              </span>
            </div>
          )}
        </div>

        {/* ── Live agent mirror (desktop) ─────────────────────────────────
            A read-only (optionally interactive) view of the active session's
            agent. It NEVER reports its size — see AgentMirror — so showing it
            here can't shrink the width-locked PTY the grid pane is also using.
            Drag the left-edge splitter to resize (mirror of the tree). */}
        {isDesktop && agentOpen && (
          <div
            ref={agentRef}
            className="relative shrink-0"
            style={{ width: `${agentWidth}px` }}
          >
            <div
              onPointerDown={startAgentResize}
              onDoubleClick={() => setAgentRecalibrate((n) => n + 1)}
              role="separator"
              aria-orientation="vertical"
              aria-label="Resize agent view — drag to zoom, double-click to refit columns"
              className="group absolute inset-y-0 -left-1 z-10 flex w-2 cursor-col-resize touch-none justify-center"
            >
              <span
                className={`w-px transition-colors ${
                  resizingAgent ? "bg-accent" : "bg-transparent group-hover:bg-accent/60"
                }`}
              />
            </div>
            <AgentColumn
              session={session}
              cols={agentCols}
              rows={agentRows}
              fontSize={termFontSize}
              control={agentControl}
              conn={agentConn}
              recalibrate={agentRecalibrate}
              onToggleControl={() => setAgentControl((v) => !v)}
              onEngageControl={() => setAgentControl(true)}
              onConn={setAgentConn}
            />
          </div>
        )}
      </div>

      {/* ── Phone file tree ────────────────────────────────────────────────────
          The desktop sidebar's mobile twin, in two guises:
            • Home (no file open) — a full-screen browser you land on after
              closing/deleting a file. Its ✕ exits the overlay to the terminal.
            • Peek (a file open) — a near-full-width drawer over the editor with a
              dimmed sliver behind that taps to dismiss, for quick switching. Its
              ✕ just closes the drawer, back to the file.
          Tapping a file opens it and closes the panel either way. */}
      {!isDesktop && treePanelOpen && (
        <div className="absolute inset-0 z-[70] flex">
          <div className={`flex flex-col bg-bar shadow-2xl ${activePath ? "w-[86%] max-w-sm" : "w-full"}`}>
            <div className="flex items-center gap-2 border-b border-edge px-3 py-2.5 pt-safe">
              <span className="flex-1 text-base font-semibold text-slate-100">Files</span>
              <button
                onClick={() => {
                  setTreePanelOpen(false);
                  setNewName("");
                  setNewOpen(true);
                }}
                className={ghostBtn}
                title="New file"
                aria-label="New file"
              >
                <FilePlusIcon className="h-[18px] w-[18px]" />
              </button>
              <button
                onClick={() => (activePath ? setTreePanelOpen(false) : requestClose())}
                className={ghostBtn}
                title={activePath ? "Close files" : "Close editor"}
                aria-label={activePath ? "Close files" : "Close editor"}
              >
                <XIcon className="h-[18px] w-[18px]" />
              </button>
            </div>
            <div className="min-h-0 flex-1">
              <EditorTree
                tree={tree}
                activePath={activePath}
                touch
                onDownload={onDownload}
                downloadingPath={downloading}
                onContextMenu={onTreeContextMenu}
                onDropFiles={onTreeDropFiles}
                onOpenFile={(p) => {
                  setActivePath(p);
                  setTreePanelOpen(false);
                }}
              />
            </div>
          </div>
          {activePath && (
            <div
              className="flex-1 bg-black/50"
              onClick={() => setTreePanelOpen(false)}
              aria-hidden="true"
            />
          )}
        </div>
      )}

      {/* Drag-and-drop upload progress/result toast. A drop on a tree folder
          uploads straight into it (no sheet); this pill reports progress and
          the outcome, then auto-dismisses. */}
      {uploadStatus && (
        <div
          role="status"
          aria-live="polite"
          className="pointer-events-none absolute bottom-6 left-1/2 z-[80] flex max-w-[90%] -translate-x-1/2 items-center gap-2.5 rounded-full border border-edge bg-bar/95 px-4 py-2 text-sm shadow-xl backdrop-blur"
        >
          {!uploadStatus.done ? (
            <>
              <span className="h-3.5 w-3.5 shrink-0 animate-spin rounded-full border-2 border-edge border-t-accent" aria-hidden="true" />
              <span className="truncate text-slate-200">
                Uploading {uploadStatus.total} file{uploadStatus.total === 1 ? "" : "s"} to{" "}
                <span className="font-medium text-slate-100">{basename(uploadStatus.dir)}</span> ·{" "}
                {Math.round(uploadStatus.progress * 100)}%
              </span>
            </>
          ) : uploadStatus.errors > 0 ? (
            <span className="truncate text-amber">
              Uploaded {uploadStatus.total - uploadStatus.errors} of {uploadStatus.total} — {uploadStatus.errors} failed
            </span>
          ) : (
            <span className="truncate text-emerald-400">
              ✓ Uploaded {uploadStatus.total} file{uploadStatus.total === 1 ? "" : "s"} to {basename(uploadStatus.dir)}
            </span>
          )}
        </div>
      )}

      {/* File-tree CRUD menu (right-click / long-press). Renders fixed at the
          cursor, above everything. */}
      {ctx && (
        <ContextMenu
          x={ctx.x}
          y={ctx.y}
          target={ctx.target}
          onClose={() => setCtx(null)}
          onOpenFile={(p) => {
            setActivePath(p);
            setTreePanelOpen(false); // phone: reveal the file behind the tree
          }}
          onDownload={(p, n) => void onDownload({ name: n, path: p, size: 0, mtime: 0 })}
          onNewFile={ctxNewFile}
          onNewFolder={ctxNewFolder}
          onRename={ctxRename}
          onDeleteFile={ctxDeleteFile}
          onDeleteFolder={ctxDeleteFolder}
        />
      )}
    </div>
  );
}

// EmptyState — a tasteful placeholder for the writing surface before a file is
// open instead of a bare line of grey text. Desktop points at the sidebar tree;
// phone points at the slide-over Files panel and offers a new-note shortcut.
function EmptyState({
  isDesktop,
  onNew,
  onBrowse,
}: {
  isDesktop: boolean;
  onNew: () => void;
  onBrowse: () => void;
}) {
  return (
    <div className="flex h-full flex-col items-center justify-center gap-4 px-8 text-center">
      <FileEditIcon className="h-10 w-10 text-edge" />
      <div className="space-y-1">
        <p className="text-sm text-slate-300">
          {isDesktop ? "Open a file to start writing" : "No file open"}
        </p>
        <p className="text-xs text-slate-500">
          {isDesktop
            ? "Pick one from the tree on the left, or create a new note."
            : "Tap Files to browse, or start a new note."}
        </p>
      </div>
      {isDesktop ? (
        <button
          onClick={onNew}
          className="flex items-center gap-1.5 rounded-lg bg-panel px-3 py-2 text-sm text-slate-200 ring-1 ring-inset ring-edge hover:bg-edge"
        >
          <FilePlusIcon className="h-4 w-4" /> New note
        </button>
      ) : (
        <div className="flex items-center gap-2">
          <button
            onClick={onBrowse}
            className="flex items-center gap-1.5 rounded-lg bg-panel px-3 py-2 text-sm text-slate-200 ring-1 ring-inset ring-edge active:bg-edge"
          >
            <SidebarIcon className="h-4 w-4" /> Browse files
          </button>
          <button
            onClick={onNew}
            className="flex items-center gap-1.5 rounded-lg bg-panel px-3 py-2 text-sm text-slate-200 ring-1 ring-inset ring-edge active:bg-edge"
          >
            <FilePlusIcon className="h-4 w-4" /> New note
          </button>
        </div>
      )}
    </div>
  );
}

// AgentColumn — the editor's right-hand live-agent column: a thin header (the
// session, a connection dot, and the phase-2 "take control" keyboard toggle)
// over the AgentMirror. When control is engaged the column border accents and
// keystrokes are forwarded to the agent; clicking back into the editor or tree
// releases it (see releaseControl). No session → a quiet placeholder.
function AgentColumn({
  session,
  cols,
  rows,
  fontSize,
  control,
  conn,
  recalibrate,
  onToggleControl,
  onEngageControl,
  onConn,
}: {
  session: string | null;
  cols: number;
  rows: number;
  fontSize: number;
  control: boolean;
  conn: ConnState;
  recalibrate: number;
  onToggleControl: () => void;
  onEngageControl: () => void;
  onConn: (c: ConnState) => void;
}) {
  const dot =
    conn === "open" ? "bg-emerald-400" : conn === "connecting" ? "bg-amber" : "bg-red-500";
  return (
    <div className={`flex h-full flex-col border-l bg-bar ${control ? "border-accent" : "border-edge"}`}>
      <div className="flex h-9 shrink-0 items-center gap-2 border-b border-edge px-2.5">
        <span className="text-[10px] font-semibold uppercase tracking-[0.14em] text-slate-600">
          Agent
        </span>
        {session ? (
          <>
            <span className={`h-1.5 w-1.5 shrink-0 rounded-full ${dot}`} aria-hidden="true" />
            <span className="min-w-0 flex-1 truncate text-xs text-slate-300">{session}</span>
            <button
              onClick={onToggleControl}
              className={`flex h-7 shrink-0 items-center gap-1 rounded-md px-2 text-[11px] font-medium ring-1 ring-inset transition-colors ${
                control
                  ? "bg-accent/15 text-accent ring-accent/60"
                  : "text-slate-400 ring-edge hover:bg-panel hover:text-slate-200"
              }`}
              title={
                control
                  ? "Typing goes to the agent — click (or click the editor) to release"
                  : "Take control — forward keystrokes to the agent"
              }
              aria-pressed={control}
            >
              <KeyboardIcon className="h-3.5 w-3.5" />
              {control ? "Live" : "Control"}
            </button>
          </>
        ) : (
          <span className="min-w-0 flex-1 truncate text-xs text-slate-600">No active session</span>
        )}
      </div>
      {/* Click anywhere in the terminal area to engage control (→ "Live");
          clicking the editor or tree releases it (see releaseControl). Capture
          phase so it registers before xterm focuses its textarea. */}
      <div
        className="min-h-0 flex-1"
        onPointerDownCapture={session ? onEngageControl : undefined}
      >
        {session ? (
          <AgentMirror
            key={session}
            session={session}
            cols={cols}
            rows={rows}
            maxFontSize={fontSize}
            control={control}
            recalibrateSignal={recalibrate}
            onState={onConn}
          />
        ) : (
          <div className="flex h-full flex-col items-center justify-center gap-2 px-6 text-center text-xs text-slate-600">
            <TerminalIcon className="h-6 w-6 opacity-40" />
            <span>Focus a terminal pane to mirror its agent here.</span>
          </div>
        )}
      </div>
    </div>
  );
}

// CodeBlock overrides react-markdown's <pre> for fenced (```) code blocks,
// floating a copy button over it. We read the rendered text off the <pre>
// via a ref (innerText) instead of walking the markdown AST, so it copies
// exactly what's shown regardless of nested syntax nodes. writeClipboard
// handles the HTTPS (async clipboard) vs plain-HTTP (execCommand) split, so
// copying works on the tailnet's http:// deployment too. Inline `code` is
// untouched — only fenced blocks render through <pre>.
function CodeBlock({ children }: { children?: ReactNode }) {
  const ref = useRef<HTMLPreElement>(null);
  const [copied, setCopied] = useState(false);
  const onCopy = useCallback(() => {
    const text = ref.current?.innerText ?? "";
    if (!text) return;
    writeClipboard(text)
      .then(() => {
        setCopied(true);
        window.setTimeout(() => setCopied(false), 1200);
      })
      .catch(() => {});
  }, []);
  return (
    <div className="cc-codeblock">
      <button type="button" className="cc-copy-btn" onClick={onCopy} aria-label="Copy code">
        {copied ? "Copied" : "Copy"}
      </button>
      <pre ref={ref}>{children}</pre>
    </div>
  );
}

const MARKDOWN_COMPONENTS = { pre: CodeBlock };

// ReadingView renders the markdown fully (Obsidian's "reading mode"). It shares
// the writing surface's centered measure so toggling Edit<->Read doesn't shift
// the text column.
function ReadingView({ content }: { content: string }) {
  return (
    <div className="h-full overflow-y-auto px-6 py-10">
      <div className="cc-prose mx-auto" style={{ maxWidth: "var(--cc-measure, 44rem)" }}>
        <ReactMarkdown remarkPlugins={[remarkGfm]} components={MARKDOWN_COMPONENTS}>
          {content}
        </ReactMarkdown>
      </div>
    </div>
  );
}
