import { lazy, Suspense, useCallback, useEffect, useMemo, useRef, useState, type ChangeEvent } from "react";
import type { Terminal } from "@xterm/xterm";
import {
  clearHistory,
  deleteSession,
  fetchFavorites,
  fetchRestorable,
  fetchSessions,
  flattenDataTransfer,
  getAuthStatus,
  pasteText,
  restoreSessions,
  saveFavorites,
  sendImage,
  sendKey,
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
import {
  cycleSessionInPane,
  LAST_KEY,
  loadPaneState,
  PANES_KEY,
  type PaneState,
} from "./paneState";
import TerminalView, { type ConnState } from "./components/TerminalView";
import SessionDrawer from "./components/SessionDrawer";
import ControlBar from "./components/ControlBar";
import ComposeSheet, { type ComposeHandle } from "./components/ComposeSheet";
import ImageSheet from "./components/ImageSheet";
import FavoritesSheet, { type FavoritesHandle } from "./components/FavoritesSheet";
import NewSessionPanel from "./components/NewSessionPanel";
import TileGrid, { type Layout, paneCount } from "./components/TileGrid";
import LayoutPicker from "./components/LayoutPicker";
import LayoutPalette from "./components/LayoutPalette";
import UploadSheet from "./components/UploadSheet";
import LoginScreen from "./components/LoginScreen";
// The editor pulls in CodeMirror + react-markdown — a big chunk only needed
// once the user actually opens a file. Lazy-load it so the terminal app's
// initial bundle stays light.
const EditorOverlay = lazy(() => import("./components/EditorOverlay"));
import { agentStatus, statusDot, statusTitle, toolColor, toPng, writeClipboard } from "./util";
import { DownloadIcon, EraserIcon, FileEditIcon, ImageIcon, PencilIcon, StarIcon, UploadIcon } from "./icons";

const FONT_KEY = "ccweb.fontSize";
// One-shot "how to select" hint. `.v2` because the v1 wording said
// "Shift+drag" universally — wrong on Mac, where the modifier is Option.
// Bumping the key re-shows the corrected hint to users who already
// dismissed v1.
const COPY_HINT_KEY = "ccweb.copyHintSeen.v2";

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

// shouldSkipShortcut returns true when focus is in a real text input (compose
// textarea, favourites search, etc.) — but NOT when focus is in xterm.js's
// hidden helper textarea, which is technically a textarea but is really just
// the terminal. See AGENTS.md "xterm.js routes all keystrokes through a hidden
// <textarea>" for the full footgun explanation.
function shouldSkipShortcut(e: KeyboardEvent): boolean {
  const t = e.target as HTMLElement | null;
  const tag = t?.tagName?.toLowerCase();
  const isXtermPlumbing = !!t?.classList?.contains("xterm-helper-textarea");
  return (
    (tag === "input" || tag === "textarea" || !!t?.isContentEditable) &&
    !isXtermPlumbing
  );
}

export default function App() {
  const isDesktop = useIsDesktop();

  const [sessions, setSessions] = useState<Session[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Auth gate (opt-in server-side). null = still checking; false = show the
  // login screen; true = authed (or auth is off). The session cookie rides all
  // requests automatically, so the rest of the app is unchanged.
  const [authed, setAuthed] = useState<boolean | null>(null);
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

  // The whole multi-pane state lives in one object; persisted as one blob.
  const [paneState, setPaneState] = useState<PaneState>(loadPaneState);
  const { layout, panes, active } = paneState;
  const currentSession = panes[active] ?? null;

  // Mirror layout/active/sessions/panes into refs so the keyboard handler can
  // read fresh values without re-binding (which would reset its in-flight
  // prefix timer mid-chord). State drives rendering; refs drive the handler.
  const layoutRef = useRef(layout);
  const activeRef = useRef(active);
  const sessionsRef = useRef<Session[]>([]);
  const panesRef = useRef<(PaneRef | null)[]>(panes);
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
  // Live xterm.js instance per pane, populated by TerminalView's onTerm
  // callback. The global Cmd/Ctrl+C handler reads the active slot to decide
  // whether there's a selection to copy. Length 4 matches the max layout.
  const termsRef = useRef<(Terminal | null)[]>([null, null, null, null]);
  useEffect(() => { layoutRef.current = layout; }, [layout]);
  useEffect(() => { activeRef.current = active; }, [active]);
  useEffect(() => { panesRef.current = panes; }, [panes]);

  const [drawerOpen, setDrawerOpen] = useState(false);
  const [composeOpen, setComposeOpen] = useState(false);
  const [imageOpen, setImageOpen] = useState(false);
  const [favOpen, setFavOpen] = useState(false);
  // The file editor is a SINGLETON, app-wide overlay — not per-pane (desktop can
  // show up to 4 terminals, but only ever one editor, covering the whole
  // screen). `path` is the file to open; null means "let the user pick from the
  // desktop tree" (the Ctrl+B e entry). editorOpenRef shadows it so the global
  // keyboard handler can go inert while the editor owns the screen.
  const [editor, setEditor] = useState<{ open: boolean; path: string | null }>({
    open: false,
    path: null,
  });
  const editorOpenRef = useRef(false);
  useEffect(() => { editorOpenRef.current = editor.open; }, [editor.open]);
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
  // When the New-Session panel opens we remember which pane to mount the
  // newly-created session into. -1 means "phone path / default — pane 0".
  const [newOpen, setNewOpen] = useState(false);
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

  const setActive = useCallback(
    (idx: number) =>
      updatePanes((s) => ({
        ...s,
        active: Math.max(0, Math.min(paneCount(s.layout) - 1, idx)),
      })),
    [updatePanes]
  );

  // setLayout grows/shrinks the panes array to match paneCount(l). Growing
  // fills with nulls. Shrinking: if the active pane's index falls outside
  // the new range, the user's focused session is migrated into the last
  // surviving slot (overwriting whatever was there) before truncation — so
  // changing to single-pane while focused on pane 3 of a quad doesn't
  // silently nuke the session the user was looking at. The sessions in the
  // other dropped slots are still alive in tmux; the drawer is the recovery
  // path. Active is then clamped into the new range.
  const setLayout = useCallback(
    (l: Layout) =>
      updatePanes((s) => {
        const newCount = paneCount(l);
        let next = s.panes.slice();
        if (s.active >= newCount && next[s.active]) {
          // Promote the focused session into the last surviving slot.
          next[newCount - 1] = next[s.active]!;
        }
        next = Array.from({ length: newCount }, (_, i) => next[i] ?? null);
        const active = Math.max(0, Math.min(newCount - 1, s.active));
        return { layout: l, panes: next, active };
      }),
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
    setNewOpen(false);
    setUploadOpen(false);
    closePalette();
  }, [closePalette]);

  // openEditor surfaces the singleton editor overlay (closing any sheet first
  // so it doesn't peek through). `path` null = desktop tree-pick entry.
  const openEditor = useCallback(
    (path: string | null) => {
      closeAllSheets();
      setEditor({ open: true, path });
    },
    [closeAllSheets]
  );
  const closeEditor = useCallback(() => setEditor({ open: false, path: null }), []);

  // Sessions just created via New Session, keyed `machine/name` → grace expiry
  // (ms). A create confirms the agent made the session, but the hub's union list
  // can lag a push behind — so for a short window we DON'T let applySessionList
  // null the freshly-mounted pane merely because the session hasn't propagated
  // yet (which would bounce the user to the switcher). Cleared once it appears.
  const recentMounts = useRef<Map<string, number>>(new Map());
  const refKey = (r: { name: string; machine: string }) => `${r.machine}/${r.name}`;

  // Adopt a freshly-fetched session list: render it, keep the chord handler's
  // ref fresh, and drop any pane holding a now-dead session. We deliberately
  // never auto-attach to an arbitrary session — landing on someone's live agent
  // unbidden would resize and disrupt it. Split out from `refresh` so the quiet
  // background poll can reuse it without touching the loading/error UI.
  const applySessionList = useCallback(
    (list: Session[]) => {
      setSessions(list);
      sessionsRef.current = list;
      const live = new Set(list.map((s) => s.name));
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
    getAuthStatus()
      .then((s) => setAuthed(!s.authRequired || s.authed))
      .catch(() => setAuthed(true));
    return () => setUnauthorizedHandler(null);
  }, []);

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
  // refresh button's spinner or clobbering an error banner. Browsers throttle
  // this to ~1×/min in a backgrounded tab, which is fine for an at-a-glance
  // signal; `focus` (below) also forces an immediate refresh on return.
  useEffect(() => {
    const id = setInterval(() => {
      fetchSessions().then(applySessionList).catch(() => {});
    }, 4000);
    return () => clearInterval(id);
  }, [applySessionList]);

  // Ambient "are my agents still running?" signal: the tab title and (installed
  // PWA) app-icon badge show how many sessions are actively producing output.
  // `waiting` is an idle agent's resting state, so we surface the inverse — the
  // count of *working* agents — which falls to zero once everything has
  // finished and is waiting for you. (See the server's IDLE_AFTER_SECS.)
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

  // Initial load.
  useEffect(() => {
    refresh();
  }, [refresh]);

  // Poll the hub's machine roster (empty [] on a standalone agent, which has no
  // /api/machines). Drives the per-machine grouping + pickers; polled slowly
  // since the roster changes rarely (an agent joining/leaving the fleet).
  useEffect(() => {
    const load = () => fetchMachines().then(setMachines).catch(() => {});
    load();
    const id = setInterval(load, 10000);
    return () => clearInterval(id);
  }, []);

  // Refresh the restore offer whenever the drawer opens — cheap, and the only
  // surface that shows it. Errors are non-fatal (just hides the offer).
  // With multiple machines, the restore offer is scoped to the focused machine
  // (else the first online) — a machine-less restore would be ambiguous at the
  // hub. Single-machine passes "" (unchanged, machine-less) behaviour.
  const restoreMachine = multiMachine ? currentSession?.machine || firstOnlineMachine : "";
  useEffect(() => {
    if (!drawerOpen) return;
    fetchRestorable(restoreMachine).then(setRestorable).catch(() => setRestorable([]));
  }, [drawerOpen, restoreMachine]);

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
  useEffect(() => {
    if (!isDesktop && currentSession === null) setDrawerOpen(true);
  }, [isDesktop, currentSession]);

  // Re-list when returning to the app (PWA resume / tab focus).
  useEffect(() => {
    const onFocus = () => refresh();
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [refresh]);

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
        if (lay > 1) {
          const delta = k === "ArrowRight" ? 1 : -1;
          setActive((cur + delta + lay) % lay);
        }
        return;
      }
      // Up / Down — session cycle in the active pane.
      const dir: 1 | -1 = k === "ArrowDown" ? 1 : -1;
      const names = sessionsRef.current.map((x) => x.name);
      const next = cycleSessionInPane(panesRef.current, cur, names, dir);
      if (next !== null) mountAt(cur, paneRefFor(next));
    };

    const isArrow = (k: string) =>
      k === "ArrowLeft" || k === "ArrowRight" || k === "ArrowUp" || k === "ArrowDown";

    const handler = (e: KeyboardEvent) => {
      if (shouldSkipShortcut(e)) return;
      // The editor overlay owns the whole screen and handles its own keys
      // (Esc/Cmd+S in its own capture-phase listener); the tmux-style prefix is
      // inert while it's open so Ctrl+B doesn't cycle panes underneath.
      if (editorOpenRef.current) return;
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
        armTimer = window.setTimeout(() => {
          armed = false;
          armTimer = null;
          openDrawer();
        }, PREFIX_TIMEOUT_MS);
        return;
      }

      const stop = () => {
        e.preventDefault();
        e.stopPropagation();
        e.stopImmediatePropagation();
      };

      if (armed) {
        const k = e.key;
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
          stop();
          clearArm();
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
        if (isArrow(k)) {
          stop();
          handleArrow(k);
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
  }, [isDesktop, closeAllSheets, mountAt, setActive, openPalette, openEditor]);

  // Suppress xterm.js's own paste-shortcut keydown handler.
  //
  // xterm.js converts the paste-shortcut keydown directly into a 0x16 byte
  // on the PTY's stdin — Claude Code sees that, runs its clipboard probe,
  // and finds nothing because our `/api/clip` POST hasn't completed staging
  // yet. Then our POST finally lands and the server fires *another* 0x16 via
  // tmux send-keys, but it arrives after Claude Code already gave up.
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
  // Ctrl+V still get the old behaviour (xterm forwards 0x16, Claude Code
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

      const term = termsRef.current[activeRef.current];
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
  //   image in clipboard -> POST /api/clip (stages + tmux send-keys C-v;
  //     Claude Code's shim then reads the staged PNG)
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
            showToast("📋 Image pasted", true);
          } catch (err) {
            console.error("clipboard image paste:", err);
            showToast("Paste failed", false);
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
  // in the active pane.
  const pick = (s: Session) => {
    mountAt(active, { name: s.name, machine: s.machine ?? "" });
    setDrawerOpen(false);
  };

  // Delete a session: show a spinner on its row, ask the server to end it
  // (soft = inject /exit, hard = kill), then poll until tmux no longer lists
  // it and drop it from the list (and from any pane holding it).
  const removeSession = useCallback(
    async (name: string, mode: "exit" | "kill", machine = "") => {
      setDeleting((d) => new Set(d).add(name));
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
    [refresh, updatePanes]
  );

  // Favourites live server-side (durable, shared across devices). Load once, then
  // keep an optimistic local copy and PUT the whole list on every change,
  // adopting the server's sanitised result.
  useEffect(() => {
    fetchFavorites().then(setFavorites).catch(() => {});
  }, []);
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
  const onImage = (png: Blob) => {
    if (!currentSession) return;
    sendImage(currentSession.name, png, currentSession.machine).catch(() => {});
  };
  const conn = conns[active] ?? "closed";
  // One unified status dot: connection trouble (red) wins, else the agent is
  // working (amber) or ready for input (green). See util/agentStatus.
  const headerStatus = agentStatus(cur?.waiting ?? true, conn);
  const dot = statusDot(headerStatus);
  // Per-session WS state for the switcher: only sessions open in a pane have a
  // connection that can be "wrong"; everything else falls through to waiting.
  const connByRef: Record<string, string> = {};
  panes.forEach((p, i) => {
    if (p) connByRef[`${p.machine ?? ""}/${p.name}`] = conns[i] ?? "closed";
  });

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

  // openNewFor opens the create-session panel and remembers which pane to
  // mount the new session into when it returns.
  const openNewFor = (idx: number) => {
    setNewForPane(idx);
    setDrawerOpen(false);
    setNewOpen(true);
  };

  // The session switcher, built once and rendered in one of two places:
  //  - phone  → full-screen takeover at the app root (embedded=false)
  //  - desktop → a left-pinned slide-in sidebar over the terminal area
  //    (sidebar=true), so Ctrl+B reveals the picker without blanking the
  //    terminal you were in (proposal 0006).
  const renderDrawer = (embedded: boolean, sidebar = false) => (
    <SessionDrawer
      open={drawerOpen}
      embedded={embedded}
      sidebar={sidebar}
      sessions={sessions}
      connByRef={connByRef}
      machines={machines}
      multiMachine={multiMachine}
      current={currentSession}
      loading={loading}
      error={error}
      onPick={pick}
      onClose={() => setDrawerOpen(false)}
      onRefresh={refresh}
      onNew={() => openNewFor(active)}
      deleting={deleting}
      onDelete={removeSession}
      restorable={restorable}
      onRestore={onRestore}
    />
  );

  // Auth gate (after all hooks, so the rules of hooks hold): a blank splash
  // while checking, the login screen when locked, otherwise the app.
  if (authed === null) {
    return <div className="fixed inset-0 bg-bar" />;
  }
  if (!authed) {
    return <LoginScreen onSuccess={() => setAuthed(true)} />;
  }

  return (
    <div
      className="relative flex flex-col bg-bar text-slate-200"
      style={{ height: appH ? `${appH}px` : "100%" }}
    >
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
              <span className="truncate font-medium text-slate-100">{cur.short}</span>
            </>
          ) : (
            <span className="text-slate-400">Pick a session</span>
          )}
        </button>

        <span className={`h-2.5 w-2.5 rounded-full ${dot}`} title={statusTitle(headerStatus)} />

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

      {/* Terminal(s) */}
      <main className="relative min-h-0 flex-1">
        {isDesktop ? (
          <TileGrid
            layout={layout}
            panes={panes}
            active={active}
            sessions={sessions}
            fontSize={fontSize}
            onActivate={setActive}
            onConn={setPaneConn}
            onPickFor={(idx, ref) => mountAt(idx, ref)}
            onOpenDrawerFor={(idx) => {
              setActive(idx);
              setDrawerOpen(true);
            }}
            onNewFor={openNewFor}
            onOpenEditor={() => openEditor(null)}
            onTermFor={(idx, t) => { termsRef.current[idx] = t; }}
            onDropFiles={onPaneDrop}
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
            onState={(c) => setPaneConn(active, c)}
            onTerm={(t) => { termsRef.current[active] = t; }}
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
                className="absolute inset-0 z-20 bg-black/20"
                onClick={() => setDrawerOpen(false)}
                aria-hidden
              />
            )}
            {renderDrawer(true, true)}
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

      {/* Phone: full-screen switcher. Desktop renders it pane-scoped inside
          TileGrid (see paneOverlay above), so it only covers the active box. */}
      {!isDesktop && renderDrawer(false)}
      <NewSessionPanel
        open={newOpen}
        machines={machines}
        multiMachine={multiMachine}
        initialMachine={currentSession?.machine || firstOnlineMachine}
        onClose={() => setNewOpen(false)}
        onCreated={(session) => {
          setNewOpen(false);
          setDrawerOpen(false);
          // Mount in the pane the user came from (-1 = active fallback). Mark a
          // propagation grace window so the immediate refresh() (and the
          // background poll) don't null this pane before the hub's session list
          // catches up — otherwise we'd bounce straight back to the switcher.
          recentMounts.current.set(refKey(session), Date.now() + 15000);
          const target = newForPane >= 0 ? newForPane : active;
          mountAt(target, session);
          setActive(target);
          setNewForPane(-1);
          refresh();
        }}
      />
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
        <Suspense fallback={null}>
          <EditorOverlay
            open={editor.open}
            initialPath={editor.path}
            session={currentSession?.name ?? null}
            machines={machines}
            multiMachine={multiMachine}
            initialMachine={currentSession?.machine || firstOnlineMachine}
            agentMachine={currentSession?.machine ?? ""}
            isDesktop={isDesktop}
            onClose={closeEditor}
            // The agent mirror renders at the active pane's true grid size so it
            // never has to report a size of its own (which would re-pin the
            // width-locked PTY). Falls back to 80×24 if the term isn't ready.
            agentCols={termsRef.current[active]?.cols}
            agentRows={termsRef.current[active]?.rows}
            termFontSize={fontSize}
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
