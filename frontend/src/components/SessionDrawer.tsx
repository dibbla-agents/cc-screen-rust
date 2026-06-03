import { useEffect, useRef, useState } from "react";
import type { RestorableSession, Session } from "../api";
import { ago, toolColor } from "../util";
import { PlusIcon, RefreshIcon, TrashIcon, XIcon } from "../icons";
import NotificationsButton from "./NotificationsButton";

interface Props {
  open: boolean;
  // Desktop renders this scoped to the active terminal pane (frosted panel,
  // no safe-area padding); phone renders it full-screen. The markup is the
  // same — only the chrome/positioning differs. See App.tsx / TileGrid.
  embedded?: boolean;
  sessions: Session[];
  current: string | null;
  loading: boolean;
  error: string | null;
  onPick: (name: string) => void;
  onClose: () => void;
  onRefresh: () => void;
  onNew: () => void;
  deleting: Set<string>;
  onDelete: (name: string, mode: "exit" | "kill") => void;
  // Sessions a reboot/tmux restart took down that can be resumed; the button
  // appears only when non-empty. onRestore brings them all back.
  restorable: RestorableSession[];
  onRestore: () => void | Promise<void>;
}

// Session switcher — the one place to see every agent (with a live preview
// line) and one tap to jump to it. Visual language matches the file
// editor/viewer: clean sans, tight rounded rows, a small tool-colour dot
// instead of a loud pill, and icon-button chrome. On a phone it's a
// full-screen takeover; on desktop it's scoped to the active terminal box
// (see `embedded`).
export default function SessionDrawer({
  open,
  embedded = false,
  sessions,
  current,
  loading,
  error,
  onPick,
  onClose,
  onRefresh,
  onNew,
  deleting,
  onDelete,
  restorable,
  onRestore,
}: Props) {
  const [confirmDel, setConfirmDel] = useState<string | null>(null);
  const [restoring, setRestoring] = useState(false);
  // Keyboard-cursor index into `sessions`. Separate from the "active"
  // (currently-attached) row so ↑/↓ can park on a different session before
  // committing with Enter. -1 means "no row focused" (e.g. empty list).
  const [cursor, setCursor] = useState<number>(-1);
  const rowRefs = useRef<(HTMLDivElement | null)[]>([]);

  // Whenever the drawer opens (or the session list changes while it's open),
  // park the cursor on the currently-attached session — that's the natural
  // starting point for "show me what else is around".
  useEffect(() => {
    if (!open) {
      setCursor(-1);
      setConfirmDel(null);
      return;
    }
    const cur = sessions.findIndex((s) => s.name === current);
    setCursor(cur >= 0 ? cur : sessions.length > 0 ? 0 : -1);
  }, [open, sessions, current]);

  // Keep the cursor row in view when it moves off-screen (long lists).
  useEffect(() => {
    if (cursor < 0) return;
    rowRefs.current[cursor]?.scrollIntoView({ block: "nearest" });
  }, [cursor]);

  // ↑/↓ to move the cursor, Enter to pick, Esc to dismiss. Bound only while
  // the drawer is open so the terminal still owns these keys when it's not.
  //
  // Capture phase is load-bearing here. When the user opens the drawer via
  // Ctrl+B the xterm.js helper textarea typically still has focus from the
  // session they were just looking at — and xterm.js calls stopPropagation
  // on arrows/Enter/Esc on that textarea (it forwards them to tmux as the
  // ANSI escape sequences agents use to drive their menus). A bubble-phase
  // listener on window would never run; capture phase fires *before* the
  // target handler, so we win the race. Symptom of the bug-that-was: arrows
  // started working only after clicking in the drawer area (which blurs the
  // xterm textarea, breaking the stopPropagation chain).
  useEffect(() => {
    if (!open) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onClose();
        return;
      }
      if (sessions.length === 0) return;
      if (e.key === "ArrowDown") {
        e.preventDefault();
        e.stopPropagation();
        setCursor((i) => (i < 0 ? 0 : Math.min(sessions.length - 1, i + 1)));
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        e.stopPropagation();
        setCursor((i) => (i <= 0 ? 0 : i - 1));
        return;
      }
      if (e.key === "Enter") {
        if (cursor >= 0 && cursor < sessions.length) {
          e.preventDefault();
          e.stopPropagation();
          onPick(sessions[cursor].name);
        }
      }
    };
    window.addEventListener("keydown", handler, { capture: true });
    return () =>
      window.removeEventListener("keydown", handler, { capture: true });
  }, [open, sessions, cursor, onClose, onPick]);

  if (!open) return null;

  // Shared icon-button chrome (refresh / close) — same understated look as the
  // editor's toolbar buttons.
  const iconBtn =
    "flex h-7 w-7 shrink-0 items-center justify-center rounded-md text-slate-400 transition-colors hover:bg-edge/60 hover:text-slate-100";

  return (
    <div
      className={`absolute inset-0 z-30 flex flex-col text-slate-200 ${
        embedded ? "bg-bar/95 backdrop-blur-md" : "bg-bar pt-safe"
      }`}
    >
      {/* Header: title + count, then the keyboard hint and icon chrome. */}
      <div className="flex items-center gap-2 border-b border-edge/80 px-3 py-2.5">
        <span className="text-[13px] font-semibold tracking-wide text-slate-100">
          Sessions
        </span>
        {sessions.length > 0 && (
          <span className="rounded bg-edge/60 px-1.5 py-0.5 text-[10px] tabular-nums text-slate-400">
            {sessions.length}
          </span>
        )}
        <span className="ml-auto hidden text-[10px] text-slate-600 sm:inline">
          ↑↓ ⏎ · Esc · ⌃B
        </span>
        <NotificationsButton className={`${iconBtn} ml-auto sm:ml-0`} />
        <button onClick={onRefresh} aria-label="Refresh sessions" className={iconBtn}>
          <RefreshIcon className={`h-4 w-4 ${loading ? "animate-spin" : ""}`} />
        </button>
        <button onClick={onClose} aria-label="Close" className={iconBtn}>
          <XIcon className="h-4 w-4" />
        </button>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto px-1.5 py-1.5">
        {/* New session */}
        <button
          onClick={onNew}
          className="flex w-full items-center gap-2 rounded-md py-2 pl-2 pr-2 text-left transition-colors hover:bg-edge/50"
        >
          <span className="flex h-5 w-5 shrink-0 items-center justify-center">
            <PlusIcon className="h-4 w-4 text-accent" />
          </span>
          <span className="text-[13px] font-medium text-slate-200">New session…</span>
        </button>

        {/* Restore (only after a reboot/tmux restart took sessions down) */}
        {restorable.length > 0 && (
          <button
            onClick={async () => {
              if (restoring) return;
              setRestoring(true);
              try {
                await onRestore();
              } finally {
                setRestoring(false);
              }
            }}
            disabled={restoring}
            className="flex w-full items-center gap-2 rounded-md py-2 pl-2 pr-2 text-left transition-colors hover:bg-edge/50 disabled:opacity-60"
            title={restorable.map((r) => `${r.tool}-${r.short} · ${r.dir}`).join("\n")}
          >
            <span className="flex h-5 w-5 shrink-0 items-center justify-center">
              <RefreshIcon
                className={`h-4 w-4 text-emerald-400 ${restoring ? "animate-spin" : ""}`}
              />
            </span>
            <span className="min-w-0">
              <span className="block text-[13px] font-medium text-slate-200">
                {restoring
                  ? "Restoring…"
                  : `Restore ${restorable.length} saved session${
                      restorable.length > 1 ? "s" : ""
                    }`}
              </span>
              <span className="block truncate text-[11px] text-slate-600">
                resume after a reboot · {restorable.map((r) => r.short).join(", ")}
              </span>
            </span>
          </button>
        )}

        {sessions.length > 0 && (
          <div className="mx-1 my-1.5 border-t border-edge/50" />
        )}

        {error && <div className="px-2 py-2 text-[12px] text-red-400">{error}</div>}
        {!error && sessions.length === 0 && (
          <div className="px-3 py-10 text-center text-[12px] leading-relaxed text-slate-600">
            No sessions yet.
            <br />
            Start one with <code className="text-slate-500">cc</code> on the box.
          </div>
        )}

        {sessions.map((s, idx) => {
          const active = s.name === current;
          const focused = idx === cursor;
          const isDeleting = deleting.has(s.name);
          const rowState = focused
            ? "bg-edge/70 ring-1 ring-inset ring-accent/40"
            : active
            ? "bg-edge/30"
            : "hover:bg-edge/40";
          return (
            <div
              key={s.name}
              ref={(el) => {
                rowRefs.current[idx] = el;
              }}
              className={`group flex items-center rounded-md transition-colors ${rowState}`}
            >
              <button
                onClick={() => onPick(s.name)}
                className="flex min-w-0 flex-1 items-center gap-2 py-1.5 pl-2 pr-1 text-left"
              >
                <span className="flex h-5 w-5 shrink-0 items-center justify-center">
                  <span
                    className={`h-2 w-2 rounded-full ${toolColor(s.tool)}`}
                    title={s.tool}
                  />
                </span>
                <span className="min-w-0 flex-1">
                  <span className="flex items-center gap-1.5">
                    <span className="truncate text-[13px] font-medium text-slate-100">
                      {s.short}
                    </span>
                    {/* `waiting` is an idle agent's resting state, so we mark the
                        inverse: an amber pulse on agents still producing output.
                        A glance shows which are working vs done. (See the
                        server's IDLE_AFTER_SECS.) */}
                    {!s.waiting && (
                      <span
                        className="h-1.5 w-1.5 shrink-0 animate-pulse rounded-full bg-amber-400"
                        title="working"
                      />
                    )}
                    {s.attached && (
                      <span
                        className="h-1.5 w-1.5 shrink-0 rounded-full bg-emerald-400"
                        title="attached"
                      />
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
                        onDelete(s.name, "exit");
                        setConfirmDel(null);
                      }}
                      className="rounded-md bg-edge px-2 py-1 text-[11px] text-slate-200 hover:bg-edge/70"
                      title="inject /exit, then wait for it to quit"
                    >
                      /exit
                    </button>
                    <button
                      onClick={() => {
                        onDelete(s.name, "kill");
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
          );
        })}
      </div>
    </div>
  );
}
