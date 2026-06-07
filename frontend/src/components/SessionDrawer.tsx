import { Fragment, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { type MachineInfo, type PaneRef, type RestorableSession, type Session } from "../api";
import { ago, agentStatus, fuzzyScore, statusDot, statusTitle, toolColor } from "../util";
import { PlusIcon, RefreshIcon, TrashIcon, XIcon } from "../icons";
import NotificationsButton from "./NotificationsButton";
import CreateSession from "./CreateSession";

interface Props {
  open: boolean;
  // Desktop renders this as a frosted panel (no safe-area padding); phone
  // renders it full-screen. The markup is the same — only the chrome/positioning
  // differs. See App.tsx / TileGrid.
  embedded?: boolean;
  // Desktop variant: a left-pinned, fixed-width column that slides in/out over
  // the terminal area (rather than covering a whole pane). When set, the node
  // stays mounted while closed and animates the transform, so the slide-out
  // plays; the phone path keeps the `!open` early return. See proposal 0006.
  sidebar?: boolean;
  sessions: Session[];
  // Per-session WebSocket state, keyed `${machine}/${name}`, for sessions open
  // in a pane — lets a row's status dot go red when its connection drops. Rows
  // not in the map simply have no connection to be wrong about.
  connByRef: Record<string, string>;
  // The hub's machine roster — used to render group headers (hostname + offline
  // indicator) and to surface idle machines that have no sessions yet. Empty on
  // a standalone agent.
  machines: MachineInfo[];
  // True when >1 machine is connected: group the list by machine. False keeps
  // the flat, single-machine layout unchanged.
  multiMachine: boolean;
  current: PaneRef | null;
  loading: boolean;
  error: string | null;
  onPick: (s: Session) => void;
  onClose: () => void;
  onRefresh: () => void;
  // Called when the user chooses "New session" — App records which pane to mount
  // into; the create flow itself now lives in-drawer (proposal 0016).
  onNew: () => void;
  // Create-flow plumbing (proposal 0016): the agent to create on, a recents
  // shortcut, and the mount callback once a session is created.
  createInitialMachine: string;
  recentDirs?: string[];
  onCreated: (ref: PaneRef) => void;
  // A token bumped by App to ask the (open) drawer to jump straight into create
  // mode — the per-pane "new session" affordances in TileGrid use this.
  createReq?: number;
  // "New layout" routes here (desktop only — there's no multi-pane grid on
  // phone). Reaches the existing LayoutPalette. `showLayout` gates the row.
  showLayout?: boolean;
  onLayout: () => void;
  deleting: Set<string>;
  onDelete: (name: string, mode: "exit" | "kill", machine?: string) => void;
  // Sessions a reboot/tmux restart took down that can be resumed; the button
  // appears only when non-empty. onRestore brings them all back.
  restorable: RestorableSession[];
  onRestore: () => void | Promise<void>;
}

// A navigable item the keyboard cursor can land on (proposal 0011, generalized
// by 0016 to include "New layout"). Empty filter → today's order; a non-empty
// filter fuzzy-ranks all of these in one flat list.
type NavItem =
  | { kind: "new" }
  | { kind: "layout" }
  | { kind: "restore" }
  | { kind: "session"; session: Session };

// Aliases that let the actions surface from a fuzzy query (e.g. "split" → New
// layout). The label is matched too.
const ACTION_TERMS: Record<string, string[]> = {
  new: ["new session", "create", "start"],
  layout: ["new layout", "split", "grid", "tile", "panes"],
  restore: ["restore", "resume", "saved sessions"],
};

// Session switcher — the one search-first place to switch, create, and re-layout
// (proposals 0006 / 0011 / 0016). Open it and just start typing: the list filters
// in place across sessions *and* the actions New session / New layout / Restore,
// ranked by a fuzzy score. ⌃B → type → ⏎ switches, starts, or re-layouts. On a
// phone it's a full-screen takeover; on desktop a left slide-in over the active
// terminal (see `sidebar`), so the width-locked PTY is never resized.
export default function SessionDrawer({
  open,
  embedded = false,
  sidebar = false,
  sessions,
  connByRef,
  machines,
  multiMachine,
  current,
  loading,
  error,
  onPick,
  onClose,
  onRefresh,
  onNew,
  createInitialMachine,
  recentDirs,
  onCreated,
  createReq = 0,
  showLayout = false,
  onLayout,
  deleting,
  onDelete,
  restorable,
  onRestore,
}: Props) {
  const [confirmDel, setConfirmDel] = useState<string | null>(null);
  const [restoring, setRestoring] = useState(false);
  // The sidebar has two body modes: the list (default) and the in-sidebar create
  // flow (proposal 0016). `createQuery` seeds the folder search from the list
  // filter when "New session ‹q›" is picked.
  const [mode, setMode] = useState<"list" | "create">("list");
  const [createQuery, setCreateQuery] = useState("");
  // The type-to-filter query (Part A). Empty = exactly today's resting list.
  const [query, setQuery] = useState("");
  const filterRef = useRef<HTMLInputElement>(null);

  // Triage order: the agent that *just became ready for input* floats to the top
  // of its machine group, so finishing work surfaces itself like a priority
  // queue. Within a group: ready (waiting) before running, then most-recently
  // active first. Machine stays the primary key so the multiMachine grouping
  // (contiguous-run header detection below) still holds.
  const ordered = useMemo(() => {
    return [...sessions].sort((a, b) => {
      const ma = a.machine ?? "";
      const mb = b.machine ?? "";
      if (ma !== mb) return ma < mb ? -1 : 1;
      if (a.waiting !== b.waiting) return a.waiting ? -1 : 1;
      return b.activity - a.activity;
    });
  }, [sessions]);

  // The base (unfiltered) item list — actions first, then session rows. The
  // cursor indexes the *visible* list (`view`), which equals this when the
  // filter is empty.
  const baseItems = useMemo<NavItem[]>(
    () => [
      { kind: "new" },
      ...(showLayout ? [{ kind: "layout" as const }] : []),
      ...(restorable.length > 0 ? [{ kind: "restore" as const }] : []),
      ...ordered.map((session) => ({ kind: "session" as const, session })),
    ],
    [ordered, restorable.length, showLayout]
  );

  const q = query.trim();
  const filtering = q.length > 0;

  // Fuzzy score for one item against the query (best over its label/aliases or
  // the session's searchable fields). null = no match (dropped while filtering).
  const scoreItem = useCallback(
    (it: NavItem): number | null => {
      if (it.kind === "session") {
        const s = it.session;
        const fields = [s.short, s.preview, s.tool, s.machine ?? ""];
        let best: number | null = null;
        for (const f of fields) {
          const sc = fuzzyScore(q, f);
          if (sc !== null) best = best === null ? sc : Math.max(best, sc);
        }
        return best;
      }
      let best: number | null = null;
      for (const term of ACTION_TERMS[it.kind]!) {
        const sc = fuzzyScore(q, term);
        if (sc !== null) best = best === null ? sc : Math.max(best, sc);
      }
      return best;
    },
    [q]
  );

  // The visible, possibly-ranked list. Empty query → baseItems verbatim (zero
  // behavioural change to the resting state). Non-empty → filtered + re-ranked.
  const view = useMemo<NavItem[]>(() => {
    if (!filtering) return baseItems;
    const scored = baseItems
      .map((it) => ({ it, score: scoreItem(it) }))
      .filter((x): x is { it: NavItem; score: number } => x.score !== null)
      .sort((a, b) => b.score - a.score);
    return scored.map((x) => x.it);
  }, [filtering, baseItems, scoreItem]);

  // Keyboard-cursor index into `view`. -1 means "nothing focused" (closed).
  const [cursor, setCursor] = useState<number>(-1);
  const itemRefs = useRef<(HTMLElement | null)[]>([]);

  // Reset transient UI whenever the drawer closes.
  useEffect(() => {
    if (!open) {
      setCursor(-1);
      setConfirmDel(null);
      setMode("list");
      setQuery("");
    }
  }, [open]);

  // Park the cursor: while filtering, on the top result (so ⏎ does the most
  // likely thing); otherwise on the currently-attached session, else "New
  // session" (proposal 0011 behaviour preserved for the resting list).
  useEffect(() => {
    if (!open || mode !== "list") return;
    if (filtering) {
      setCursor(view.length > 0 ? 0 : -1);
      return;
    }
    const sessionBase = baseItems.length - ordered.length;
    const cur = ordered.findIndex(
      (s) => s.name === current?.name && (s.machine ?? "") === current?.machine
    );
    setCursor(cur >= 0 ? sessionBase + cur : 0);
  }, [open, mode, filtering, view.length, baseItems.length, ordered, current]);

  // Keep the cursor item in view when it moves off-screen (long lists).
  useEffect(() => {
    if (cursor < 0) return;
    itemRefs.current[cursor]?.scrollIntoView({ block: "nearest" });
  }, [cursor]);

  // An external create request (TileGrid's per-pane "new session") jumps the
  // open drawer straight to create mode. Only react to a *change* in the token,
  // so a normal open (Ctrl+B) stays on the list.
  const lastCreateReq = useRef(createReq);
  useEffect(() => {
    if (createReq === lastCreateReq.current) return;
    lastCreateReq.current = createReq;
    if (!open) return;
    setCreateQuery("");
    setMode("create");
  }, [createReq, open]);

  // Autofocus the filter box on open / when returning to the list, so typing
  // filters immediately (deferred a frame so the input is mounted).
  useEffect(() => {
    if (!open || mode !== "list") return;
    const id = requestAnimationFrame(() => filterRef.current?.focus());
    return () => cancelAnimationFrame(id);
  }, [open, mode]);

  // Shared by the Restore button's click and the Enter-on-Restore keyboard path.
  const runRestore = useCallback(async () => {
    if (restoring) return;
    setRestoring(true);
    try {
      await onRestore();
    } finally {
      setRestoring(false);
    }
  }, [restoring, onRestore]);

  // Enter the in-sidebar create flow, carrying the current filter as the initial
  // folder query (Part A → Part B handoff).
  const enterCreate = useCallback(() => {
    onNew(); // App records which pane to mount the new session into
    setCreateQuery(filtering ? q : "");
    setMode("create");
  }, [onNew, filtering, q]);

  // Dispatch the focused item.
  const activate = useCallback(
    (it: NavItem | undefined) => {
      if (!it) return;
      if (it.kind === "session") onPick(it.session);
      else if (it.kind === "new") enterCreate();
      else if (it.kind === "layout") onLayout();
      else if (it.kind === "restore") void runRestore();
    },
    [onPick, enterCreate, onLayout, runRestore]
  );

  // ↑/↓ move the cursor, Enter dispatches, Esc clears the filter then closes.
  // Capture phase is load-bearing (see proposal 0011): xterm.js stopPropagation's
  // arrows/Enter/Esc on its helper textarea, which may still hold focus when the
  // drawer opens via Ctrl+B; capture phase fires before that handler so we win.
  // Printable keys still type into the (focused) filter input natively.
  useEffect(() => {
    if (!open || mode !== "list") return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        if (query) setQuery("");
        else onClose();
        return;
      }
      if (e.key === "ArrowDown") {
        e.preventDefault();
        e.stopPropagation();
        setCursor((i) => (i < 0 ? 0 : Math.min(view.length - 1, i + 1)));
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        e.stopPropagation();
        setCursor((i) => (i <= 0 ? 0 : i - 1));
        return;
      }
      if (e.key === "Enter") {
        e.preventDefault();
        e.stopPropagation();
        activate(view[cursor]);
      }
    };
    window.addEventListener("keydown", handler, { capture: true });
    return () => window.removeEventListener("keydown", handler, { capture: true });
  }, [open, mode, view, cursor, query, onClose, activate]);

  // Phone / pane-embedded variants unmount when closed. The sidebar variant
  // stays mounted so its slide-out transition can play; the keyboard/cursor
  // effects above are gated on `open`, so a mounted-but-closed sidebar is inert.
  if (!open && !sidebar) return null;

  const iconBtn =
    "flex h-7 w-7 shrink-0 items-center justify-center rounded-md text-slate-400 transition-colors hover:bg-edge/60 hover:text-slate-100";

  // The sidebar variant is a left-pinned, fixed-width column that translates
  // in/out over the terminal area (proposal 0006). It overlays — never pushes —
  // so the width-locked PTY is never resized. While closed it slides off-screen
  // and drops pointer events so clicks fall through to the terminal.
  const rootClass = sidebar
    ? [
        "absolute inset-y-0 left-0 z-30 flex w-[320px] max-w-[85%] flex-col text-slate-200",
        "border-r border-edge/80 bg-bar/95 backdrop-blur-md shadow-xl",
        "transition-transform duration-200 ease-out",
        open ? "translate-x-0" : "-translate-x-full pointer-events-none",
      ].join(" ")
    : `absolute inset-0 z-30 flex flex-col text-slate-200 ${
        embedded ? "bg-bar/95 backdrop-blur-md" : "bg-bar pt-safe"
      }`;

  // ── Create mode: the in-sidebar search-first create flow (proposal 0016). ──
  if (mode === "create") {
    return (
      <div className={rootClass} aria-hidden={sidebar && !open}>
        <CreateSession
          machines={machines}
          multiMachine={multiMachine}
          initialMachine={createInitialMachine}
          initialQuery={createQuery}
          recentDirs={recentDirs}
          onBack={() => {
            setMode("list");
            setQuery("");
          }}
          onClose={onClose}
          onCreated={onCreated}
        />
      </div>
    );
  }

  // A machine group header (shown only when grouping AND not filtering — a flat
  // ranked list reads better than sparse groups). Resolves hostname + online.
  const renderMachineHeader = (machineId: string, empty = false) => {
    const m = machines.find((x) => x.machine === machineId);
    const label = m?.hostname || machineId || "this machine";
    return (
      <div className="sticky top-0 z-10 flex items-center gap-1.5 bg-bar/95 px-2 pb-1 pt-2 backdrop-blur-sm">
        <span className="text-[10px] font-semibold uppercase tracking-wider text-slate-500">
          {label}
        </span>
        {m && !m.online && (
          <span className="rounded bg-edge/60 px-1 py-px text-[9px] text-slate-500" title="offline">
            offline
          </span>
        )}
        {empty && <span className="text-[10px] text-slate-600">· no sessions</span>}
      </div>
    );
  };

  // Empty (session-less) machines, listed at the end so an idle box stays
  // visible. Grouping only, and not while filtering.
  const emptyMachines =
    multiMachine && !filtering
      ? machines.filter((m) => !sessions.some((s) => (s.machine ?? "") === m.machine))
      : [];

  const actionRow = (it: NavItem, i: number) => {
    const focused = i === cursor;
    const ring = focused ? "bg-edge/70 ring-1 ring-inset ring-accent/40" : "hover:bg-edge/50";
    if (it.kind === "new") {
      return (
        <button
          key="new"
          ref={(el) => {
            itemRefs.current[i] = el;
          }}
          onClick={enterCreate}
          className={`flex w-full items-center gap-2 rounded-md py-2 pl-2 pr-2 text-left transition-colors ${ring}`}
        >
          <span className="flex h-5 w-5 shrink-0 items-center justify-center">
            <PlusIcon className="h-4 w-4 text-accent" />
          </span>
          <span className="text-[13px] font-medium text-slate-200">
            {filtering ? `New session “${q}”…` : "New session…"}
          </span>
        </button>
      );
    }
    if (it.kind === "layout") {
      return (
        <button
          key="layout"
          ref={(el) => {
            itemRefs.current[i] = el;
          }}
          onClick={onLayout}
          className={`flex w-full items-center gap-2 rounded-md py-2 pl-2 pr-2 text-left transition-colors ${ring}`}
        >
          <span className="flex h-5 w-5 shrink-0 items-center justify-center text-accent">⊞</span>
          <span className="text-[13px] font-medium text-slate-200">New layout</span>
        </button>
      );
    }
    // restore
    return (
      <button
        key="restore"
        ref={(el) => {
          itemRefs.current[i] = el;
        }}
        onClick={runRestore}
        disabled={restoring}
        className={`flex w-full items-center gap-2 rounded-md py-2 pl-2 pr-2 text-left transition-colors disabled:opacity-60 ${ring}`}
        title={restorable.map((r) => `${r.tool}-${r.short} · ${r.dir}`).join("\n")}
      >
        <span className="flex h-5 w-5 shrink-0 items-center justify-center">
          <RefreshIcon className={`h-4 w-4 text-emerald-400 ${restoring ? "animate-spin" : ""}`} />
        </span>
        <span className="min-w-0">
          <span className="block text-[13px] font-medium text-slate-200">
            {restoring
              ? "Restoring…"
              : `Restore ${restorable.length} saved session${restorable.length > 1 ? "s" : ""}`}
          </span>
          <span className="block truncate text-[11px] text-slate-600">
            resume after a reboot · {restorable.map((r) => r.short).join(", ")}
          </span>
        </span>
      </button>
    );
  };

  const sessionRow = (s: Session, i: number, showHeader: boolean) => {
    const active = s.name === current?.name && (s.machine ?? "") === current?.machine;
    const focused = i === cursor;
    const isDeleting = deleting.has(s.name);
    const status = agentStatus(
      s.waiting,
      connByRef[`${s.machine ?? ""}/${s.name}`] as "connecting" | "open" | "closed" | undefined
    );
    const rowState = focused
      ? "bg-edge/70 ring-1 ring-inset ring-accent/40"
      : active
        ? "bg-edge/30"
        : "hover:bg-edge/40";
    return (
      <Fragment key={`${s.machine ?? ""}/${s.name}`}>
        {showHeader && renderMachineHeader(s.machine ?? "")}
        <div
          ref={(el) => {
            itemRefs.current[i] = el;
          }}
          className={`group flex items-center rounded-md transition-colors ${rowState}`}
        >
          <button
            onClick={() => onPick(s)}
            className="flex min-w-0 flex-1 items-center gap-2 py-1.5 pl-2 pr-1 text-left"
          >
            <span className="flex h-5 w-5 shrink-0 items-center justify-center">
              <span className={`h-2 w-2 rounded-full ${toolColor(s.tool)}`} title={s.tool} />
            </span>
            <span className="min-w-0 flex-1">
              <span className="flex items-center gap-1.5">
                <span className="truncate text-[13px] font-medium text-slate-100">{s.short}</span>
                <span
                  className={`h-2 w-2 shrink-0 rounded-full ${statusDot(status)}`}
                  title={statusTitle(status)}
                />
                {s.skip_permissions === false && (
                  <span
                    className="shrink-0 rounded bg-emerald-500/20 px-1 py-px text-[9px] font-semibold uppercase tracking-wide text-emerald-300"
                    title="Launched with normal permission prompts (not YOLO)"
                  >
                    safe
                  </span>
                )}
                {/* When filtering across machines, show the machine inline since
                    the group headers are suppressed. */}
                {filtering && multiMachine && s.machine && (
                  <span className="shrink-0 rounded bg-edge/60 px-1 py-px text-[9px] text-slate-400">
                    {machines.find((m) => m.machine === s.machine)?.hostname || s.machine}
                  </span>
                )}
                <span className="ml-auto shrink-0 pl-2 text-[10px] tabular-nums text-slate-500">
                  {ago(s.activity)}
                </span>
              </span>
              <span className="mt-0.5 block truncate font-mono text-[11px] leading-tight text-slate-600">
                {s.preview || "—"}
              </span>
            </span>
          </button>

          <div className="flex shrink-0 items-center pr-1">
            {isDeleting ? (
              <span
                className="mx-1.5 inline-block h-4 w-4 animate-spin rounded-full border-2 border-edge border-t-accent"
                title="ending…"
              />
            ) : confirmDel === s.name ? (
              <div className="flex items-center gap-1">
                <button
                  onClick={() => {
                    onDelete(s.name, "exit", s.machine);
                    setConfirmDel(null);
                  }}
                  className="rounded-md bg-edge px-2 py-1 text-[11px] text-slate-200 hover:bg-edge/70"
                  title="inject /exit, then wait for it to quit"
                >
                  /exit
                </button>
                <button
                  onClick={() => {
                    onDelete(s.name, "kill", s.machine);
                    setConfirmDel(null);
                  }}
                  className="rounded-md bg-red-500/80 px-2 py-1 text-[11px] font-semibold text-bar hover:bg-red-500"
                  title="force kill the process"
                >
                  kill
                </button>
                <button
                  onClick={() => setConfirmDel(null)}
                  aria-label="Cancel delete"
                  className="flex h-6 w-6 items-center justify-center rounded text-slate-500 hover:text-slate-300"
                >
                  <XIcon className="h-3.5 w-3.5" />
                </button>
              </div>
            ) : (
              <button
                onClick={() => setConfirmDel(s.name)}
                aria-label={`Delete session ${s.short}`}
                className="flex h-8 w-8 items-center justify-center rounded-md text-slate-600 opacity-80 transition-colors hover:bg-edge hover:text-red-400 hover:opacity-100"
              >
                <TrashIcon className="h-4 w-4" />
              </button>
            )}
          </div>
        </div>
      </Fragment>
    );
  };

  // Track the previous session's machine while rendering so a contiguous run
  // gets one header (unfiltered grouping only).
  let lastMachine: string | null = null;

  return (
    <div className={rootClass} aria-hidden={sidebar && !open}>
      {/* Header: title + count, then the keyboard hint and icon chrome. */}
      <div className="flex items-center gap-2 border-b border-edge/80 px-3 py-2.5">
        <span className="text-[13px] font-semibold tracking-wide text-slate-100">Sessions</span>
        {sessions.length > 0 && (
          <span className="rounded bg-edge/60 px-1.5 py-0.5 text-[10px] tabular-nums text-slate-400">
            {sessions.length}
          </span>
        )}
        <span className="ml-auto hidden text-[10px] text-slate-600 sm:inline">↑↓ ⏎ · Esc · ⌃B</span>
        <NotificationsButton className={`${iconBtn} ml-auto sm:ml-0`} />
        <button onClick={onRefresh} aria-label="Refresh sessions" className={iconBtn}>
          <RefreshIcon className={`h-4 w-4 ${loading ? "animate-spin" : ""}`} />
        </button>
        <button onClick={onClose} aria-label="Close" className={iconBtn}>
          <XIcon className="h-4 w-4" />
        </button>
      </div>

      {/* Filter box (Part A) — autofocused; type to filter sessions + actions. */}
      <div className="flex items-center gap-2 border-b border-edge/60 px-3 py-1.5">
        <span className="text-slate-500">🔎</span>
        <input
          ref={filterRef}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="Search sessions, actions…"
          className="min-w-0 flex-1 bg-transparent text-[13px] text-slate-100 placeholder:text-slate-600 outline-none"
        />
        {query && (
          <button
            onClick={() => {
              setQuery("");
              filterRef.current?.focus();
            }}
            aria-label="Clear filter"
            className="shrink-0 text-slate-500 hover:text-slate-300"
          >
            <XIcon className="h-3.5 w-3.5" />
          </button>
        )}
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto px-1.5 py-1.5">
        {error && <div className="px-2 py-2 text-[12px] text-red-400">{error}</div>}

        {view.length === 0 && filtering && (
          <div className="px-3 py-8 text-center text-[12px] text-slate-600">
            No matches for “{q}”.
          </div>
        )}

        {view.map((it, i) => {
          if (it.kind !== "session") {
            // A divider between the action block and the session rows in the
            // resting (unfiltered) list, mirroring today's layout.
            const nextIsSession = view[i + 1]?.kind === "session";
            return (
              <Fragment key={`action-${it.kind}`}>
                {actionRow(it, i)}
                {!filtering && nextIsSession && (
                  <div className="mx-1 my-1.5 border-t border-edge/50" />
                )}
              </Fragment>
            );
          }
          const machine = it.session.machine ?? "";
          const showHeader = !filtering && multiMachine && machine !== lastMachine;
          lastMachine = machine;
          return sessionRow(it.session, i, showHeader);
        })}

        {!filtering && !error && sessions.length === 0 && (
          <div className="px-3 py-10 text-center text-[12px] leading-relaxed text-slate-600">
            No sessions yet.
            <br />
            Start one with <code className="text-slate-500">cc</code> on the box, or “New session”.
          </div>
        )}

        {/* Idle/offline machines with no sessions — visible so you know they're
            there (start one via New session). Grouping only, unfiltered. */}
        {emptyMachines.map((m) => (
          <Fragment key={`empty/${m.machine}`}>{renderMachineHeader(m.machine, true)}</Fragment>
        ))}
      </div>
    </div>
  );
}
