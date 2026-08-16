import { lazy, Suspense, useCallback, useEffect, useMemo, useRef, useState, type ChangeEvent } from "react";
import type { Terminal } from "@xterm/xterm";
import {
  clearHistory,
  deleteSession,
  fetchFavorites,
  fetchFiles,
  fetchRestorable,
  fetchSessions,
  flattenDataTransfer,
  getAuthStatus,
  imageSendError,
  pasteText,
  restoreSessions,
  saveFavorites,
  sendImage,
  sendKey,
  setSessionColor,
  setSessionLabel,
  setUnauthorizedHandler,
  type Favorite,
  type MachineInfo,
  type PaneRef,
  type RestorableSession,
  type Session,
  type UploadFile,
  type UploadResult,
} from "./api";
import { fetchMachines } from "./api";
import { getMe, type MeInfo } from "./api";
import {
  applyLayout,
  cycleSessionInPane,
  LAST_KEY,
  loadPaneState,
  PANES_KEY,
  type PaneState,
} from "./paneState";
import { SessionRecentsStore, type RecentRef } from "./sessionRecents";
import TerminalView, { type ConnState } from "./components/TerminalView";
import SessionDrawer, { type PaneSwitcherProps } from "./components/SessionDrawer";
import ControlBar from "./components/ControlBar";
import ComposeSheet, { type ComposeHandle } from "./components/ComposeSheet";
import ImageSheet from "./components/ImageSheet";
import FavoritesSheet, { type FavoritesHandle } from "./components/FavoritesSheet";
import StatusView from "./components/StatusView";
import TileGrid, { type Layout, paneCount } from "./components/TileGrid";
import LayoutPicker from "./components/LayoutPicker";
import LayoutPalette from "./components/LayoutPalette";
import UploadSheet from "./components/UploadSheet";
import LoginScreen from "./components/LoginScreen";
import { AuthScreen, ActivatePage, Dashboard, InviteLanding } from "./components/MultiTenant";
import ToastHost, { type ToastHostHandle } from "./components/ToastHost";
import ClipboardOfferHost from "./components/ClipboardOfferHost";
import { noteUserCopy } from "./osc52Bus";
import InboxButton from "./components/InboxButton";
import UpdateAssistants from "./components/UpdateAssistants";
import ShareForm, { type ShareSubject } from "./components/ShareForm";
import { detectReadyEdges, sessionKey } from "./readyEdges";
import {
  fileLinkPath,
  joinHome,
  parseFileLink,
  parseLinkToken,
  relFromHome,
  type EditorLocation,
} from "./fileLink";
// The editor pulls in CodeMirror + react-markdown — a big chunk only needed
// once the user actually opens a file. Lazy-load it so the terminal app's
// initial bundle stays light.
const EditorOverlay = lazy(() => import("./components/EditorOverlay"));
// The read-only link-grant page (proposal 0083 Part C). Its own chunk: it pulls
// the markdown reading view + CodeMirror, and an ordinary app load must never
// pay for a page only anonymous bearers see.
const LinkView = lazy(() => import("./components/LinkView"));
import { agentStatus, buildSharedMap, displayName, nextSessionColor, sameJson, sessionAccent, shouldSkipShortcut, statusDot, statusTitle, toolColor, toPng, writeClipboard } from "./util";
import { HIDDEN_SESSIONS_MS, usePoll } from "./poll";
import { setPrefixArmed } from "./prefix";
import { listReceivedShares, type ReceivedShare } from "./api";
import { DownloadIcon, EraserIcon, FileEditIcon, ImageIcon, PencilIcon, RefreshIcon, SearchIcon, ServerIcon, StarIcon, StatusListIcon, UploadIcon } from "./icons";

// A Google sign-in that could not be completed comes back as
// `/?login_error=<reason>` (crates/hub/src/oauth.rs) rather than a dead-end error
// page — the window the user lands in is often the installed app itself. Read it
// once at load and strip it from the URL immediately, so a restored or reloaded
// window doesn't keep re-asserting a failure that already happened.
const LOGIN_ERROR: string | null = (() => {
  if (typeof window === "undefined") return null;
  const url = new URL(window.location.href);
  const reason = url.searchParams.get("login_error");
  if (!reason) return null;
  url.searchParams.delete("login_error");
  window.history.replaceState({}, "", url.pathname + url.search + url.hash);
  return reason;
})();
const LOGIN_ERROR_HINT: Record<string, string> = {
  expired: "That sign-in link had already been used or expired — try once more.",
  state: "The sign-in couldn't be verified — try once more.",
  denied: "Google sign-in was cancelled.",
  google: "Google couldn't complete the sign-in — try once more.",
  unverified: "That Google account's email isn't verified.",
  account: "Signed in with Google, but the account couldn't be set up.",
};

// Proposal 0083 Part A — the file deep link, read ONCE at load (like
// LOGIN_ERROR above) and consumed by an effect after auth resolves. Read at
// module scope so the consumption never races a re-render, and deliberately
// NOT stripped from the URL on success: Part B's urlSync owns the address bar
// from that point on, and the URL *is* the feature.
const FILE_LINK = typeof window === "undefined" ? null : parseFileLink(window.location.pathname);
// Proposal 0083 Part C — `/s/<token>`, the read-only link grant page. Its
// viewer has no account, so this route renders BEFORE the auth gate.
const LINK_TOKEN = typeof window === "undefined" ? null : parseLinkToken(window.location.pathname);

const FONT_KEY = "ccweb.fontSize";
// In-app session-ready toasts (proposal 0017) on/off, persisted. Defaults ON
// (the proposal ships always-on; the gated, foreground-only edge keeps it
// quiet) — only an explicit "0" disables it.
const TOASTS_KEY = "ccweb.toasts.v1";
const loadToastsEnabled = (): boolean => {
  try {
    return localStorage.getItem(TOASTS_KEY) !== "0";
  } catch {
    return true;
  }
};
// One-shot "how to select" hint. `.v2` because the v1 wording said
// "Shift+drag" universally — wrong on Mac, where the modifier is Option.
// Bumping the key re-shows the corrected hint to users who already
// dismissed v1.
const COPY_HINT_KEY = "ccweb.copyHintSeen.v2";

// Whether the file-viewer overlay (vs the agent/terminal grid) was the active
// view, persisted so a page reload comes back in the same mode (proposal 0019
// follow-up). Only the open/closed *mode* is stored — never the path: the file
// the viewer should reopen on is restored from the per-session viewer memory
// (viewerState.ts, keyed by machine+session), which avoids reopening a stale
// path the user had since navigated away from in the tree.
const EDITOR_OPEN_KEY = "ccweb.editorOpen.v1";
const loadEditorOpen = (): boolean => {
  try {
    return localStorage.getItem(EDITOR_OPEN_KEY) === "1";
  } catch {
    return false;
  }
};

// useIsDesktop is true on a wide window with a precise pointer (mouse/trackpad
// — Chrome desktop). The multi-pane UI is gated on this; phones always render
// a single pane and never see the layout picker.
function useIsDesktop(): boolean {
  const query = "(pointer: fine) and (min-width: 900px)";
  const get = () => typeof matchMedia !== "undefined" && matchMedia(query).matches;
  const [d, setD] = useState<boolean>(get);
  useEffect(() => {
    const mq = matchMedia(query);
    const on = () => setD(mq.matches);
    mq.addEventListener("change", on);
    return () => mq.removeEventListener("change", on);
  }, []);
  return d;
}

// isCtrlB matches the bare Ctrl+B chord — no Shift/Alt/Meta, case-insensitive.
function isCtrlB(e: KeyboardEvent): boolean {
  return (
    e.ctrlKey &&
    !e.shiftKey &&
    !e.altKey &&
    !e.metaKey &&
    e.key.toLowerCase() === "b"
  );
}

// A keydown from a modifier key alone. These must NEVER be treated as "the
// chord key" — they arrive *before* the character on any layout where the chord
// needs a modifier to type. On a Swedish keyboard `/` is Shift+7, so ⌃B then
// Shift+7 delivers keydown "Shift" first; without this guard that unrecognised
// key cancelled the prefix and the `/` that followed went to the terminal. Same
// for ⌃B S (share) and ⌃B ⇧C (clear colour) on every layout.
function isModifierKey(k: string): boolean {
  return (
    k === "Shift" || k === "Control" || k === "Alt" || k === "Meta" ||
    k === "AltGraph" || k === "CapsLock"
  );
}

// shouldSkipShortcut — "is focus in a real text field, so the ⌃B prefix must
// stay out of the way?" — lives in util.ts, next to the other pure helpers, so
// its table of cases can be unit-tested without dragging the whole app into a
// test's import graph. It exempts the [0026] empty-pane filter alongside
// xterm's helper textarea (0081 Part C); see the comment there.

export default function App() {
  const isDesktop = useIsDesktop();

  const [sessions, setSessions] = useState<Session[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Auth gate (opt-in server-side). null = still checking; false = show the
  // login screen; true = authed (or auth is off). The session cookie rides all
  // requests automatically, so the rest of the app is unchanged.
  const [authed, setAuthed] = useState<boolean | null>(null);
  // Multi-tenant boot info (proposal 0001). null on a single-tenant agent/hub or
  // before the first /api/me. Drives the email/Google login, /activate, and the
  // machines dashboard; single-tenant keeps the shared-secret LoginScreen.
  const [me, setMe] = useState<MeInfo | null>(null);
  const [showDash, setShowDash] = useState(false);
  // Checkout return (proposal 0058 C4): null = not returning from Stripe;
  // "pending" = webhook not landed yet, plan card shows "activating…"; "slow" =
  // past the 30s budget, degrade to the honest "your payment is safe" copy.
  const [billingPending, setBillingPending] = useState<"pending" | "slow" | null>(null);
  // A one-shot seed for the drawer's create mode (proposal 0056 A1/A2): set by
  // "Start your first session" (/activate) and a machine row's "New session",
  // consumed by SessionDrawer, which opens straight into create pre-scoped to
  // that machine.
  const [createSeed, setCreateSeed] = useState<string | null>(null);
  // Sharing (proposal 0041). The active shares granted TO me drive the
  // shared-vs-owned badges; `shareTarget` (when set) opens the ShareForm overlay
  // for one session. Multi-tenant only — empty/idle otherwise.
  const [receivedShares, setReceivedShares] = useState<ReceivedShare[]>([]);
  const [shareTarget, setShareTarget] = useState<ShareSubject | null>(null);
  const sharedMap = useMemo(() => buildSharedMap(receivedShares), [receivedShares]);
  // Sessions a reboot/tmux restart took down that we can bring back. Fetched
  // lazily when the drawer opens (it's the only place the offer is shown), so
  // the session-list poll stays a single request.
  const [restorable, setRestorable] = useState<RestorableSession[]>([]);
  // The hub's machine roster (id, hostname, online). Empty when talking to a
  // standalone agent (no /api/machines) — i.e. "no hub, single machine". We
  // group the session list / show machine pickers only when >1 machine, so a
  // single-box deployment looks exactly as before.
  const [machines, setMachines] = useState<MachineInfo[]>([]);
  const multiMachine = machines.length > 1;
  // Default machine for session-less surfaces (New Session, standalone editor
  // browse) when no pane gives one: the first online agent, else "".
  const firstOnlineMachine = machines.find((m) => m.online)?.machine ?? "";
  // The machines-dashboard button's status dot (proposal 0043): amber when ≥1
  // machine is online, hollow otherwise. Preserves the old floating pill's cue.
  const anyMachineOnline = machines.some((m) => m.online);

  // The whole multi-pane state lives in one object; persisted as one blob.
  const [paneState, setPaneState] = useState<PaneState>(loadPaneState);
  const { layout, panes, active } = paneState;
  const currentSession = panes[active] ?? null;
  // The set of sessions currently on screen in any pane (sessionKey()s) — the
  // toast host uses it to never toast (and to retract) a mounted session.
  const mountedKeys = useMemo(
    () => new Set(panes.filter((p): p is PaneRef => p != null).map(sessionKey)),
    [panes]
  );

  // Most-recently-focused sessions (proposal 0078) — the `Recent` section at the
  // top of the switcher. The store owns the dwell gate, the trailing-debounced
  // persist and the read-modify-write against localStorage; App owns only the
  // two write edges (focus, background mount) and the render copy.
  const [recentsStore] = useState(() => new SessionRecentsStore());
  const [recents, setRecents] = useState<RecentRef[]>(() => recentsStore.list());
  useEffect(() => {
    const unsub = recentsStore.subscribe(() => setRecents(recentsStore.list()));
    // A second tab on this origin shares the store; adopt its writes rather than
    // rendering a divergent section (0078 A11).
    const onStorage = () => recentsStore.reload();
    // Flush before the tab goes away — the in-memory list is authoritative and
    // the debounce window may still be open.
    const onHide = () => recentsStore.flush();
    window.addEventListener("storage", onStorage);
    window.addEventListener("pagehide", onHide);
    return () => {
      unsub();
      window.removeEventListener("storage", onStorage);
      window.removeEventListener("pagehide", onHide);
      recentsStore.dispose();
    };
  }, [recentsStore]);

  // Mirror layout/active/sessions/panes into refs so the keyboard handler can
  // read fresh values without re-binding (which would reset its in-flight
  // prefix timer mid-chord). State drives rendering; refs drive the handler.
  const layoutRef = useRef(layout);
  const activeRef = useRef(active);
  const sessionsRef = useRef<Session[]>([]);
  const panesRef = useRef<(PaneRef | null)[]>(panes);
  // Live `me` for the keydown handler (proposal 0041 ⌃B S gate) — a ref so the
  // stable handler reads the current multi-tenant flag without re-registering.
  const meRef = useRef<MeInfo | null>(null);
  // paneRefFor builds a pane identity for a session *name*, resolving its owning
  // machine from the current session list (machine "" when unknown / single
  // agent). Used by call sites that only have a name (keyboard cycle).
  const paneRefFor = useCallback(
    (name: string): PaneRef => ({
      name,
      machine: sessionsRef.current.find((s) => s.name === name)?.machine ?? "",
    }),
    []
  );
  // markColor sets (or clears, with null) a session's mark colour (proposal
  // 0029). Optimistic: update local state immediately so the border/swatch flips
  // at once, then POST; the next /api/sessions poll reconciles either way. Routed
  // to the owning agent (direct or via the hub) by the ref's machine.
  const markColor = useCallback((ref: PaneRef | null, color: string | null) => {
    if (!ref) return;
    setSessions((prev) =>
      prev.map((s) =>
        s.name === ref.name && (s.machine ?? "") === ref.machine
          ? { ...s, color: color ?? undefined }
          : s
      )
    );
    // The optimistic edit diverges from the last-applied poll payload, so drop
    // the equality cache — otherwise an unchanged server list would be skipped
    // and a rejected POST would never self-heal (proposal 0068 Part C).
    sessionsKeyRef.current = null;
    setSessionColor(ref.name, color, ref.machine).catch(() => {});
  }, []);
  // renameSession sets (or clears, with null/empty) a session's display label
  // (proposal 0035). Same optimistic shape as markColor: flip local state at once
  // so the rename feels instant, then POST; the next /api/sessions poll
  // reconciles (and self-heals if the server rejects, e.g. too long). Identity
  // (`name`/`short`) is never touched. Routed to the owning agent by the ref.
  const renameSession = useCallback((ref: PaneRef | null, label: string | null) => {
    if (!ref) return;
    const next = label?.trim() || null;
    setSessions((prev) =>
      prev.map((s) =>
        s.name === ref.name && (s.machine ?? "") === ref.machine
          ? { ...s, label: next ?? undefined }
          : s
      )
    );
    sessionsKeyRef.current = null; // see markColor
    setSessionLabel(ref.name, next, ref.machine).catch(() => {});
  }, []);
  // Live xterm.js instance per pane, populated by TerminalView's onTerm
  // callback. The global Cmd/Ctrl+C handler reads the active slot to decide
  // whether there's a selection to copy. Length 4 matches the max layout.
  const termsRef = useRef<(Terminal | null)[]>([null, null, null, null]);
  useEffect(() => { layoutRef.current = layout; }, [layout]);
  useEffect(() => { activeRef.current = active; }, [active]);
  useEffect(() => { panesRef.current = panes; }, [panes]);
  useEffect(() => { meRef.current = me; }, [me]);

  // The two write edges of the Recent section (proposal 0078 A2). "Viewed" means
  // *focused*: in a 2×2 grid four sessions are visible, and only the focused one
  // is being worked in. Recording focus (not just mounts) is what makes the
  // order right after the panes change — collapsing a 2×2 unmounts three
  // sessions at once, and they must fall into last-focused order.
  const focusKey = currentSession ? sessionKey(currentSession) : "";
  useEffect(() => {
    const p = panesRef.current[activeRef.current] ?? null;
    // Dwell-gated inside the store: passing through panes (⌃B ↑/↓) writes
    // nothing, so the cycle can't rewrite the list it walks.
    recentsStore.focus(p ? { name: p.name, machine: p.machine } : null);
  }, [focusKey, recentsStore]);

  // A mount that lands in a *background* pane (a deep link, a notification tap,
  // a pick into another tile) is a real view event — recorded at once, but never
  // promoted above the focused pane's session.
  const prevPanes = useRef<(PaneRef | null)[]>(panes);
  useEffect(() => {
    const before = prevPanes.current;
    prevPanes.current = panes;
    panes.forEach((p, i) => {
      if (!p || i === active) return;
      const was = before[i];
      if (was && was.name === p.name && was.machine === p.machine) return;
      recentsStore.record({ name: p.name, machine: p.machine });
    });
  }, [panes, active, recentsStore]);

  const [drawerOpen, setDrawerOpen] = useState(false);
  // Persist pending promotions when the selector opens (proposal 0078 A3): the
  // list is about to be read, and another tab reading it should see it.
  useEffect(() => {
    if (drawerOpen) recentsStore.flush();
  }, [drawerOpen, recentsStore]);
  // Bumped by the `⌃B r` chord (proposal 0035) to put the active pane's
  // identity-bar name into edit mode — even with no pointer. Same focus-seq
  // trick as the editor's `focusSearchSeq`.
  const [renameSeq, setRenameSeq] = useState(0);
  // Bumped by the `⌃B /` chord (proposal 0068) to open the active pane's
  // terminal find bar. Under the WebGL renderer the terminal is pixels, so the
  // browser's own Cmd/Ctrl+F can't search agent output any more — this is its
  // replacement. `⌃B t` still opens the file tree's filter field ([0038]).
  const [termSearchSeq, setTermSearchSeq] = useState(0);
  const [composeOpen, setComposeOpen] = useState(false);
  const [imageOpen, setImageOpen] = useState(false);
  const [favOpen, setFavOpen] = useState(false);
  // The searchable session-status overview (proposal 0022).
  const [statusOpen, setStatusOpen] = useState(false);
  // "Update coding assistants" (proposal 0049): the confirm-then-progress
  // overlay, and whether a job is currently moving (the header button keeps a
  // live indicator while the panel is closed — the job runs on the agent).
  const [updateOpen, setUpdateOpen] = useState(false);
  const [updateBusy, setUpdateBusy] = useState(false);
  // Non-empty when the flow was opened from a machine row (the dashboard's
  // per-machine entry point) rather than from the top bar's fleet action.
  const [updateScope, setUpdateScope] = useState("");
  // Non-empty when the flow was opened from a *greyed-out tool* (the create
  // picker's "Install it" path, proposal 0050 F4) — the dialog then scopes to
  // that one CLI on that one machine.
  const [updateTools, setUpdateTools] = useState<string[]>([]);
  // The file editor is a SINGLETON, app-wide overlay — not per-pane (desktop can
  // show up to 4 terminals, but only ever one editor, covering the whole
  // screen). `path` is the file to open; null means "let the user pick from the
  // desktop tree" (the Ctrl+B e entry). editorOpenRef shadows it so the global
  // keyboard handler can go inert while the editor owns the screen.
  // Restore the file-viewer *mode* across reloads (path stays null — the file is
  // restored from per-session viewer memory once the overlay mounts).
  // `focusSearchSeq` is bumped by the `Ctrl+B f` chord (proposal 0027) to focus
  // the viewer's in-tree Find bar — even when the viewer is already open.
  const [editor, setEditor] = useState<{
    open: boolean;
    path: string | null;
    // Proposal 0083: the browse machine a deep link named, overriding the
    // pane-derived default. "" = follow the pane (today's behaviour).
    machine: string;
    // Proposal 0083: the folder form of a deep link (`/file/<m>/dir/`) — the
    // tree opens here instead of a file buffer.
    dir: string | null;
    focusSearchSeq: number;
    // Proposal 0038: `Ctrl+B /` bumps this to focus the in-tree "Filter tree"
    // field (mirror of focusSearchSeq for [0027]'s find-file bar).
    focusTreeFilterSeq: number;
  }>(() => ({
    open: loadEditorOpen(),
    path: null,
    machine: "",
    dir: null,
    focusSearchSeq: 0,
    focusTreeFilterSeq: 0,
  }));
  const editorOpenRef = useRef(false);
  useEffect(() => { editorOpenRef.current = editor.open; }, [editor.open]);
  // Proposal 0083: a file deep link is being resolved. True from the FIRST
  // render (not from an effect) so the phone's drawer auto-open below never
  // gets a frame in which to cover the editor the bookmark asked for. Cleared
  // whether the resolve succeeds or fails — a stuck flag would leave a phone
  // with no way back to the switcher.
  const [fileLinkPending, setFileLinkPending] = useState(!!FILE_LINK);
  const fileLinkDone = useRef(false);
  // ⌃B l — copy the open file's link (proposal 0083 Part B). Bumped here,
  // consumed inside the overlay (which holds the path, the machine and $HOME).
  const [copyLinkSeq, setCopyLinkSeq] = useState(0);
  // Where the open editor is pointing, reported up by the overlay — the input
  // to Part B's address-bar sync and to the ⌃B l copy chord.
  const [editorLoc, setEditorLoc] = useState<EditorLocation | null>(null);
  const onEditorLocation = useCallback(
    (loc: EditorLocation) =>
      setEditorLoc((prev) =>
        prev &&
        prev.path === loc.path &&
        prev.dir === loc.dir &&
        prev.machine === loc.machine &&
        prev.home === loc.home
          ? prev // identity-stable: a no-op report must not re-run urlSync
          : loc
      ),
    []
  );
  // Persist the open/closed mode so a reload returns to the file viewer or the
  // terminal grid, whichever was active.
  useEffect(() => {
    try {
      if (editor.open) localStorage.setItem(EDITOR_OPEN_KEY, "1");
      else localStorage.removeItem(EDITOR_OPEN_KEY);
    } catch { /* quota — ignore */ }
  }, [editor.open]);
  // The editor reports its unsaved-buffer state up here so a session switch under
  // the open viewer (Ctrl+B ↑/↓ or a drawer pick) can prompt before discarding
  // it — the source-side dirty guard from proposal 0019. A ref shadows it so the
  // keyboard handler + pick read the fresh value without re-binding.
  const editorDirtyRef = useRef(false);
  const onEditorDirtyChange = useCallback((d: boolean) => {
    editorDirtyRef.current = d;
  }, []);
  // File-upload state. The list is captured at trigger time — flattened from
  // a desktop drop (folders walked via webkitGetAsEntry in api.ts) or from the
  // phone's file picker — and uploadPane is the pane's session+machine captured
  // at the same moment, so a later pane switch doesn't retarget the upload (and
  // the upload routes to the owning machine).
  const [uploadOpen, setUploadOpen] = useState(false);
  const [uploadFilesList, setUploadFilesList] = useState<UploadFile[]>([]);
  const [uploadPane, setUploadPane] = useState<PaneRef | null>(null);
  // Hidden <input type="file"> the phone's footer Upload button triggers. iOS
  // turns this into a Photo Library / Take Photo / Choose Files menu, so one
  // control covers both "image" and "file" uploads.
  const uploadInputRef = useRef<HTMLInputElement>(null);
  // Layout palette (desktop-only): floating popover anchored under the
  // header trigger, navigated by ←/→ + Enter. paletteOpenRef shadows it so
  // the Ctrl+B chord handler — which captures keys on `window` *before* the
  // palette's onKeyDown sees them — can bail out and let the palette own
  // the keyboard. Synchronously updated by openPalette/closePalette so the
  // gating works on the very next keystroke, not after the next render.
  const [paletteOpen, setPaletteOpen] = useState(false);
  const paletteOpenRef = useRef(false);
  const openPalette = useCallback(() => {
    paletteOpenRef.current = true;
    setPaletteOpen(true);
  }, []);
  const closePalette = useCallback(() => {
    paletteOpenRef.current = false;
    setPaletteOpen(false);
  }, []);
  const [favorites, setFavorites] = useState<Favorite[]>([]);
  // When the create flow starts we remember which pane to mount the
  // newly-created session into. -1 means "phone path / default — pane 0".
  // The create flow lives in-drawer (proposal 0016). Empty panes now host the
  // switcher inline (proposal 0026), so their create flow runs in place and
  // mounts via onPaneCreated — no cross-component "jump to create" token needed.
  const [newForPane, setNewForPane] = useState<number>(-1);
  const [deleting, setDeleting] = useState<Set<string>>(new Set());
  // Small ephemeral toast for paste-event feedback (and any other one-shot
  // confirmation we add later). Auto-dismissed by the show() helper below.
  const [toast, setToast] = useState<{ msg: string; ok: boolean } | null>(null);
  const toastTimerRef = useRef<number | null>(null);
  const showToast = useCallback((msg: string, ok: boolean) => {
    setToast({ msg, ok });
    if (toastTimerRef.current != null) window.clearTimeout(toastTimerRef.current);
    toastTimerRef.current = window.setTimeout(() => {
      setToast(null);
      toastTimerRef.current = null;
    }, 2500);
  }, []);
  const composeRef = useRef<ComposeHandle>(null);
  const favRef = useRef<FavoritesHandle>(null);
  // In-app "session went ready" toasts (proposal 0017). The host owns its own
  // toast list + dismissal timers; we feed it gated busy→waiting edges below.
  const toastHostRef = useRef<ToastHostHandle>(null);
  // Previous session snapshot for the ready-edge diff. null until the first poll
  // establishes a baseline (that snapshot toasts nothing).
  const prevSnapshotRef = useRef<Session[] | null>(null);
  // Persisted on/off for the toasts (drawer toggle). A ref shadows it so the
  // detector effect can read the live value without re-subscribing.
  const [toastsEnabled, setToastsEnabled] = useState<boolean>(loadToastsEnabled);
  const toastsEnabledRef = useRef(toastsEnabled);
  useEffect(() => { toastsEnabledRef.current = toastsEnabled; }, [toastsEnabled]);
  // Toggle the setting; on enable, fire a one-off **test toast** so the user
  // immediately sees what a real ready-notification looks like (mirrors the Web
  // Push bell's test buzz). The test entry has an empty name so a click just
  // dismisses (openSessionByName("") is a no-op) rather than hunting a session.
  const toggleToasts = useCallback(() => {
    setToastsEnabled((prev) => {
      const next = !prev;
      try {
        localStorage.setItem(TOASTS_KEY, next ? "1" : "0");
      } catch { /* quota — ignore */ }
      if (next) {
        toastHostRef.current?.push([
          {
            name: "",
            machine: "",
            tool: currentSession ? sessionsRef.current.find((s) => s.name === currentSession.name)?.tool ?? "claude" : "claude",
            short: "Test toast — this is how a ready session appears",
          },
        ]);
      }
      return next;
    });
  }, [currentSession]);

  // Track the visible area (shrinks when the soft keyboard opens) so the app —
  // terminal, footer, and the compose/image sheets — stays above the keyboard
  // instead of hiding behind it. iOS Safari overlays the keyboard rather than
  // resizing the layout viewport, so we resize ourselves to visualViewport.
  const [appH, setAppH] = useState<number | null>(null);
  useEffect(() => {
    const vv = window.visualViewport;
    if (!vv) return;
    const apply = () => {
      setAppH(vv.height);
      window.scrollTo(0, 0); // keep the layout viewport pinned to the top
      // ...and undo any focus-induced offset on the inner shell (#root); the
      // window reset above can't touch a scrollTop that lives there.
      // See cc-screen-saas docs/proposals/archived/0004-scroll-jump-fix.md.
      const root = document.getElementById("root");
      if (root) {
        root.scrollTop = 0;
        root.scrollLeft = 0;
      }
    };
    apply();
    vv.addEventListener("resize", apply);
    vv.addEventListener("scroll", apply);
    return () => {
      vv.removeEventListener("resize", apply);
      vv.removeEventListener("scroll", apply);
    };
  }, []);

  // Backstop for the scroll-jump (cc-screen-saas
  // docs/proposals/archived/0004-scroll-jump-fix.md). The shell is meant to be a
  // fixed, non-scrolling frame, but a programmatic .focus() on an element below
  // the fold makes the browser scroll the focused element into view — and an
  // overflow:hidden ancestor (#root) is still programmatically scrollable, so it
  // ends up with a non-zero scrollTop that shoves the header off-screen. Fix 1
  // (preventScroll on every focus) removes the cause; this listener is the cheap
  // belt-and-suspenders that catches any focus path Fix 1 misses, now or later.
  useEffect(() => {
    const root = document.getElementById("root");
    if (!root) return;
    const pin = () => {
      if (root.scrollTop !== 0) root.scrollTop = 0;
      if (root.scrollLeft !== 0) root.scrollLeft = 0;
    };
    root.addEventListener("scroll", pin, { passive: true });
    return () => root.removeEventListener("scroll", pin);
  }, []);

  // Per-pane connection state for the header dot and pane-corner indicators.
  // Indexed by pane; entries past `layout` are ignored.
  const [conns, setConns] = useState<ConnState[]>(() => Array(4).fill("closed"));
  // refresh() is defined further down; a ref lets setPaneConn reach the latest
  // without a declaration-order dependency.
  const refreshRef = useRef<() => void>(() => {});
  const closeRefreshTimer = useRef<number | null>(null);
  const setPaneConn = useCallback(
    (idx: number, c: ConnState) => {
      setConns((prev) => {
        if (prev[idx] === c) return prev;
        const next = prev.slice();
        next[idx] = c;
        return next;
      });
      // A socket closing usually means the session just ended — the backend
      // closes the WS the instant the child process exits. Re-poll promptly
      // (debounced) so the dead session's pane clears right away instead of on
      // the 2.5s interval. recover-when-gone only clears a pane whose session is
      // actually gone, so a transient reconnect blip is harmless.
      if (c === "closed") {
        if (closeRefreshTimer.current != null) clearTimeout(closeRefreshTimer.current);
        closeRefreshTimer.current = window.setTimeout(() => {
          closeRefreshTimer.current = null;
          refreshRef.current();
        }, 150);
      }
    },
    []
  );
  // Pane-indexed xterm registration (TerminalView.onTerm). Stable identity so a
  // re-render of App doesn't invalidate the memoized panes (proposal 0068 C).
  const setPaneTerm = useCallback((idx: number, t: Terminal | null) => {
    termsRef.current[idx] = t;
  }, []);
  // The editor overlay's agent mirror, when one is mounted (proposal 0077 B).
  const agentTermRef = useRef<Terminal | null>(null);
  const setAgentTerm = useCallback((t: Terminal | null) => {
    agentTermRef.current = t;
  }, []);
  // The phone renders a single TerminalView, but it reports into the ACTIVE
  // pane's slot (see the comment at that call site) — so these bind to `active`.
  const onPhoneConn = useCallback(
    (c: ConnState) => setPaneConn(activeRef.current, c),
    [setPaneConn]
  );
  const onPhoneTerm = useCallback((t: Terminal | null) => {
    termsRef.current[activeRef.current] = t;
  }, []);
  const [fontSize, setFontSize] = useState<number>(
    () => Number(localStorage.getItem(FONT_KEY)) || 13
  );

  // Pane mutators (all funnel through here so persistence stays consistent).
  const updatePanes = useCallback(
    (mut: (s: PaneState) => PaneState) => setPaneState((s) => mut(s)),
    []
  );

  // mountAt assigns `ref` (or null) to pane `idx`. If the same session is
  // already mounted in another pane, it's removed from there — each session
  // can live in at most one pane (PTY width is shared, so two attached clients
  // at different widths would fight every resize). Identity is (name, machine):
  // a same-named session on a *different* machine is a different session and is
  // left alone.
  const mountAt = useCallback(
    (idx: number, ref: PaneRef | null) => {
      updatePanes((s) => {
        const next = s.panes.slice();
        if (ref) {
          for (let i = 0; i < next.length; i++) {
            if (i !== idx && next[i]?.name === ref.name && next[i]?.machine === ref.machine) {
              next[i] = null;
            }
          }
        }
        next[idx] = ref;
        return { ...s, panes: next };
      });
    },
    [updatePanes]
  );

  // setActive focuses a pane and remembers the one it came from, so ⌃B ;
  // (tmux's last-pane, proposal 0081 Part D) can bounce back. The identity
  // return matters: onActivate fires on every pointerdown, so without it a
  // click on the *already* focused pane would overwrite `prev` with itself and
  // strand the toggle.
  const setActive = useCallback(
    (idx: number) =>
      updatePanes((s) => {
        const next = Math.max(0, Math.min(paneCount(s.layout) - 1, idx));
        if (next === s.active) return s;
        return { ...s, active: next, prev: s.active };
      }),
    [updatePanes]
  );

  // toggleLastPane — the ⌃B ; chord (tmux's last-pane, proposal 0081 Part D).
  // Swaps active/prev *inside the reducer* rather than reading a mirrored ref,
  // so a fast ping-pong can't act on a value a render hasn't caught up with.
  // Stable identity, so the keydown effect can depend on it without re-binding
  // (a re-bind would clear an in-flight prefix timer mid-chord).
  const toggleLastPane = useCallback(
    () =>
      updatePanes((s) => {
        const next = Math.max(0, Math.min(paneCount(s.layout) - 1, s.prev));
        if (next === s.active) return s;
        return { ...s, active: next, prev: s.active };
      }),
    [updatePanes]
  );

  // setLayout resizes the grid — the migration lives in `applyLayout`
  // (paneState.ts) so it is pure and unit-testable; see its comment for the
  // shrink/promote rule and the clamping of `active`/`prev`.
  const setLayout = useCallback(
    (l: Layout) => updatePanes((s) => applyLayout(s, l)),
    [updatePanes]
  );

  // Persist on every change (small payload, debounce not worth it).
  useEffect(() => {
    try {
      localStorage.setItem(PANES_KEY, JSON.stringify(paneState));
    } catch { /* quota — ignore */ }
    // Also keep the legacy single-session key in sync so an older client
    // version still lands somewhere sensible if downgraded.
    if (currentSession) localStorage.setItem(LAST_KEY, currentSession.name);
    else localStorage.removeItem(LAST_KEY);
  }, [paneState, currentSession]);

  // closeAllSheets centralises the "open the drawer, hide everything else"
  // dance so it stays consistent across the Ctrl+B and ☰ paths.
  const closeAllSheets = useCallback(() => {
    setComposeOpen(false);
    setImageOpen(false);
    setFavOpen(false);
    setUploadOpen(false);
    closePalette();
  }, [closePalette]);

  // openEditor surfaces the singleton editor overlay (closing any sheet first
  // so it doesn't peek through). `path` null = desktop tree-pick entry.
  // `focusSearch` (the Ctrl+B f path) bumps focusSearchSeq so the viewer focuses
  // its Find bar — also when already open, so re-pressing re-focuses in place.
  // `opts.machine` / `opts.dir` are the proposal 0083 deep-link seam: a
  // `/file/…` URL names the machine whose $HOME the path belongs to, and its
  // folder form opens the tree instead of a buffer. Both default to "follow the
  // pane / no folder", so every pre-0083 call site behaves exactly as before.
  const openEditor = useCallback(
    (path: string | null, focusSearch = false, opts?: { machine?: string; dir?: string | null }) => {
      closeAllSheets();
      setEditor((s) => ({
        ...s,
        open: true,
        path,
        machine: opts?.machine ?? "",
        dir: opts?.dir ?? null,
        focusSearchSeq: s.focusSearchSeq + (focusSearch ? 1 : 0),
      }));
    },
    [closeAllSheets]
  );
  // Open the editor and focus its tree-filter field (the `Ctrl+B /` path) —
  // bumps focusTreeFilterSeq so it re-focuses even when already open (0038).
  const focusTreeFilter = useCallback(() => {
    closeAllSheets();
    setEditor((s) => ({
      ...s,
      open: true,
      path: null,
      dir: null,
      focusTreeFilterSeq: s.focusTreeFilterSeq + 1,
    }));
  }, [closeAllSheets]);
  const closeEditor = useCallback(
    () => setEditor((s) => ({ ...s, open: false, path: null, machine: "", dir: null })),
    []
  );

  // Sessions just created via New Session, keyed `machine/name` → grace expiry
  // (ms). A create confirms the agent made the session, but the hub's union list
  // can lag a push behind — so for a short window we DON'T let applySessionList
  // null the freshly-mounted pane merely because the session hasn't propagated
  // yet (which would bounce the user to the switcher). Cleared once it appears.
  const recentMounts = useRef<Map<string, number>>(new Map());
  const refKey = (r: { name: string; machine: string }) => `${r.machine}/${r.name}`;
  // Serialized form of the last applied session list — the equality gate in
  // applySessionList (proposal 0068 Part C).
  const sessionsKeyRef = useRef<string | null>(null);

  // Adopt a freshly-fetched session list: render it, keep the chord handler's
  // ref fresh, and drop any pane holding a now-dead session. We deliberately
  // never auto-attach to an arbitrary session — landing on someone's live agent
  // unbidden would resize and disrupt it. Split out from `refresh` so the quiet
  // background poll can reuse it without touching the loading/error UI.
  const applySessionList = useCallback(
    (list: Session[]) => {
      // Byte-identical payload → keep the existing array identity (proposal
      // 0068 Part C). An idle fleet re-serves the same list every 4s, and
      // handing React a fresh array made the whole app (every mounted xterm
      // pane included) re-render on each tick for nothing. The pane reconcile
      // below still runs when a propagation grace window is outstanding, since
      // that decision is time-dependent rather than payload-dependent.
      const key = JSON.stringify(list);
      const same = key === sessionsKeyRef.current;
      if (!same) {
        sessionsKeyRef.current = key;
        setSessions(list);
        sessionsRef.current = list;
      } else if (recentMounts.current.size === 0) {
        return;
      }
      const live = new Set(sessionsRef.current.map((s) => s.name));
      const now = Date.now();
      updatePanes((s) => {
        const next = s.panes.map((p) => {
          if (!p) return null;
          if (live.has(p.name)) {
            recentMounts.current.delete(refKey(p)); // propagated — drop the grace
            return p;
          }
          // Not (yet) in the list: keep it only if it's a just-created session
          // still inside its propagation grace window; otherwise it's dead.
          const exp = recentMounts.current.get(refKey(p));
          return exp && now < exp ? p : null;
        });
        const changed = next.some((p, i) => p !== s.panes[i]);
        return changed ? { ...s, panes: next } : s;
      });
    },
    [updatePanes]
  );

  // Boot-time auth check + 401 handler. With auth off this resolves to authed
  // immediately; with auth on it shows the login screen until a valid cookie or
  // token is present. A later 401 from the poll (expired cookie / logged out
  // elsewhere) flips us back to login. If /api/auth itself fails (an older
  // server with no such endpoint, or a transient error) we don't hard-block an
  // unprotected box — treat it as "no gate".
  useEffect(() => {
    setUnauthorizedHandler(() => setAuthed(false));
    // Prefer /api/me (multi-tenant boot read). On a multi-tenant hub it drives the
    // identity gate; otherwise (single-tenant hub, or an older agent with no
    // /api/me) fall back to the shared-secret /api/auth gate — unchanged behavior.
    const singleTenantGate = () =>
      getAuthStatus()
        .then((s) => setAuthed(!s.authRequired || s.authed))
        .catch(() => setAuthed(true));
    getMe()
      .then((m) => {
        setMe(m);
        if (m.multiTenant) setAuthed(m.authenticated);
        else return singleTenantGate();
      })
      .catch(singleTenantGate);
    return () => setUnauthorizedHandler(null);
  }, []);

  // After a multi-tenant login/signup, re-read identity (email/userId) and reveal.
  const refetchMe = useCallback(() => {
    getMe()
      .then((m) => {
        setMe(m);
        setAuthed(m.authenticated);
      })
      .catch(() => setAuthed(true));
  }, []);

  // Checkout return (proposal 0058 C4): /billing/success lands on the Dashboard
  // and polls for the entitlement flip; /billing/cancel just opens the Dashboard.
  // Runs once on mount — the SPA fallback serves both paths (no hub routing).
  useEffect(() => {
    if (typeof window === "undefined") return;
    const p = window.location.pathname;
    if (p === "/billing/success") {
      window.history.replaceState({}, "", "/");
      setShowDash(true);
      setBillingPending("pending");
    } else if (p === "/billing/cancel") {
      window.history.replaceState({}, "", "/");
      setShowDash(true);
    }
  }, []);

  // Entitlement-flip watcher (0058 C4): the webhook typically lands in 1–5s and
  // flips plan.status to "active" — clear the "activating…" notice when it does.
  useEffect(() => {
    if (billingPending !== null && me?.plan?.status === "active") {
      setBillingPending(null);
    }
  }, [billingPending, me]);

  // Poll /api/me every 2s for up to 30s after returning from checkout, plus once
  // whenever the tab becomes visible (an installed PWA the OS froze during the
  // Stripe hop). Past 30s we stop the cadence and degrade the copy to "slow".
  // Depends only on the null↔non-null transition so refetches don't reset the
  // 30s budget; the visibilitychange listener stays for the "slow" phase too.
  const billingActive = billingPending !== null;
  useEffect(() => {
    if (!billingActive) return;
    const started = Date.now();
    const iv = window.setInterval(() => {
      if (Date.now() - started >= 30_000) {
        setBillingPending((cur) => (cur === "pending" ? "slow" : cur));
        window.clearInterval(iv);
        return;
      }
      refetchMe();
    }, 2000);
    const onVis = () => {
      if (document.visibilityState === "visible") refetchMe();
    };
    document.addEventListener("visibilitychange", onVis);
    return () => {
      window.clearInterval(iv);
      document.removeEventListener("visibilitychange", onVis);
    };
  }, [billingActive, refetchMe]);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      applySessionList(await fetchSessions());
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, [applySessionList]);
  refreshRef.current = refresh; // keep the on-close re-poll pointing at the latest

  // Explicit navigation target used by notification taps and `?session=...`
  // deep links. This is the one place that turns a session name into the pane
  // identity the app stores, including the owning machine when known.
  const openSessionByName = useCallback(
    async (name: string): Promise<boolean> => {
      const wanted = name.trim();
      if (!wanted) return false;

      let list = sessionsRef.current;
      let found = list.find((s) => s.name === wanted);
      if (!found) {
        try {
          list = await fetchSessions();
          applySessionList(list);
          found = list.find((s) => s.name === wanted);
        } catch (e) {
          setError(e instanceof Error ? e.message : String(e));
          return false;
        }
      }
      if (!found) return false;

      closeAllSheets();
      mountAt(activeRef.current, { name: found.name, machine: found.machine ?? "" });
      setDrawerOpen(false);
      return true;
    },
    [applySessionList, closeAllSheets, mountAt]
  );

  // Cold-open deep link: a service-worker `openWindow("/?session=...")` lands
  // here. Keep the query parameter until we successfully mount the session so a
  // transient early fetch failure can be retried by a reload.
  useEffect(() => {
    if (authed !== true) return;
    const params = new URLSearchParams(window.location.search);
    const session = params.get("session");
    if (!session) return;

    let cancelled = false;
    openSessionByName(session).then((opened) => {
      if (cancelled || !opened) return;
      params.delete("session");
      const qs = params.toString();
      window.history.replaceState(
        null,
        "",
        `${window.location.pathname}${qs ? `?${qs}` : ""}${window.location.hash}`
      );
    });
    return () => {
      cancelled = true;
    };
  }, [authed, openSessionByName]);

  // Cold-open FILE deep link (proposal 0083 Part A): `/file/<machine>/<rel>`
  // opens that file straight in the editor — the bookmark path, and the whole
  // point of the proposal on a phone, where the viewer restores nothing.
  //
  // Modeled line-for-line on the `?session=` consumer above: it waits for
  // `authed === true`, so a signed-out hit lands in login and continues here
  // afterwards (including across the Google OAuth bounce, which returns to the
  // same path). It differs in one deliberate way — it does NOT strip the URL.
  // Part B's urlSync owns the address bar from here, and a bookmarkable URL
  // that erased itself on arrival would defeat the feature.
  //
  // The one round-trip is `home`: a link is home-relative (see fileLink.ts), so
  // the absolute path the API wants is `home + "/" + relPath`, and `home` comes
  // back on any `/api/files` listing for that machine. A failure leaves the URL
  // alone so a reload retries, and still opens the editor — on the tree, where
  // [0079]'s heal-don't-latch rule wants a stale link to land.
  useEffect(() => {
    if (authed !== true || !FILE_LINK) return;
    if (fileLinkDone.current) return;
    fileLinkDone.current = true;
    const link = FILE_LINK;
    let cancelled = false;
    fetchFiles(undefined, undefined, link.machine)
      .then((r) => {
        if (cancelled) return;
        const abs = joinHome(r.home, link.relPath);
        if (link.isDir) openEditor(null, false, { machine: link.machine, dir: abs });
        else openEditor(abs, false, { machine: link.machine });
      })
      .catch(() => {
        // Unknown machine, offline agent, expired cookie mid-flight: open the
        // editor on that machine's tree and let the overlay say what's wrong.
        if (!cancelled) openEditor(null, false, { machine: link.machine });
      })
      .finally(() => {
        if (!cancelled) setFileLinkPending(false);
      });
    return () => {
      cancelled = true;
    };
  }, [authed, openEditor]);

  // Warm-open notification tap: the service worker focuses this window and asks
  // it to mount the notified session instead of leaving the user on the prior
  // pane.
  useEffect(() => {
    if (!("serviceWorker" in navigator)) return;
    const handler = (event: MessageEvent) => {
      const data = event.data as { type?: unknown; session?: unknown } | null;
      if (data?.type !== "open-session" || typeof data.session !== "string") return;
      openSessionByName(data.session).catch(() => {});
    };
    navigator.serviceWorker.addEventListener("message", handler);
    return () => navigator.serviceWorker.removeEventListener("message", handler);
  }, [openSessionByName]);

  // Quiet background poll so the working/idle state (and the title + app-icon
  // badge below) stays current while the app is open — without the manual
  // refresh button's spinner or clobbering an error banner.
  //
  // Visible: 4s, unchanged (0017's toast diffing and 0023's timers are specified
  // against it). Hidden: a 60s heartbeat rather than a full pause — the title
  // and the PWA app badge are read precisely *while* the tab is hidden, and 60s
  // is what the browser's own background-timer throttle already gave us. On
  // return (focus or visibilitychange, deduped to one) it refetches through this
  // quiet path rather than refresh(), so there's no spinner flash.
  const pollSessions = useCallback(() => {
    fetchSessions().then(applySessionList).catch(() => {});
  }, [applySessionList]);
  usePoll(pollSessions, 4000, {
    enabled: authed === true,
    hiddenMs: HIDDEN_SESSIONS_MS,
    onFocus: true,
  });

  // Ambient "are my agents still running?" signal: the tab title and (installed
  // PWA) app-icon badge show how many sessions are actively producing output.
  // `waiting` is an idle agent's resting state, so we surface the inverse — the
  // count of *working* agents — which falls to zero once everything has
  // finished and is waiting for you. (See the server's WORK_GRACE_SECS.)
  useEffect(() => {
    const working = sessions.filter((s) => !s.waiting).length;
    document.title = working > 0 ? `${working} running — Pine` : "Pine";
    const nav = navigator as Navigator & {
      setAppBadge?: (n?: number) => Promise<void>;
      clearAppBadge?: () => Promise<void>;
    };
    if (working > 0) nav.setAppBadge?.(working).catch(() => {});
    else nav.clearAppBadge?.().catch(() => {});
  }, [sessions]);

  // In-app session toasts (proposal 0017): diff each new poll snapshot against
  // the previous one and toast any non-mounted session that crossed the gated
  // busy→waiting edge (§2 — same gate the 0002 OS push uses server-side). This
  // runs on every `sessions` update (one per poll, since applySessionList always
  // sets a fresh array), so it is exactly per-snapshot.
  //
  // Foreground-only: when the tab is hidden, 0002's OS push owns the event — we
  // still advance the baseline (so a busy→waiting that happened while hidden is
  // never retroactively toasted on return) but emit nothing. The first snapshot
  // (prev === null) is baseline-only.
  useEffect(() => {
    const prev = prevSnapshotRef.current;
    prevSnapshotRef.current = sessions;
    if (prev === null) return; // first snapshot: baseline only
    // Still advance the baseline above when toasts are off, so re-enabling
    // doesn't replay a stale edge; just don't emit while disabled.
    if (!toastsEnabledRef.current) return;
    if (document.visibilityState !== "visible") return; // hidden: OS push owns it
    const mounted = new Set(
      panesRef.current.filter((p): p is PaneRef => p != null).map(sessionKey)
    );
    const edges = detectReadyEdges(prev, sessions, mounted, Date.now());
    if (edges.length) toastHostRef.current?.push(edges);
  }, [sessions]);

  // Initial load — only once authenticated, so a multi-tenant login screen
  // doesn't fire authed-only API calls (which would 401 noisily). With auth off
  // `authed` flips true immediately, so this is unchanged for single-tenant.
  useEffect(() => {
    if (authed === true) refresh();
  }, [refresh, authed]);

  // Poll the hub's machine roster (empty [] on a standalone agent, which has no
  // /api/machines). Drives the per-machine grouping + pickers; polled slowly
  // since the roster changes rarely (an agent joining/leaving the fleet).
  // Paused while the tab is hidden (it only feeds pickers and badges nobody can
  // read then) and refetched on return — proposal 0068 Part C.
  const loadMachines = useCallback(() => {
    fetchMachines()
      .then((list) => setMachines((prev) => (sameJson(prev, list) ? prev : list)))
      .catch(() => {});
  }, []);
  usePoll(loadMachines, 10000, { enabled: authed === true, immediate: true });

  // Poll the shares granted TO me (proposal 0041) so the shared-vs-owned badges
  // reflect accepts/leaves/revokes. Multi-tenant only; a no-op endpoint on a
  // single agent, so we gate on me.multiTenant to avoid a needless 404 loop.
  const refreshReceivedShares = useCallback(() => {
    if (!me?.multiTenant) return;
    listReceivedShares()
      .then((list) => setReceivedShares((prev) => (sameJson(prev, list) ? prev : list)))
      .catch(() => {});
  }, [me?.multiTenant]);
  usePoll(refreshReceivedShares, 20000, {
    enabled: authed === true && !!me?.multiTenant,
    immediate: true,
  });

  // Open the ShareForm overlay for a session (proposal 0041), titling it by the
  // session's display name. Reached from the switcher row, the identity bar, and
  // the ⌃B S chord.
  const openShareFor = useCallback((ref: PaneRef) => {
    // Read the live list via the ref so this stays a stable callback safe to use
    // from the (re-registered-rarely) keydown handler.
    const meta = sessionsRef.current.find((s) => s.name === ref.name && (s.machine ?? "") === ref.machine);
    setShareTarget({ title: meta ? displayName(meta) : ref.name, machine: ref.machine, session: ref.name });
  }, []);

  // Mint a read-only link grant for one file (proposal 0083 Part C). The same
  // ShareForm overlay as every other share — the file subject just puts it in
  // its `link` mode (no recipient; the URL is the recipient).
  const openShareLink = useCallback(
    (s: { title: string; machine: string; path: string; relPath: string }) =>
      setShareTarget({ title: s.title, machine: s.machine, path: s.path, pathLabel: s.relPath }),
    []
  );

  // Refresh the restore offer whenever the drawer opens — cheap, and the only
  // surface that shows it. Errors are non-fatal (just hides the offer).
  // With multiple machines, the restore offer is scoped to the focused machine
  // (else the first online) — a machine-less restore would be ambiguous at the
  // hub. Single-machine passes "" (unchanged, machine-less) behaviour.
  const restoreMachine = multiMachine ? currentSession?.machine || firstOnlineMachine : "";
  useEffect(() => {
    if (authed !== true || !drawerOpen) return;
    fetchRestorable(restoreMachine).then(setRestorable).catch(() => setRestorable([]));
  }, [authed, drawerOpen, restoreMachine]);

  // Bring back every recorded-but-dead session (resuming each tool's
  // conversation), then re-list and re-check what's still restorable.
  const onRestore = useCallback(async () => {
    try {
      await restoreSessions(restoreMachine);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      await refresh();
      fetchRestorable(restoreMachine).then(setRestorable).catch(() => setRestorable([]));
    }
  }, [refresh, restoreMachine]);

  // Open the switcher whenever the active pane has nothing (first run, last
  // session in this pane vanished). On desktop the empty pane already shows
  // an inline picker — but on phones (single pane, no inline picker) the
  // drawer is the only way to attach, so keep popping it open there.
  // Proposal 0083: two suppressions, and the second is the one that bit.
  //
  // `fileLinkPending` covers the window before the editor opens. But the drawer
  // also opens on `currentSession === null`, and on a cold phone load that is
  // true for a beat *after* the deep link resolves — the session list hasn't
  // arrived yet — so the drawer slid over the file the user had just been
  // taken to, and nothing ever closed it again.
  //
  // The general rule fixes both: while the file viewer owns the screen, don't
  // put the switcher over it. The drawer exists to say "this pane has nothing,
  // here's how to attach", which is not urgent while you're reading a file.
  // Closing the viewer re-runs this (it's in the deps) and pops the drawer then
  // — so the empty-pane case a phone still needs is unchanged.
  useEffect(() => {
    if (fileLinkPending || editor.open) return;
    if (!isDesktop && currentSession === null) setDrawerOpen(true);
  }, [isDesktop, currentSession, fileLinkPending, editor.open]);

  // ── Proposal 0083 Part B — the address bar tracks the open file ────────────
  // The cheapest way to create a bookmark is for the URL to already be right
  // when the user presses ⌘D. `replaceState` only (the app's single history
  // verb — 0083 explicitly does not introduce push/popstate navigation).
  //
  // Single writer, two guards:
  //   * it only ever writes over `/` or an existing `/file/…` — so a reserved
  //     page (`/activate`, `/invite/…`, `/billing/…`, `/s/…`) is never
  //     clobbered, and neither is Part A's inbound link before it resolves;
  //   * it writes nothing until `home` is known for the current browse machine,
  //     because a URL is home-relative and an absolute path has no link form.
  // Query + hash are preserved so a pending `?session=` consumer keeps its
  // parameter.
  useEffect(() => {
    if (typeof window === "undefined") return;
    const here = window.location.pathname;
    if (here !== "/" && !here.startsWith("/file/")) return;
    let want: string | null = null;
    if (editor.open && editorLoc) {
      // File → folder → machine root, the same ladder the phone's two-step
      // close walks (0083 Mobile/touch), so a bookmark made mid-ladder means
      // what the screen showed.
      const target = editorLoc.path ?? editorLoc.dir;
      const rel = target ? relFromHome(editorLoc.home, target) : null;
      if (target) {
        // Outside $HOME there is no link form; leave the bar as it is rather
        // than minting a URL that would 404 on arrival.
        if (rel !== null) want = fileLinkPath(editorLoc.machine, rel, !editorLoc.path);
      } else if (editorLoc.home) {
        want = fileLinkPath(editorLoc.machine, "", true);
      }
    } else if (!editor.open) {
      want = "/";
    }
    if (want && want !== here) {
      window.history.replaceState({}, "", `${want}${window.location.search}${window.location.hash}`);
    }
  }, [editor.open, editorLoc]);

  // (Re-listing on PWA resume / tab focus is part of the sessions poll above —
  // one deduplicated handler for `focus` + `visibilitychange`, on the quiet
  // applySessionList path so returning to the tab never flashes the spinner.)

  // Keyboard:
  //  Phone: Ctrl+B toggles the drawer immediately (existing behaviour).
  //  Desktop: Ctrl+B is a tmux-style PREFIX. The next key within 600ms is
  //  consumed as a chord; if no chord arrives the drawer opens (same end
  //  state, just slightly delayed when bare). Chords:
  //    1-4         focus pane N
  //    ← / →       cycle the active pane (index ±1 with wrap)
  //    ↑ / ↓       cycle the session shown in the active pane through the
  //                global session list (skipping sessions already mounted in
  //                another pane). On an empty pane, ↓ mounts the first
  //                available, ↑ the last — so you can fill a fresh pane
  //                without opening the drawer.
  //    l / Space   open the layout palette (←/→ pick, ⏎ apply, Esc cancel)
  //    s           open the session drawer (instant — for users who hated
  //                the 600ms wait of bare Ctrl+B)
  //    x           unmount the session in the active pane
  //    Esc         cancel the prefix
  //
  // After an arrow chord, the next arrow keypress within ARROW_REPEAT_MS
  // is also intercepted *without* needing Ctrl+B again (tmux `bind -r`
  // style). Each arrow extends the window; any non-arrow cancels it and
  // falls through. This makes `Ctrl+B → → →` cycle panes and `↑ ↓` chain
  // through sessions without re-pressing the prefix each time, and makes
  // holding an arrow key naturally drive the cycle via keydown auto-repeat.
  //
  // Capture-phase on window so this fires BEFORE xterm.js forwards the
  // keystroke to tmux (see AGENTS.md). The shouldSkipShortcut guard lets
  // real text inputs (compose, favourites search) keep their normal keys.
  useEffect(() => {
    if (!isDesktop) {
      const handler = (e: KeyboardEvent) => {
        if (!isCtrlB(e)) return;
        if (shouldSkipShortcut(e)) return;
        e.preventDefault();
        e.stopPropagation();
        e.stopImmediatePropagation();
        closeAllSheets();
        setDrawerOpen((d) => !d);
      };
      window.addEventListener("keydown", handler, { capture: true });
      return () => window.removeEventListener("keydown", handler, { capture: true });
    }

    const PREFIX_TIMEOUT_MS = 600;
    const ARROW_REPEAT_MS = 800; // window for follow-up arrows after a chord

    // Two independent timers:
    //   `armed`     = inside a fresh Ctrl+B prefix (any chord key consumed)
    //   `repeating` = follow-up window after an arrow chord (arrows-only)
    // A new Ctrl+B always supersedes any in-flight repeat.
    let armed = false;
    let armTimer: number | null = null;
    let repeating = false;
    let repeatTimer: number | null = null;

    const clearArm = () => {
      armed = false;
      // Publish it: the empty pane's switcher ([0026]) registers its own window
      // capture listener and must not also act on an armed chord key. See
      // prefix.ts.
      setPrefixArmed(false);
      if (armTimer != null) {
        window.clearTimeout(armTimer);
        armTimer = null;
      }
    };
    const clearRepeat = () => {
      repeating = false;
      if (repeatTimer != null) {
        window.clearTimeout(repeatTimer);
        repeatTimer = null;
      }
    };
    const extendRepeat = () => {
      if (repeatTimer != null) window.clearTimeout(repeatTimer);
      repeating = true;
      repeatTimer = window.setTimeout(() => {
        repeating = false;
        repeatTimer = null;
      }, ARROW_REPEAT_MS);
    };
    const openDrawer = () => {
      closeAllSheets();
      setDrawerOpen((d) => !d);
    };

    // The arrow chord behaviour is the same whether we got here from a fresh
    // Ctrl+B prefix or from the follow-up repeat window — so it lives in one
    // helper both branches call.
    const handleArrow = (k: string) => {
      const lay = layoutRef.current;
      const cur = activeRef.current;
      if (k === "ArrowLeft" || k === "ArrowRight") {
        // Count, not id: `Layout` is 1..6 but layout 5 has 2 panes and layout 6
        // has 3 (TileGrid PANE_COUNT). Wrapping modulo the id produced an
        // out-of-range index that setActive's clamp pinned back onto the last
        // pane, so Ctrl+B → was a dead key in exactly those two layouts (← wrapped
        // by accident). See docs/proposals/0081-pane-focus-navigation.md Part A.
        const n = paneCount(lay);
        if (n > 1) {
          const delta = k === "ArrowRight" ? 1 : -1;
          setActive((cur + delta + n) % n);
        }
        return;
      }
      // Up / Down — session cycle in the active pane.
      const dir: 1 | -1 = k === "ArrowDown" ? 1 : -1;
      const names = sessionsRef.current.map((x) => x.name);
      const next = cycleSessionInPane(panesRef.current, cur, names, dir);
      if (next !== null) mountAt(cur, paneRefFor(next));
    };

    // Proposal 0019 — session cycle while the file viewer owns the screen. Only
    // up/down (session cycle) makes sense over the singleton editor; left/right
    // (pane focus) does not, so it's ignored. The dirty guard lives here at the
    // switch source: an effect reacting to an already-moved pane couldn't cancel
    // it. Cancelling the confirm leaves the viewer on the current session.
    const handleViewerArrow = (k: string) => {
      if (k !== "ArrowUp" && k !== "ArrowDown") return;
      const cur = activeRef.current;
      const dir: 1 | -1 = k === "ArrowDown" ? 1 : -1;
      const names = sessionsRef.current.map((x) => x.name);
      const next = cycleSessionInPane(panesRef.current, cur, names, dir);
      if (next === null) return;
      if (editorDirtyRef.current && !window.confirm("Discard unsaved changes?")) return;
      mountAt(cur, paneRefFor(next));
    };

    const isArrow = (k: string) =>
      k === "ArrowLeft" || k === "ArrowRight" || k === "ArrowUp" || k === "ArrowDown";

    const handler = (e: KeyboardEvent) => {
      if (shouldSkipShortcut(e)) return;

      const stop = () => {
        e.preventDefault();
        e.stopPropagation();
        e.stopImmediatePropagation();
      };

      // Proposal 0019 — the editor overlay owns the whole screen and handles its
      // own keys (Esc/Cmd+S in its own capture-phase listener), so most of the
      // tmux prefix stays inert: pane numbers, the layout palette (l/Space),
      // unmount (x) and open-editor (e) have no meaning over a full-screen file
      // viewer. The exception is the *session-switch* chords, kept live so the
      // viewer can follow a session change without closing: Ctrl+B ↑/↓ cycles
      // the active session and Ctrl+B s opens the switcher over the viewer.
      if (editorOpenRef.current) {
        if (isCtrlB(e)) {
          stop();
          clearArm();
          clearRepeat();
          armed = true;
          armTimer = window.setTimeout(() => {
            armed = false;
            armTimer = null;
            openDrawer();
          }, PREFIX_TIMEOUT_MS);
          return;
        }
        if (armed) {
          const k = e.key;
          // A bare modifier keydown is not the chord key — see isModifierKey.
          if (isModifierKey(k)) return;
          if (k === "ArrowUp" || k === "ArrowDown") {
            stop();
            clearArm();
            handleViewerArrow(k);
            extendRepeat();
            return;
          }
          if (k === "s" || k === "S") {
            stop();
            clearArm();
            openDrawer();
            return;
          }
          if (k === "f" || k === "F") {
            // Find-file (proposal 0027): the viewer already owns the screen, so
            // just re-focus its Find bar in place (bump focusSearchSeq).
            stop();
            clearArm();
            openEditor(null, true);
            return;
          }
          if (k === "l" || k === "L") {
            // Copy link (proposal 0083 Part B): the URL of the file the viewer
            // has open. `l` is free HERE — over the grid it is the layout
            // palette, and a file link only exists while the viewer is up.
            stop();
            clearArm();
            setCopyLinkSeq((n) => n + 1);
            return;
          }
          if (k === "/" || k === "t" || k === "T") {
            // Filter-tree (proposal 0038): focus the in-tree "Filter tree" field.
            // "/" is the mnemonic (web-search slash); "t" is the fallback for
            // keyboard layouts where "/" needs a modifier.
            stop();
            clearArm();
            focusTreeFilter();
            return;
          }
          // Esc and every other chord key is inert over the viewer: drop the
          // prefix. (Esc-to-close is the editor's own capture-phase handler.)
          clearArm();
          return;
        }
        if (repeating) {
          const k = e.key;
          // A bare modifier keydown is not the chord key — see isModifierKey.
          if (isModifierKey(k)) return;
          if (k === "ArrowUp" || k === "ArrowDown") {
            stop();
            handleViewerArrow(k);
            extendRepeat();
            return;
          }
          clearRepeat();
          return;
        }
        return;
      }

      // While the layout palette is open it owns the keyboard. The palette's
      // onKeyDown runs in bubble phase; without this gate the window-level
      // capture handler would also chew on arrows/Enter/Esc and re-arm
      // prefixes mid-pick. See paletteOpenRef.
      if (paletteOpenRef.current) return;

      if (isCtrlB(e)) {
        e.preventDefault();
        e.stopPropagation();
        e.stopImmediatePropagation();
        clearArm();
        clearRepeat(); // a fresh prefix supersedes any in-flight repeat
        armed = true;
        setPrefixArmed(true);
        armTimer = window.setTimeout(() => {
          armed = false;
          setPrefixArmed(false);
          armTimer = null;
          openDrawer();
        }, PREFIX_TIMEOUT_MS);
        return;
      }

      if (armed) {
        const k = e.key;
        // A bare modifier keydown is not the chord key — see isModifierKey.
        if (isModifierKey(k)) return;
        const lay = layoutRef.current;

        if (k >= "1" && k <= "9") {
          const n = parseInt(k, 10) - 1;
          if (n >= 0 && n < paneCount(lay)) {
            stop();
            clearArm();
            setActive(n);
          } else {
            // Number outside current layout — cancel the prefix and let xterm
            // see the key, in case someone's typing into a TUI menu.
            clearArm();
          }
          return;
        }
        if (isArrow(k)) {
          stop();
          clearArm();
          handleArrow(k);
          extendRepeat(); // enter repeat window for follow-up arrows
          return;
        }
        if (k === " " || e.code === "Space" || k === "l" || k === "L") {
          // Open the layout palette. Space used to cycle 1→2→3→4 in place;
          // with 6 layouts now and a dedicated picker, both `l` and Space
          // converge on the same surface (one fewer chord to remember).
          stop();
          clearArm();
          closeAllSheets();
          openPalette();
          return;
        }
        if (k === "s" || k === "S") {
          // Shift discriminates inside this one case rather than adding a second
          // `if (k === "S")` further down — a later uppercase-only case is
          // unreachable, which is how [0041]'s share chord shipped dead: every
          // ⌃B ⇧S since has opened the drawer. Mirrors the ⌃B c / ⌃B ⇧C pattern
          // below. See docs/proposals/0081-pane-focus-navigation.md Part B.
          stop();
          clearArm();
          if (e.shiftKey) {
            // Share the focused pane's session (proposal 0041). Multi-tenant
            // only; on a single-tenant hub, or on an empty pane, this is a
            // deliberate no-op rather than a surprise drawer.
            if (meRef.current?.multiTenant) {
              const ref = panesRef.current[activeRef.current];
              if (ref) openShareFor(ref);
            }
            return;
          }
          openDrawer();
          return;
        }
        if (k === "x" || k === "X") {
          stop();
          clearArm();
          mountAt(activeRef.current, null);
          return;
        }
        if (k === "e" || k === "E") {
          // Open the file editor (full-screen overlay). No path yet — the
          // desktop tree lets the user pick, anchored at the active session.
          stop();
          clearArm();
          openEditor(null);
          return;
        }
        if (k === "f" || k === "F") {
          // Find file (proposal 0027): open the viewer AND focus its Find bar in
          // one step — one chord from the agent view straight to file search.
          stop();
          clearArm();
          openEditor(null, true);
          return;
        }
        if (k === "/") {
          // Find in the focused terminal (proposal 0068 Part E). Over the grid,
          // "/" means "search what I'm looking at" — the agent output — which
          // browser find-in-page can no longer do under the WebGL renderer.
          // The file-tree filter ([0038]) keeps "/" *inside* the editor overlay
          // (handled above) and keeps "t" here, its documented fallback.
          stop();
          clearArm();
          setTermSearchSeq((n) => n + 1);
          return;
        }
        if (k === "t" || k === "T") {
          // Filter tree (proposal 0038): open the viewer AND focus its "Filter
          // tree" field in one step (mirror of the find-file chord above).
          stop();
          clearArm();
          focusTreeFilter();
          return;
        }
        if (k === "c" || k === "C") {
          // Mark the focused pane's session with a colour (proposal 0029):
          // `c` re-rolls to a different palette token; `Shift+C` clears it.
          stop();
          clearArm();
          const ref = panesRef.current[activeRef.current];
          if (ref) {
            const meta = sessionsRef.current.find(
              (s) => s.name === ref.name && (s.machine ?? "") === ref.machine
            );
            markColor(ref, e.shiftKey ? null : nextSessionColor(meta?.color));
          }
          return;
        }
        if (k === "r" || k === "R") {
          // Rename the focused pane's session (proposal 0035): put its
          // identity-bar name into inline edit mode. Desktop power path for the
          // double-click affordance; mirrors the ⌃B c colour chord.
          stop();
          clearArm();
          setRenameSeq((n) => n + 1);
          return;
        }
        if (k === ";") {
          // tmux's last-pane (proposal 0081 Part D): bounce between the two
          // panes you're actually working in, instead of walking the cycle.
          // `;` is Shift+, on a Swedish layout, which is safe — isModifierKey
          // above lets the bare Shift keydown pass without cancelling the
          // prefix. extendRepeat so ⌃B ; ; ; ping-pongs like ⌃B → → →.
          stop();
          clearArm();
          toggleLastPane();
          extendRepeat();
          return;
        }
        if (k === "Escape") {
          stop();
          clearArm();
          return;
        }
        // Unrecognised key while armed: cancel prefix, let xterm have the key.
        clearArm();
        return;
      }

      // Not in a fresh prefix — are we in the post-arrow repeat window?
      if (repeating) {
        const k = e.key;
        // A bare modifier keydown is not the chord key — see isModifierKey.
        if (isModifierKey(k)) return;
        if (isArrow(k)) {
          stop();
          handleArrow(k);
          extendRepeat();
          return;
        }
        if (k === ";") {
          // last-pane repeats like an arrow — the window belongs to pane
          // navigation as a whole, so ⌃B ; ; ; ping-pongs (0081 Part D).
          stop();
          toggleLastPane();
          extendRepeat();
          return;
        }
        // Any other key while repeating: cancel and let it through to xterm.
        // This is the escape hatch — start typing into the terminal and the
        // repeat mode steps out of your way immediately.
        clearRepeat();
        return;
      }
    };

    window.addEventListener("keydown", handler, { capture: true });
    return () => {
      window.removeEventListener("keydown", handler, { capture: true });
      clearArm();
      clearRepeat();
    };
  }, [isDesktop, closeAllSheets, mountAt, setActive, toggleLastPane, openPalette, openEditor, focusTreeFilter, markColor, openShareFor]);

  // Suppress xterm.js's own paste-shortcut keydown handler.
  //
  // xterm.js converts the paste-shortcut keydown directly into a 0x16 byte
  // on the PTY's stdin — a clipboard-probing assistant sees that, runs its
  // clipboard probe, and finds nothing because our `/api/clip` POST hasn't
  // completed staging yet. Then our POST finally lands and the server injects
  // the real paste input, but it arrives after the assistant already gave up.
  //
  // Fix: stop the keydown from reaching xterm's helper-textarea listener so
  // it never sends the racing 0x16. We do NOT preventDefault — the browser's
  // default action (firing the `paste` event) still happens, so our paste
  // handler below still gets the clipboardData. Net effect: only one 0x16
  // reaches Claude Code, and it arrives *after* the image is staged.
  //
  // CRITICAL: only block the OS's *actual* paste shortcut — the one followed
  // by a real `paste` event. Browsers only fire the paste event for the
  // OS-defined shortcut:
  //   - Mac:   Cmd+V (⌘V)              — followed by `paste`
  //   - Other: Ctrl+V                    — followed by `paste`
  //   - Mac + Ctrl+V:                    — NO `paste` event, ever
  // If we blocked Ctrl+V on Mac we'd kill xterm's 0x16 but get no paste
  // event to take over — net result: dead key. Mac users who muscle-memory
  // Ctrl+V still get the old behaviour (xterm forwards 0x16, the assistant
  // probes and shows its "no clipboard image" feedback), and Cmd+V is the
  // path that actually works.
  //
  // Real text inputs (compose, favourites search) are exempted by name so
  // their native paste keeps working; xterm's helper textarea is treated as
  // the terminal, not a real input — same rule as elsewhere.
  useEffect(() => {
    const isMac = /Mac|iPad|iPhone|iPod/i.test(navigator.userAgent);
    const handler = (e: KeyboardEvent) => {
      if (e.key.toLowerCase() !== "v") return;
      const isPasteShortcut = isMac
        ? e.metaKey && !e.ctrlKey
        : e.ctrlKey && !e.metaKey;
      if (!isPasteShortcut || e.shiftKey || e.altKey) return;
      const t = e.target as HTMLElement | null;
      const tag = t?.tagName?.toLowerCase();
      const isXtermPlumbing = !!t?.classList?.contains("xterm-helper-textarea");
      const isRealInput =
        (tag === "input" || tag === "textarea" || !!t?.isContentEditable) &&
        !isXtermPlumbing;
      if (isRealInput) return; // let the input handle its own paste
      // stopPropagation (not preventDefault) — the paste event still fires.
      e.stopPropagation();
      e.stopImmediatePropagation();
    };
    window.addEventListener("keydown", handler, { capture: true });
    return () => window.removeEventListener("keydown", handler, { capture: true });
  }, []);

  // Cmd+C (Mac) / Ctrl+C (Linux/Windows) — copy the active pane's xterm
  // selection to the system clipboard.
  //
  // The whole job is **disambiguating "copy" from "interrupt"** without
  // breaking the most-used keystroke in a terminal. Rules:
  //   - selection present in active pane → copy + suppress the keydown
  //     (preventDefault stops xterm from forwarding 0x03 to the PTY *and*
  //     stops the browser's synthetic copy event, so we don't race xterm's
  //     own copy handler).
  //   - no selection → DO NOT preventDefault. xterm sends 0x03 → tmux →
  //     SIGINT. This is the only catastrophic failure mode if we get the
  //     decision wrong, so it's the default branch.
  //   - Ctrl+Shift+C (any platform) always tries to copy. Convention from
  //     gnome-terminal et al.; no SIGINT to worry about because Shift+C
  //     doesn't produce one.
  //
  // Selection comes from xterm.js's force-selection bypass of tmux mouse
  // mode. The modifier differs by platform — xterm.js's shouldForceSelection
  // honours Shift on Linux/Windows but only Option (⌥) on Mac (and only with
  // `macOptionClickForcesSelection: true`, which TerminalView enables). So:
  //   - Linux/Windows: Shift+drag selects
  //   - Mac:           Option+drag selects (plus right-click word-selects)
  // Double-/triple-click also work as usual. First-run hint below picks
  // the right modifier name based on platform so Mac users aren't sent
  // down a dead end.
  //
  // Capture phase on window for the same reason as the paste path: xterm.js's
  // helper-textarea handler stopPropagations on Ctrl-letter keys, so a bubble
  // listener would never see Ctrl+C. Capture runs before the target.
  //
  // Real text inputs (compose, favourites search) are exempted by tag so
  // their native Cmd/Ctrl+C still works; xterm's helper textarea is treated
  // as the terminal, same exemption rule used elsewhere.
  useEffect(() => {
    const isMac = /Mac|iPad|iPhone|iPod/i.test(navigator.userAgent);
    const handler = (e: KeyboardEvent) => {
      if (e.key.toLowerCase() !== "c") return;
      const macCopy = isMac && e.metaKey && !e.ctrlKey && !e.altKey && !e.shiftKey;
      const linCopy = !isMac && e.ctrlKey && !e.metaKey && !e.altKey && !e.shiftKey;
      const explicitCopy = e.ctrlKey && e.shiftKey && !e.metaKey && !e.altKey;
      if (!macCopy && !linCopy && !explicitCopy) return;

      const t = e.target as HTMLElement | null;
      const tag = t?.tagName?.toLowerCase();
      const isXtermPlumbing = !!t?.classList?.contains("xterm-helper-textarea");
      const isRealInput =
        (tag === "input" || tag === "textarea" || !!t?.isContentEditable) &&
        !isXtermPlumbing;
      if (isRealInput) return;

      // A live PROSE selection wins. The reading view and the status view render
      // real text over a grid that stays mounted behind them, so without this
      // check ⌘C on a paragraph copied whatever stale selection the terminal
      // underneath still held (proposal 0077 Part B). Terminal selections are
      // xterm's own, not the document's, so a document selection anchored
      // outside .xterm is unambiguously prose — let the browser copy it.
      const docSel = window.getSelection?.();
      if (docSel && !docSel.isCollapsed && docSel.toString().trim()) {
        const node = docSel.anchorNode;
        const el = node instanceof Element ? node : node?.parentElement ?? null;
        if (!el?.closest?.(".xterm")) return;
      }

      // The editor's agent mirror is a second terminal on screen; when the
      // selection is there, that is what the user means to copy.
      const paneTerm = termsRef.current[activeRef.current];
      const mirror = agentTermRef.current;
      const term = mirror?.hasSelection?.() ? mirror : paneTerm;
      const selection = term?.getSelection?.() ?? "";

      if (!selection) {
        // Pass through. On Linux/Win this is the SIGINT path — must NOT
        // preventDefault. On Mac, Cmd+C has no PTY meaning, so this is also
        // harmless. First-run hint only for the implicit shortcut (not
        // Ctrl+Shift+C, whose user clearly already knows what they're doing).
        if ((macCopy || linCopy) && !localStorage.getItem(COPY_HINT_KEY)) {
          try { localStorage.setItem(COPY_HINT_KEY, "1"); } catch { /* quota */ }
          // Platform-specific modifier: xterm.js's force-selection check
          // honours Shift on Linux/Windows but only Option (⌥) on Mac (see
          // macOptionClickForcesSelection in TerminalView). Telling Mac
          // users "hold Shift" would send them down a dead end.
          showToast(
            isMac
              ? "Tip — hold ⌥ Option and drag to select (or right-click a word), then ⌘C"
              : "Tip — hold Shift and drag to select, then Ctrl+C to copy",
            true
          );
        }
        return;
      }

      e.preventDefault();
      e.stopPropagation();
      e.stopImmediatePropagation();
      // 0077 A10: a copy the user just made must not be swapped out from under
      // them by an arriving OSC 52 before they get to paste it.
      noteUserCopy();
      writeClipboard(selection)
        .then(() => {
          // Match OS conventions: silent on success. Clearing the xterm
          // selection mirrors gnome-terminal — the visual "I just copied
          // that" acknowledgement without a chrome toast.
          term?.clearSelection?.();
        })
        .catch(() => showToast("Copy failed", false));
    };
    window.addEventListener("keydown", handler, { capture: true });
    return () => window.removeEventListener("keydown", handler, { capture: true });
  }, [showToast]);

  // Global Ctrl+V paste — the secure-context-free path.
  //
  // The async Clipboard API (navigator.clipboard.read) is gated to HTTPS,
  // which breaks the ImageSheet "Paste from clipboard" button on our
  // tailnet-HTTP deployment. The ClipboardEvent path (a real `paste`
  // event from a Ctrl+V keypress) is *not* gated — it's available
  // wherever the browser fires the event — so we hook it directly and
  // route any image in the payload to the active pane's session.
  //
  // Routes:
  //   image in clipboard -> POST /api/clip; the agent delivers it using the
  //     session tool's own paste contract (Claude: paste key + clipboard
  //     shim; Codex: staged file path — server-side dispatch, 0066)
  //   text only          -> POST /api/paste (bracketed paste; same path the
  //     compose sheet uses, so multi-line goes in as one block)
  //
  // Since the Ctrl+V keydown above no longer reaches xterm, the only way the
  // PTY learns about a paste is through these two routes — there's no double
  // 0x16, no race, no "nothing in clipboard" message.
  //
  // Capture-phase on window so we run BEFORE xterm.js's own paste handler
  // (which would otherwise consume the event and write its text part to
  // stdin). We skip real text inputs (compose, favourites search) by name
  // so their native text paste keeps working, and exempt xterm's helper
  // textarea by class for the same reason as the keyboard handler.
  useEffect(() => {
    const handler = (e: ClipboardEvent) => {
      const t = e.target as HTMLElement | null;
      const tag = t?.tagName?.toLowerCase();
      const isXtermPlumbing = !!t?.classList?.contains("xterm-helper-textarea");
      const isRealInput =
        (tag === "input" || tag === "textarea" || !!t?.isContentEditable) &&
        !isXtermPlumbing;
      if (isRealInput) return; // let native text paste happen in inputs

      const data = e.clipboardData;
      if (!data) return;

      const target = panesRef.current[activeRef.current] ?? null;

      // Image branch — first File-kind item with an image/* type.
      let blob: File | null = null;
      for (let i = 0; i < data.items.length; i++) {
        const it = data.items[i];
        if (it.kind === "file" && it.type.startsWith("image/")) {
          blob = it.getAsFile();
          if (blob) break;
        }
      }
      if (blob) {
        if (!target) {
          showToast("Paste failed — no active session", false);
          return;
        }
        e.preventDefault();
        e.stopPropagation();
        e.stopImmediatePropagation();
        (async () => {
          try {
            const png = await toPng(blob!);
            await sendImage(target.name, png, target.machine);
            // "Sent", not "pasted": a 204 means the agent staged + injected it;
            // the assistant itself hasn't acknowledged parsing it (0066).
            showToast("📋 Image sent", true);
          } catch (err) {
            console.error("clipboard image paste:", err);
            showToast(imageSendError(err), false);
          }
        })();
        return;
      }

      // Text branch — bracketed paste via /api/paste, same path the compose
      // sheet uses. We have to take this over too because we suppressed the
      // Ctrl+V keydown above (otherwise xterm would have done it).
      const text = data.getData("text/plain");
      if (text && target) {
        e.preventDefault();
        e.stopPropagation();
        e.stopImmediatePropagation();
        pasteText(target.name, text, false, target.machine).catch((err) => {
          console.error("clipboard text paste:", err);
          showToast("Text paste failed", false);
        });
      }
    };
    window.addEventListener("paste", handler, { capture: true });
    return () => window.removeEventListener("paste", handler, { capture: true });
  }, [showToast]);

  // Drag-and-drop file upload.
  //
  // Per-pane drop handlers live in TileGrid (PaneBox) and call back here
  // with the DataTransfer; this hub flattens folders (webkitGetAsEntry walk
  // — see api.ts) and opens the UploadSheet targeting the pane's session.
  // The session is captured at drop time, not derived from `currentSession`
  // at render time, so the user can switch panes during the upload without
  // retargeting it.
  //
  // Empty panes are filtered out: a drop on an empty pane has no project
  // root to anchor against, so we toast a hint and bail. (TileGrid also
  // refuses to render its overlay on empty panes, so this is belt-and-
  // braces — covers the case of a fast drop right after unmount.)
  // startUpload is the common tail for both upload entry points (desktop drop
  // + phone picker): stash the file list and target session, then open the
  // UploadSheet so the user picks a destination folder under the project root.
  const startUpload = useCallback(
    (pane: PaneRef | null, list: UploadFile[]) => {
      if (!pane) {
        showToast("Pick a session first", false);
        return;
      }
      if (list.length === 0) {
        showToast("No files selected", false);
        return;
      }
      setUploadPane(pane);
      setUploadFilesList(list);
      closeAllSheets();
      setUploadOpen(true);
    },
    [closeAllSheets, showToast]
  );

  const onPaneDrop = useCallback(
    async (idx: number, dt: DataTransfer) => {
      const target = panesRef.current[idx];
      if (!target) {
        showToast("Drop on an empty pane — pick a session first", false);
        return;
      }
      try {
        startUpload(target, await flattenDataTransfer(dt));
      } catch (e) {
        console.error("drop flatten:", e);
        showToast("Couldn't read the dropped files", false);
      }
    },
    [startUpload, showToast]
  );

  // Phone Upload button → native file picker → UploadSheet. A multi-select
  // input with no `accept` so iOS offers Photos, Camera, and Files alike;
  // webkitRelativePath survives a directory pick (Android/desktop) so the
  // server still rebuilds the tree. The input value is cleared after reading
  // so re-picking the same file fires `change` again.
  const onPickUpload = useCallback(
    (e: ChangeEvent<HTMLInputElement>) => {
      const input = e.currentTarget;
      const list: UploadFile[] = Array.from(input.files ?? []).map((f) => ({
        relPath: f.webkitRelativePath || f.name,
        file: f,
      }));
      input.value = "";
      startUpload(currentSession, list);
    },
    [currentSession, startUpload]
  );

  // Global "swallow stray file drops" guard. Without this, releasing a file
  // drag a few pixels outside a pane navigates the browser to view that
  // file — which on a tailnet-only single-page app means losing your
  // session list and having to reload. We only intercept when the drag
  // actually carries Files, so this never blocks in-app drags (text
  // selection, future drag-to-reorder, etc.) — only OS file drags.
  useEffect(() => {
    const isFileDrag = (e: DragEvent) =>
      !!e.dataTransfer && Array.from(e.dataTransfer.types).includes("Files");
    const onDragOver = (e: DragEvent) => {
      if (isFileDrag(e)) e.preventDefault();
    };
    const onDrop = (e: DragEvent) => {
      if (isFileDrag(e)) e.preventDefault();
    };
    window.addEventListener("dragover", onDragOver);
    window.addEventListener("drop", onDrop);
    return () => {
      window.removeEventListener("dragover", onDragOver);
      window.removeEventListener("drop", onDrop);
    };
  }, []);

  const onUploadResult = useCallback(
    (r: UploadResult) => {
      setUploadOpen(false);
      setUploadFilesList([]);
      setUploadPane(null);
      const wrote = r.written.length;
      const renamed = Object.keys(r.renamed).length;
      const errors = r.errors ? Object.keys(r.errors).length : 0;
      if (errors > 0) {
        showToast(
          `Uploaded ${wrote}, ${errors} failed${renamed ? `, ${renamed} renamed` : ""}`,
          false
        );
      } else {
        showToast(
          `Uploaded ${wrote} file${wrote === 1 ? "" : "s"}${renamed ? ` (${renamed} renamed)` : ""}`,
          true
        );
      }
    },
    [showToast]
  );

  // While a viewed session's connection is unhealthy, poll the session list.
  // When an agent exits (/exit kills its tmux session) the WebSocket drops for
  // good; this promptly drops the dead session from the list and clears its
  // pane (refresh nulls out any pane holding a session tmux no longer
  // reports). A live, momentarily-dropped session stays in the list, so a
  // transient blip won't kick you out.
  const anyUnhealthy = useMemo(
    () =>
      panes.some(
        (p, i) => p !== null && conns[i] !== "open"
      ),
    [panes, conns]
  );
  useEffect(() => {
    if (!anyUnhealthy) return;
    const id = setInterval(refresh, 2500);
    return () => clearInterval(id);
  }, [anyUnhealthy, refresh]);

  const setFont = (n: number) => {
    const v = Math.max(9, Math.min(20, n));
    setFontSize(v);
    localStorage.setItem(FONT_KEY, String(v));
  };

  // Picking from the drawer mounts the chosen session (with its owning machine)
  // in the active pane. When the file viewer is open over the grid (proposal
  // 0019) the pick retargets the viewer instead of the terminal, so guard an
  // unsaved buffer first — cancelling leaves the viewer on the current session.
  const pick = (s: Session) => {
    if (
      editorOpenRef.current &&
      editorDirtyRef.current &&
      !window.confirm("Discard unsaved changes?")
    )
      return;
    mountAt(active, { name: s.name, machine: s.machine ?? "" });
    setDrawerOpen(false);
  };

  // Delete a session: show a spinner on its row, ask the server to end it
  // (soft = inject /exit, hard = kill), then poll until tmux no longer lists
  // it and drop it from the list (and from any pane holding it).
  const removeSession = useCallback(
    async (name: string, mode: "exit" | "kill", machine = "") => {
      setDeleting((d) => new Set(d).add(name));
      // The one prune edge for the Recent list besides the 20-cap (0078 A5): the
      // user has *stated* this session is gone, so forget it. Mere absence from
      // a poll never does — that is indistinguishable from an offline machine.
      recentsStore.forget({ name, machine });
      try {
        await deleteSession(name, mode, machine);
        const deadline = Date.now() + 25000; // give a soft /exit time to wind down
        for (;;) {
          await new Promise((r) => setTimeout(r, 500));
          let list: Session[];
          try {
            list = await fetchSessions();
          } catch {
            if (Date.now() > deadline) break;
            continue;
          }
          if (!list.some((s) => s.name === name && (s.machine ?? "") === machine)) {
            setSessions(list);
            updatePanes((s) => {
              const has = s.panes.some((p) => p?.name === name && p?.machine === machine);
              if (!has) return s;
              return {
                ...s,
                panes: s.panes.map((p) =>
                  p?.name === name && p?.machine === machine ? null : p
                ),
              };
            });
            break;
          }
          if (Date.now() > deadline) break; // gave up; leave it for a force-kill
        }
      } catch {
        // ignore; the finally block refreshes the list
      } finally {
        setDeleting((d) => {
          const n = new Set(d);
          n.delete(name);
          return n;
        });
        refresh();
      }
    },
    [refresh, updatePanes, recentsStore]
  );

  // Favourites live server-side (durable, shared across devices). Load once, then
  // keep an optimistic local copy and PUT the whole list on every change,
  // adopting the server's sanitised result.
  useEffect(() => {
    if (authed !== true) return;
    fetchFavorites().then(setFavorites).catch(() => {});
  }, [authed]);
  const persistFavorites = useCallback((next: Favorite[]) => {
    setFavorites(next);
    saveFavorites(next).then(setFavorites).catch(() => {});
  }, []);
  const addFavorite = useCallback(
    (text: string) => {
      const t = text.trim();
      if (!t || favorites.some((f) => f.text === t)) return;
      const id =
        crypto.randomUUID?.() ?? `${Date.now()}-${Math.random().toString(36).slice(2)}`;
      persistFavorites([{ id, text: t }, ...favorites]);
    },
    [favorites, persistFavorites]
  );
  const updateFavorite = useCallback(
    (id: string, text: string) =>
      persistFavorites(favorites.map((f) => (f.id === id ? { ...f, text: text.trim() } : f))),
    [favorites, persistFavorites]
  );
  const deleteFavorite = useCallback(
    (id: string) => persistFavorites(favorites.filter((f) => f.id !== id)),
    [favorites, persistFavorites]
  );
  // The active session's metadata (drives the header tool/name + status dot).
  const cur = sessions.find(
    (s) => s.name === currentSession?.name && (s.machine ?? "") === currentSession?.machine
  );

  // Inject = paste the prompt into the active pane's agent AND submit it
  // (Enter), then close the sheet. One tap fires a favourite straight in.
  const injectFavorite = useCallback(
    (text: string) => {
      if (!currentSession) return;
      pasteText(currentSession.name, text, true, currentSession.machine).catch(() => {});
      setFavOpen(false);
    },
    [currentSession, showToast]
  );

  const onKey = (key: string) => {
    if (!currentSession) return;
    sendKey(currentSession.name, key, currentSession.machine).catch(() => {});
    // Keep the soft keyboard up and the cursor focused after a ControlBar tap.
    // Tapping a button blurs xterm's hidden helper textarea; on iOS that
    // dismisses the keyboard, which fires the visualViewport→appH refit and
    // jumps the agent's prompt out of view. ControlBar's mousedown-preventDefault
    // suppresses the blur on desktop, but iOS Safari doesn't honor it — so we
    // refocus the helper textarea in-gesture here (this runs inside the button's
    // click) with preventScroll (per 0004) so iOS keeps the keyboard up and the
    // view stays put. No-op on desktop where focus never left. See 0009.
    const term = termsRef.current[active];
    const ta = term?.element?.querySelector<HTMLTextAreaElement>(".xterm-helper-textarea");
    if (ta) ta.focus({ preventScroll: true });
    else term?.focus();
  };
  // Wipe the polluted scrollback that builds up when Claude Code re-renders on
  // every SIGWINCH (it writes to the normal buffer, so each redraw appends).
  const onClearHistory = () => {
    if (!currentSession) return;
    clearHistory(currentSession.name, currentSession.machine).catch(() => {});
  };
  const onSend = (text: string, enter: boolean) => {
    if (!currentSession) return;
    pasteText(currentSession.name, text, enter, currentSession.machine).catch(() => {});
  };
  // Awaited so ImageSheet can keep its preview + show a retryable error when
  // the send fails (0066) — closing optimistically used to swallow failures.
  const onImage = async (png: Blob) => {
    if (!currentSession) throw new Error("no active session");
    await sendImage(currentSession.name, png, currentSession.machine);
  };
  const conn = conns[active] ?? "closed";
  // One unified status dot: connection trouble (red) wins, else the agent is
  // working (amber) or ready for input (green). See util/agentStatus.
  const headerStatus = agentStatus(cur?.waiting ?? true, conn);
  const dot = statusDot(headerStatus);
  // Per-session WS state for the switcher: only sessions open in a pane have a
  // connection that can be "wrong"; everything else falls through to waiting.
  // Memoized so a poll tick that changes nothing hands the switcher (and through
  // it the memoized drawer/grid) the same object identity — proposal 0068 C.
  const connByRef = useMemo(() => {
    const m: Record<string, string> = {};
    panes.forEach((p, i) => {
      if (p) m[`${p.machine ?? ""}/${p.name}`] = conns[i] ?? "closed";
    });
    return m;
  }, [panes, conns]);

  // Desktop chrome auto-hide: the header (sessions ☰, conn dot, layout picker,
  // font, eraser) collapses out of view so the terminal claims the full
  // viewport, and is summoned by hovering near the top. Phone is unaffected —
  // it always wants its chrome visible, and there's no mouse to hover anyway.
  //
  // Discovery aids: visible briefly on mount, and re-summoned for ~1.6s
  // whenever the connection state changes, so the dot still announces drops.
  const [headerVisible, setHeaderVisible] = useState(true);
  const hideTimerRef = useRef<number | null>(null);
  const cancelHide = useCallback(() => {
    if (hideTimerRef.current != null) {
      window.clearTimeout(hideTimerRef.current);
      hideTimerRef.current = null;
    }
  }, []);
  const showHeader = useCallback(() => {
    cancelHide();
    setHeaderVisible(true);
  }, [cancelHide]);
  const scheduleHide = useCallback(
    (ms = 350) => {
      // Never hide while the layout palette is open — it's anchored under
      // the header trigger button and would float orphaned over the
      // terminal as the header slid out from underneath it.
      if (paletteOpenRef.current) return;
      cancelHide();
      hideTimerRef.current = window.setTimeout(() => {
        setHeaderVisible(false);
        hideTimerRef.current = null;
      }, ms);
    },
    [cancelHide]
  );
  // Brief glimpse on (re)mount or when desktop-mode flips on, then hide.
  useEffect(() => {
    if (!isDesktop) {
      cancelHide();
      setHeaderVisible(true); // phone: always-on
      return;
    }
    scheduleHide(1600);
    return cancelHide;
  }, [isDesktop, cancelHide, scheduleHide]);
  // Re-surface briefly on connection state change so the dot still works
  // as an indicator even when the header is hidden.
  useEffect(() => {
    if (!isDesktop) return;
    showHeader();
    scheduleHide(1600);
  }, [isDesktop, conn, showHeader, scheduleHide]);
  // Pin the header visible while the palette is open (it's anchored under
  // the layout trigger). On close, hand back to the normal hide schedule.
  useEffect(() => {
    if (!isDesktop) return;
    if (paletteOpen) showHeader();
    else scheduleHide(1200);
  }, [isDesktop, paletteOpen, showHeader, scheduleHide]);

  // "New session" inside an empty pane's inline switcher (proposal 0026): just
  // focus that pane. The create flow runs in place in the pane's own drawer and
  // mounts via onPaneCreated below — no sidebar to open, no create-mode token.
  const onNewForPane = useCallback((idx: number) => setActive(idx), [setActive]);
  // Stable identity for the pane corner button (proposal 0068 Part C keeps the
  // grid's props stable so its memo can hold).
  const openEditorFromPane = useCallback(() => openEditor(null), [openEditor]);

  // Mount + refresh once a session is created from the sidebar's in-drawer flow.
  // Mounts in the remembered pane (−1 = active fallback), marks a propagation
  // grace window so the immediate refresh (and the background poll) don't null
  // the pane before the hub's session list catches up, then closes the drawer.
  const onSessionCreated = useCallback(
    (session: PaneRef) => {
      setDrawerOpen(false);
      recentMounts.current.set(refKey(session), Date.now() + 15000);
      const target = newForPane >= 0 ? newForPane : active;
      mountAt(target, session);
      setActive(target);
      setNewForPane(-1);
      refresh();
    },
    [newForPane, active, mountAt, setActive, refresh]
  );

  // Same mount/grace/refresh, but for a session created straight inside an empty
  // grid pane (proposal 0026) — the target pane is explicit, so it bypasses the
  // newForPane handshake entirely.
  const onPaneCreated = useCallback(
    (idx: number, session: PaneRef) => {
      recentMounts.current.set(refKey(session), Date.now() + 15000);
      mountAt(idx, session);
      setActive(idx);
      refresh();
    },
    [mountAt, setActive, refresh]
  );

  // The non-pane-specific switcher props, hoisted once so the sidebar drawer and
  // every empty-pane switcher (proposal 0026) share one source of truth — no
  // hand-kept parallel copy to drift.
  // Memoized (proposal 0068 C): this object is spread into the memoized
  // SessionDrawer and handed to the memoized TileGrid, so rebuilding it on
  // every render would re-render both on every 4s poll tick even when the
  // payload was byte-identical.
  const paneSwitcher: PaneSwitcherProps = useMemo(
    () => ({
      sessions,
      connByRef,
      machines,
      multiMachine,
      loading,
      error,
      // The Recent section (proposal 0078): the MRU list plus the set of
      // sessions already on screen, which the drawer subtracts. Both are stable
      // references (the store notifies only on a real change; mountedKeys is
      // memoized on `panes`), so SessionDrawer's memo keeps holding.
      recents,
      mountedKeys,
      onRefresh: refresh,
      onStatus: () => setStatusOpen(true),
      createInitialMachine: currentSession?.machine || firstOnlineMachine,
      recentDirs: restorable.map((r) => r.dir),
      showLayout: isDesktop,
      onLayout: () => {
        setDrawerOpen(false);
        openPalette();
      },
      deleting,
      onDelete: removeSession,
      // Rename via the switcher (proposal 0035): build the ref from the row's
      // session (carrying its machine) and reuse the optimistic renameSession.
      onRename: (s, label) => renameSession({ name: s.name, machine: s.machine ?? "" }, label),
      // Share a session (proposal 0041) — multi-tenant only. Opens the ShareForm
      // overlay; the shared-with-me map drives the row badges.
      onShare: me?.multiTenant ? (s) => openShareFor({ name: s.name, machine: s.machine ?? "" }) : undefined,
      sharedMap: me?.multiTenant ? sharedMap : null,
      restorable,
      onRestore,
      toastsOn: toastsEnabled,
      onToggleToasts: toggleToasts,
      // Build-aware empty states + the 402 limit card (proposal 0056 A3/B2).
      multiTenant: !!me?.multiTenant,
      plan: me?.plan,
      supportEmail: me?.supportEmail ?? undefined,
      // Whether checkout is available (proposal 0058 C2): flips the session-cap
      // 402 card from a mailto to a Stripe checkout button.
      billing: me?.billing ?? false,
    }),
    [
      sessions,
      connByRef,
      machines,
      multiMachine,
      loading,
      error,
      recents,
      mountedKeys,
      refresh,
      currentSession?.machine,
      firstOnlineMachine,
      restorable,
      isDesktop,
      openPalette,
      deleting,
      removeSession,
      renameSession,
      openShareFor,
      sharedMap,
      onRestore,
      toastsEnabled,
      toggleToasts,
      me,
    ]
  );

  // The session switcher, built once and rendered in one of two places:
  //  - phone  → full-screen takeover at the app root (embedded=false)
  //  - desktop → a left-pinned slide-in sidebar over the terminal area
  //    (sidebar=true), so Ctrl+B reveals the picker without blanking the
  //    terminal you were in (proposal 0006).
  const renderDrawer = (embedded: boolean, sidebar = false, elevated = false) => (
    <SessionDrawer
      {...paneSwitcher}
      open={drawerOpen}
      embedded={embedded}
      sidebar={sidebar}
      elevated={elevated}
      current={currentSession}
      onPick={pick}
      onClose={() => setDrawerOpen(false)}
      onNew={() => setNewForPane(active)}
      onCreated={onSessionCreated}
      // The one-shot machine seed (proposal 0056 A1/A2): opens the drawer
      // straight into create mode scoped to that machine.
      createSeed={createSeed}
      onSeedConsumed={() => setCreateSeed(null)}
      // A greyed-out tool in the create picker is where the gap is felt most
      // often (proposal 0050 F4) — make it an entry point into the install
      // dialog rather than a dead end.
      onInstallTool={(m, tool) => {
        setDrawerOpen(false);
        setUpdateScope(m);
        setUpdateTools([tool]);
        setUpdateOpen(true);
      }}
    />
  );

  // Proposal 0083 Part C — `/s/<token>`, the read-only link grant. It renders
  // BEFORE the auth gate on purpose: its whole reason to exist is a reader with
  // no account. Nothing of the app comes with it: every poll above is gated on
  // `authed === true`, which an anonymous reader never is, and no drawer,
  // terminal or WebSocket is rendered — just the token, the file, and the
  // provenance banner.
  if (LINK_TOKEN) {
    return (
      <Suspense fallback={<div className="fixed inset-0 bg-bar" />}>
        <LinkView token={LINK_TOKEN} />
      </Suspense>
    );
  }

  // Auth gate (after all hooks, so the rules of hooks hold): a blank splash
  // while checking, the login screen when locked, otherwise the app.
  if (authed === null) {
    return <div className="fixed inset-0 bg-bar" />;
  }
  // Multi-tenant gate (proposal 0001): email/Google login, the /activate device-
  // approval page, and the machines dashboard. Single-tenant keeps LoginScreen.
  if (me?.multiTenant) {
    const pathname = typeof window !== "undefined" ? window.location.pathname : "/";
    const onActivate = pathname === "/activate";
    // /invite/<token> — the email-invite landing (proposal 0056 C4). Handles
    // its own unauthenticated state (AuthScreen with a hint + prefill).
    // /org-invite/<token> — the team-invite landing (proposal 0065 C4), the
    // same component with org-specific copy + the consent line.
    const inviteToken = pathname.startsWith("/invite/") ? pathname.slice("/invite/".length) : "";
    const orgInviteToken = pathname.startsWith("/org-invite/")
      ? pathname.slice("/org-invite/".length)
      : "";
    if (inviteToken || orgInviteToken) {
      return (
        <InviteLanding
          token={inviteToken || orgInviteToken}
          org={!!orgInviteToken}
          me={me}
          onAuthed={refetchMe}
          onDone={() => {
            window.history.replaceState({}, "", "/");
            setShowDash(false);
            refetchMe();
          }}
          onLoggedOut={() => setAuthed(false)}
        />
      );
    }
    // Open the create flow pre-scoped to a machine (proposal 0056 A1/A2): leave
    // any full-screen page, remember the target pane, seed the drawer.
    const startSessionOn = (m: string) => {
      window.history.replaceState({}, "", "/");
      setShowDash(false);
      setNewForPane(active);
      setCreateSeed(m);
      setDrawerOpen(true);
    };
    if (!authed) {
      return (
        <AuthScreen
          google={me.googleEnabled}
          password={me.passwordLogin !== false}
          hint={
            onActivate
              ? "Sign in to approve a device."
              : LOGIN_ERROR
                ? LOGIN_ERROR_HINT[LOGIN_ERROR] ?? "Sign-in didn't complete — try once more."
                : undefined
          }
          onAuthed={refetchMe}
        />
      );
    }
    if (onActivate) {
      return (
        <ActivatePage
          email={me.email}
          plan={me.plan}
          support={me.supportEmail}
          billing={me.billing}
          onDone={() => {
            window.history.replaceState({}, "", "/");
            setShowDash(true);
          }}
          // A machine that just enrolled short of CLIs can fix it right here
          // (proposal 0050 F3) — the same dialog, scoped to that box.
          onInstall={(m, tools) => {
            window.history.replaceState({}, "", "/");
            setUpdateScope(m);
            setUpdateTools(tools ?? []);
            setUpdateOpen(true);
          }}
          // Activation ends in a live terminal (proposal 0056 A1).
          onStartSession={startSessionOn}
        />
      );
    }
    if (showDash) {
      return (
        <Dashboard
          me={me}
          billingPending={billingPending}
          // Membership changes (join/leave/start a team, proposal 0065 C) flip
          // the /api/me plan+org blocks — re-read so the cards catch up.
          onMeChanged={refetchMe}
          // Per-machine entry into the assistant update (0049): the dashboard is
          // a full-screen view, so hand back to the terminal with the flow open
          // and scoped to that machine.
          onUpdateAssistants={(m, tools) => {
            setUpdateScope(m);
            setUpdateTools(tools ?? []);
            setShowDash(false);
            setUpdateOpen(true);
          }}
          // "New session" on a machine row (proposal 0056 A2).
          onStartSession={startSessionOn}
          onClose={() => setShowDash(false)}
          onLoggedOut={() => {
            setShowDash(false);
            setAuthed(false);
          }}
        />
      );
    }
  } else if (!authed) {
    return <LoginScreen onSuccess={() => setAuthed(true)} />;
  }

  // A file deep link is still resolving (proposal 0083 Part A). Show nothing
  // rather than the terminal grid: you asked for a file, and a beat of someone
  // else's session flashing past — and, on a phone, the drawer and the session
  // you were last in — is exactly the noise the link exists to skip. It also
  // makes the open FASTER: no terminal mounts, no WebSocket dials, nothing
  // competes with the file read for the main thread. Cleared whether the
  // resolve succeeds or fails, so this can never be a dead end.
  if (fileLinkPending) {
    return <div className="fixed inset-0 bg-bar" />;
  }

  return (
    <div
      className="relative flex flex-col bg-bar text-slate-200"
      style={{ height: appH ? `${appH}px` : "100%" }}
    >
      {/* Multi-tenant account entry (the machines dashboard, proposal 0001) lives
          as an in-flow chrome button — desktop header + phone footer below
          (proposal 0043) — not a floating pill that overlapped the canvas/footer. */}

      {/* Hover sensor: invisible strip at the very top that summons the
          collapsed header on desktop. Phone never collapses, so no sensor. */}
      {isDesktop && (
        <div
          className="absolute left-0 right-0 top-0 z-30 h-3"
          onMouseEnter={showHeader}
        />
      )}

      {/* Header — collapses out of flow on desktop (position: absolute +
          translateY off-screen when hidden), so the terminal claims the
          space underneath. On phone it stays in flow as before. */}
      <header
        onMouseEnter={isDesktop ? showHeader : undefined}
        onMouseLeave={isDesktop ? () => scheduleHide() : undefined}
        className={`flex items-center gap-2 border-b border-edge px-3 py-2 pt-safe ${
          isDesktop
            ? `absolute inset-x-0 top-0 z-30 bg-bar/95 backdrop-blur-sm transition-transform duration-200 ease-out ${
                headerVisible ? "translate-y-0" : "-translate-y-full"
              }`
            : "bg-bar"
        }`}
      >
        <button
          onClick={() => setDrawerOpen(true)}
          aria-label="Open sessions"
          title="Sessions (Ctrl+B)"
          className="flex min-w-0 flex-1 items-center gap-2 rounded-lg bg-panel px-3 py-2 active:bg-edge"
        >
          <span className="text-slate-400">☰</span>
          {cur ? (
            <>
              <span
                className={`rounded px-1.5 py-0.5 text-[10px] font-bold uppercase text-bar ${toolColor(
                  cur.tool
                )}`}
              >
                {cur.tool}
              </span>
              <span className="truncate font-medium text-slate-100">{displayName(cur)}</span>
            </>
          ) : (
            <span className="text-slate-400">Pick a session</span>
          )}
        </button>

        {/* Mark-colour button (proposal 0029) — assigns the active session a
            colour from the curated palette (click re-rolls; Shift-click clears).
            Same action as the ⌃B c chord and the per-pane bar button. Hollow ring
            when unmarked, filled swatch when marked. */}
        {cur &&
          (() => {
            const acc = sessionAccent(cur.color);
            return (
              <button
                onClick={(e) => markColor(currentSession, e.shiftKey ? null : nextSessionColor(cur.color))}
                aria-label="Mark session colour"
                title="Mark colour (⌃B c) — Shift-click to clear"
                className="flex items-center justify-center rounded-lg bg-panel px-2.5 py-2 active:bg-edge"
              >
                <span
                  aria-hidden
                  className="h-3.5 w-3.5 rounded-full"
                  style={
                    acc
                      ? { background: acc.swatch }
                      : { border: "1.5px solid rgb(100 116 139)" }
                  }
                />
              </button>
            );
          })()}

        {/* `data-conn` is the WebSocket attach state of the active pane, exposed
            for the smoke harness only (no visual effect). This dot used to carry
            it as `title`, until the title became the agent's *status*; the E2E
            waits on `[title="open"]` silently stopped matching anything and sat
            on their timeouts. See docs/proposals/0081-pane-focus-navigation.md. */}
        <span
          className={`h-2.5 w-2.5 rounded-full ${dot}`}
          title={statusTitle(headerStatus)}
          data-conn={conn}
        />

        {/* Session-status overview (proposal 0022): what every session needs, at a
            glance. Always available (phone + desktop). */}
        <button
          onClick={() => setStatusOpen(true)}
          aria-label="Session status overview"
          title="Status — what each session needs"
          className="flex items-center justify-center rounded-lg bg-panel px-2.5 py-2 text-slate-300 active:bg-edge"
        >
          <StatusListIcon className="h-5 w-5" />
        </button>

        {/* Find in the terminal (proposal 0068 Part E). Phones have no ⌃B
            chord and no hardware keyboard, so the affordance lives here;
            desktop uses ⌃B / (and keeps this bar uncluttered). */}
        {!isDesktop && currentSession && (
          <button
            onClick={() => setTermSearchSeq((n) => n + 1)}
            aria-label="Find in terminal"
            title="Find in the terminal output"
            className="flex items-center justify-center rounded-lg bg-panel px-2.5 py-2 text-slate-300 active:bg-edge"
          >
            <SearchIcon className="h-5 w-5" />
          </button>
        )}

        {/* Share-invite inbox (proposal 0041) — multi-tenant only. Accepting an
            invite refreshes the roster + shared-with-me feed so the shared
            machine/session shows up with its badge. */}
        {me?.multiTenant && (
          <InboxButton
            isDesktop={isDesktop}
            className="flex items-center justify-center rounded-lg bg-panel px-2.5 py-2 text-slate-300 active:bg-edge"
            onAccepted={() => {
              refresh();
              fetchMachines().then(setMachines).catch(() => {});
              refreshReceivedShares();
            }}
          />
        )}

        {isDesktop && (
          <div className="relative">
            {/* Wire the trigger to whichever side of the toggle is next.
                Combined with LayoutPalette's data-layout-trigger exemption,
                this makes a second click on the button cleanly close it. */}
            <LayoutPicker
              layout={layout}
              onOpen={paletteOpen ? closePalette : openPalette}
            />
            {paletteOpen && (
              <LayoutPalette
                current={layout}
                onPick={setLayout}
                onClose={closePalette}
              />
            )}
          </div>
        )}

        <div className="flex items-center overflow-hidden rounded-lg bg-panel">
          <button onClick={() => setFont(fontSize - 1)} className="px-3 py-2 text-slate-300 active:bg-edge">
            A−
          </button>
          <button onClick={() => setFont(fontSize + 1)} className="px-3 py-2 text-slate-300 active:bg-edge">
            A+
          </button>
        </div>

        {/* File editor / browser — desktop top-bar entry point, mirroring the
            per-pane corner button (same FileEditIcon + accent, same action).
            Phones use the footer ⬇ instead. Not gated on a session: the editor
            opens in browse mode and the tree falls back to Home/share when no
            pane is attached. */}
        {isDesktop && (
          <button
            onClick={() => openEditor(null)}
            aria-label="Open file browser / editor"
            title="Files — browse, view, edit, download"
            className="flex items-center justify-center rounded-lg bg-panel px-2.5 py-2 text-accent active:bg-edge"
          >
            <FileEditIcon className="h-5 w-5" />
          </button>
        )}

        {/* Favourites: desktop-only entry point. Phone has its own button in the
            footer. Not gated on a session — opening the sheet to add/edit
            favourites is useful even with no pane attached; Inject is already a
            no-op without a session. */}
        {isDesktop && (
          <button
            onClick={() => setFavOpen(true)}
            aria-label="Favourite prompts"
            title="Favourite prompts"
            className="flex items-center justify-center rounded-lg bg-panel px-2.5 py-2 text-amber active:bg-edge"
          >
            <StarIcon filled className="h-5 w-5" />
          </button>
        )}

        {/* Machines & account (proposal 0043): the old floating pill's role, now a
            desktop header peer of Status/Files/Favourites. Phone routes it to the
            footer instead. Multi-tenant only — single-tenant header unchanged. */}
        {isDesktop && me?.multiTenant && (
          <button
            onClick={() => setShowDash(true)}
            aria-label="Your machines & account"
            title="Machines & account"
            className="relative flex items-center justify-center rounded-lg bg-panel px-2.5 py-2 text-slate-300 hover:text-accent active:bg-edge"
          >
            <ServerIcon className="h-5 w-5" />
            <span
              aria-hidden
              className={`absolute right-1 top-1 h-1.5 w-1.5 rounded-full ${
                anyMachineOnline ? "bg-amber" : "border border-edge"
              }`}
            />
          </button>
        )}

        {/* Update the coding assistants, then restart their sessions (proposal
            0049). A plain action at rest — the dot + spin appear only while a
            job is running, so it never reads as an unread badge. Shown on phone
            too: that's the surface with no terminal to SSH from. */}
        <button
          onClick={() => setUpdateOpen(true)}
          aria-label="Update coding assistants"
          title="Update coding assistants & restart sessions"
          className="relative flex items-center justify-center rounded-lg bg-panel px-2.5 py-2 text-slate-300 hover:text-accent active:bg-edge"
        >
          <RefreshIcon className={`h-5 w-5 ${updateBusy ? "animate-spin-slow" : ""}`} />
          {updateBusy && (
            <span aria-hidden className="absolute right-1 top-1 h-1.5 w-1.5 rounded-full bg-amber" />
          )}
        </button>

        <button
          onClick={onClearHistory}
          disabled={!currentSession}
          aria-label="Clear scrollback for this session"
          title="Clear scrollback"
          className="flex items-center justify-center rounded-lg bg-panel px-2.5 py-2 text-slate-300 active:bg-edge disabled:opacity-40"
        >
          <EraserIcon className="h-5 w-5" />
        </button>
      </header>

      {/* Agent/phone-view mark spine (proposal 0029): a thin colour bar under the
          header when the active session is marked — the single-pane analogue of
          the desktop pane's mark border. Quiet, never a full-surface wash. */}
      {!isDesktop && cur && sessionAccent(cur.color) && (
        <div
          aria-hidden
          className="shrink-0"
          style={{ height: 2, background: sessionAccent(cur.color)!.border }}
        />
      )}

      {/* Terminal(s) */}
      <main className="relative min-h-0 flex-1">
        {isDesktop ? (
          <TileGrid
            layout={layout}
            panes={panes}
            active={active}
            sessions={sessions}
            machines={machines}
            fontSize={fontSize}
            onActivate={setActive}
            onConn={setPaneConn}
            onPickFor={mountAt}
            onNewFor={onNewForPane}
            onPaneCreated={onPaneCreated}
            onOpenEditor={openEditorFromPane}
            onMarkColor={markColor}
            onRename={renameSession}
            renameSeq={renameSeq}
            searchSeq={termSearchSeq}
            onTermFor={setPaneTerm}
            onDropFiles={onPaneDrop}
            switcher={paneSwitcher}
            // Empty panes yield the keyboard while the sidebar switcher or the
            // file viewer is up — otherwise two window-capture handlers fight
            // over ↑/↓/⏎ (proposal 0026).
            gridKeyboardActive={!drawerOpen && !editor.open}
          />
        ) : currentSession ? (
          // Phone path: one terminal, single pane — but it shows `panes[active]`
          // (see currentSession), and every shared read keys off `active`: the
          // header dot reads conns[active], the copy handler reads
          // termsRef[active], agentCols/Rows read termsRef[active]. So report
          // this pane's connection and terminal into the SAME `active` slot, not
          // a hardcoded 0 — otherwise, whenever a persisted layout leaves
          // `active` non-zero, the dot reads an untouched slot and stays red
          // while the socket is wide open (and copy reads an empty slot).
          <TerminalView
            key={`${currentSession.machine}/${currentSession.name}`}
            session={currentSession.name}
            machine={currentSession.machine}
            fontSize={fontSize}
            onState={onPhoneConn}
            onTerm={onPhoneTerm}
            searchSignal={termSearchSeq}
          />
        ) : (
          <div className="flex h-full items-center justify-center px-8 text-center text-sm text-slate-500">
            No session selected. Tap ☰ to choose one.
          </div>
        )}

        {/* No global download icon on desktop — each pane has its own that
            fades in on mouse activity. See PaneBox in TileGrid.tsx. */}

        {/* Desktop session switcher: a left slide-in sidebar over the terminal
            area (proposal 0006). It lives in <main> (anchored below the
            collapsing header) and overlays the grid without resizing it, so the
            width-locked PTY is never re-pinned. A faint scrim makes click-outside
            -to-close obvious; it sits below the sidebar's z-30 and above the grid. */}
        {isDesktop && (
          <>
            {drawerOpen && (
              <div
                className={`absolute inset-0 bg-black/20 ${
                  editor.open ? "z-[64]" : "z-20"
                }`}
                onClick={() => setDrawerOpen(false)}
                aria-hidden
              />
            )}
            {/* When the file viewer (z-[60]) is up, the switcher must render
                above it (proposal 0019) so Ctrl+B s / the toolbar button reveal
                a usable switcher rather than one hidden behind the viewer. */}
            {renderDrawer(true, true, editor.open)}
          </>
        )}
      </main>

      {/* Footer: phone only — control keys + compose + file transfer /
          favourites buttons. On desktop you have a hardware keyboard for those
          keys, Ctrl/Cmd+V for image paste, and drag-and-drop upload + the
          floating download above. Download (⬇) and Upload (⬆) sit together as a
          file-transfer pair; Image (🖼) pastes inline into the terminal, which
          is a different action from uploading a file to the project. */}
      {!isDesktop && (
        <>
          <ControlBar onKey={onKey} disabled={!currentSession} />
          {/* Hidden picker the Upload button triggers; multiple + no accept so
              iOS offers Photos / Camera / Files. */}
          <input
            ref={uploadInputRef}
            type="file"
            multiple
            className="hidden"
            onChange={onPickUpload}
          />
          <div className="flex gap-2 border-t border-edge bg-bar px-2 py-2 pb-safe">
            {/* Machines & account (proposal 0043): on phones the dashboard entry is
                a footer sibling — in the row, flowing with pb-safe — not a pill
                floating over it. First (left) for thumb reach. Multi-tenant only. */}
            {me?.multiTenant && (
              <button
                onClick={() => setShowDash(true)}
                className="relative flex items-center justify-center rounded-lg bg-panel px-3 py-3 text-slate-300 active:bg-edge"
                aria-label="Your machines & account"
              >
                <ServerIcon className="h-5 w-5" />
                <span
                  aria-hidden
                  className={`absolute right-1 top-1 h-1.5 w-1.5 rounded-full ${
                    anyMachineOnline ? "bg-amber" : "border border-edge"
                  }`}
                />
              </button>
            )}
            <button
              onClick={() => openEditor(null)}
              className="flex items-center justify-center rounded-lg bg-panel px-3 py-3 text-slate-300 active:bg-edge"
              aria-label="Browse, view and download files"
            >
              <DownloadIcon className="h-5 w-5" />
            </button>
            <button
              onClick={() => uploadInputRef.current?.click()}
              disabled={!currentSession}
              className="flex items-center justify-center rounded-lg bg-panel px-3 py-3 text-slate-300 active:bg-edge disabled:opacity-40"
              aria-label="Upload files or photos"
            >
              <UploadIcon className="h-5 w-5" />
            </button>
            <button
              onClick={() => setImageOpen(true)}
              disabled={!currentSession}
              className="flex items-center justify-center rounded-lg bg-panel px-3 py-3 text-slate-300 active:bg-edge disabled:opacity-40"
              aria-label="Paste an image into the terminal"
            >
              <ImageIcon className="h-5 w-5" />
            </button>
            <button
              onClick={() => {
                setFavOpen(true);
                favRef.current?.focus(); // focus in-gesture so iOS shows the keyboard
              }}
              disabled={!currentSession}
              className="flex items-center justify-center rounded-lg bg-panel px-3 py-3 text-amber active:bg-edge disabled:opacity-40"
              aria-label="Favourite prompts"
            >
              <StarIcon filled className="h-5 w-5" />
            </button>
            <button
              onClick={() => {
                setComposeOpen(true);
                composeRef.current?.focus(); // focus in-gesture so iOS shows the keyboard
              }}
              disabled={!currentSession}
              className="flex min-w-0 flex-1 items-center gap-2 rounded-lg bg-panel px-4 py-3 text-left text-sm text-slate-400 active:bg-edge disabled:opacity-40"
            >
              <PencilIcon className="h-4 w-4 shrink-0" />
              <span className="truncate">Write a prompt…</span>
            </button>
          </div>
        </>
      )}

      {/* Phone: full-screen switcher. Desktop renders the switcher as the
          left slide-in sidebar (above) plus, for empty grid panes, inline via
          TileGrid's pane variant (proposal 0026). */}
      {!isDesktop && renderDrawer(false)}
      <ComposeSheet
        ref={composeRef}
        open={composeOpen}
        favorites={favorites}
        onClose={() => setComposeOpen(false)}
        onSend={onSend}
      />
      <ImageSheet open={imageOpen} onClose={() => setImageOpen(false)} onSend={onImage} />
      <UploadSheet
        open={uploadOpen}
        session={uploadPane?.name ?? null}
        machine={uploadPane?.machine ?? ""}
        files={uploadFilesList}
        onClose={() => {
          setUploadOpen(false);
          setUploadFilesList([]);
          setUploadPane(null);
        }}
        onResult={onUploadResult}
      />
      {editor.open && (
        // A solid cover, not `null`: the overlay is a lazy chunk, and on a cold
        // open (a deep link, a phone on mobile data) an empty fallback lets the
        // terminal grid show through for as long as the chunk takes to arrive.
        // The overlay is opaque and full-screen, so its placeholder should be too.
        <Suspense fallback={<div className="fixed inset-0 z-[60] bg-bar" />}>
          <EditorOverlay
            open={editor.open}
            initialPath={editor.path}
            // Proposal 0083: the folder form of a deep link — the tree opens
            // here (and its ancestors expand) instead of a file buffer.
            initialDir={editor.dir}
            session={currentSession?.name ?? null}
            machines={machines}
            multiMachine={multiMachine}
            // A deep link names the machine whose $HOME the path belongs to and
            // must win over the pane-derived default; "" falls back to today's.
            initialMachine={editor.machine || currentSession?.machine || firstOnlineMachine}
            agentMachine={currentSession?.machine ?? ""}
            isDesktop={isDesktop}
            onClose={closeEditor}
            // The agent mirror renders at the active pane's true grid size so it
            // never has to report a size of its own (which would re-pin the
            // width-locked PTY). Falls back to 80×24 if the term isn't ready.
            agentCols={termsRef.current[active]?.cols}
            agentRows={termsRef.current[active]?.rows}
            termFontSize={fontSize}
            // Proposal 0019: let the viewer follow a session switch. Desktop
            // only — the toolbar button + Ctrl+B s open the same SessionDrawer
            // over the viewer; onDirtyChange feeds the source-side switch guard.
            onOpenSwitcher={isDesktop ? () => setDrawerOpen(true) : undefined}
            onDirtyChange={onEditorDirtyChange}
            // Proposal 0077 B: the mirror's xterm, so ⌘C can copy a selection
            // made in the editor's agent column.
            onAgentTerm={setAgentTerm}
            // Proposal 0083 Part C: mint a read-only file link. Multi-tenant
            // only — the grant lives in the hub's store, and a single-tenant
            // hub has none, so the menu item simply isn't offered there.
            onShareLink={me?.multiTenant ? openShareLink : undefined}
            // Proposal 0083 Part B: the overlay reports where it is pointing so
            // App can mirror it into the address bar (one writer, up here).
            onLocation={onEditorLocation}
            // Proposal 0083 Part B: ⌃B l bumps this to copy the open file's
            // link. A broadcast counter consumed by the receiver, per [0081]'s
            // rule — never a prop swapped per target.
            copyLinkSeq={copyLinkSeq}
            // Proposal 0027: Ctrl+B f bumps this to focus the in-tree Find bar.
            focusSearchSeq={editor.focusSearchSeq}
            // Proposal 0038: Ctrl+B / bumps this to focus the tree-filter field.
            focusTreeFilterSeq={editor.focusTreeFilterSeq}
          />
        </Suspense>
      )}
      <FavoritesSheet
        ref={favRef}
        open={favOpen}
        onClose={() => setFavOpen(false)}
        favorites={favorites}
        onInject={injectFavorite}
        onAdd={addFavorite}
        onUpdate={updateFavorite}
        onDelete={deleteFavorite}
      />

      {/* Share a session (proposal 0041): a centered ShareForm overlay opened from
          the switcher row, the pane identity bar, or ⌃B S. One overlay, every
          entry point. */}
      {shareTarget && (
        <div
          className="fixed inset-0 z-[70] flex items-start justify-center bg-black/50 p-4 pt-[18vh] backdrop-blur-sm"
          onClick={() => setShareTarget(null)}
        >
          <div className="w-full max-w-sm" onClick={(e) => e.stopPropagation()}>
            <ShareForm
              subject={shareTarget}
              onClose={() => setShareTarget(null)}
              onShared={refreshReceivedShares}
              /* Whether THIS hub emails the invite (proposal 0073 D1) — a
                 per-hub capability off /api/me, drilled in because there is no
                 context in this app. Absent on a hub with no mailer, where the
                 copyable link stays the only channel. */
              mail={me?.mail}
            />
          </div>
        </div>
      )}

      {/* Searchable Session × Status overview (proposal 0022). Reads off the same
          /api/sessions poll; picking a row mounts that session. */}
      <StatusView
        open={statusOpen}
        sessions={sessions}
        machines={machines}
        multiMachine={multiMachine}
        multiTenant={!!me?.multiTenant}
        onClose={() => setStatusOpen(false)}
        onPick={(s) => {
          pick(s);
          setStatusOpen(false);
        }}
      />

      {/* Update coding assistants → restart their sessions (proposal 0049).
          One surface, two states: the confirmation dialog becomes the live
          progress panel. The job is server state, so closing this only stops
          watching. */}
      <UpdateAssistants
        open={updateOpen}
        machines={machines}
        scopeMachine={updateScope || undefined}
        scopeTools={updateTools.length ? updateTools : undefined}
        sessions={sessions}
        onClose={() => {
          setUpdateOpen(false);
          setUpdateScope("");
          setUpdateTools([]);
        }}
        onBusyChange={setUpdateBusy}
        onDone={() => {
          // Panes re-attach to the restarted sessions (names are unchanged) on
          // the next list; pull it now rather than waiting out the interval.
          refresh();
          fetchMachines().then(setMachines).catch(() => {});
        }}
      />

      {/* In-app session-ready toasts (proposal 0017). Fed gated busy→waiting
          edges by the detector effect above; retracts a toast once its session
          is mounted. Routes a click through the same openSessionByName mount
          path as a notification tap / deep link. */}
      <ToastHost
        ref={toastHostRef}
        isDesktop={isDesktop}
        mountedKeys={mountedKeys}
        onOpen={(name) => { openSessionByName(name).catch(() => {}); }}
        onOverflow={() => setDrawerOpen(true)}
      />

      {/* Clipboard writes arriving FROM a session (proposal 0077 Part A): the
          click-to-copy recovery toast for anything we refuse to write
          silently, the confirmation for one we did, and the sticky banner a
          flooding session earns. Separate from the toast below because that
          one is a single pointer-events-none slot and cannot host a button. */}
      <ClipboardOfferHost />

      {/* Transient feedback (paste confirmation, future one-shots). */}
      {toast && (
        <div
          role="status"
          aria-live="polite"
          className={`pointer-events-none absolute bottom-24 left-1/2 z-50 -translate-x-1/2 rounded-full px-4 py-2 text-sm font-medium shadow-lg backdrop-blur-sm ${
            toast.ok
              ? "bg-emerald-500/85 text-bar"
              : "bg-red-500/85 text-bar"
          }`}
        >
          {toast.msg}
        </div>
      )}
    </div>
  );
}
