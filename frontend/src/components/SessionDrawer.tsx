import { useEffect, useRef, useState } from "react";
import type { RestorableSession, Session } from "../api";
import { ago, toolColor } from "../util";

interface Props {
  open: boolean;
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

// Full-height switcher overlay. The whole point of the phone UX: one tap to see
// every agent (with a live preview line) and one tap to jump to it.
export default function SessionDrawer({
  open,
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
  return (
    <div className="absolute inset-0 z-30 flex flex-col bg-bar pt-safe">
      <div className="flex items-center gap-3 border-b border-edge px-4 py-3">
        <span className="flex-1 text-lg font-semibold text-slate-100">Sessions</span>
        <span className="hidden text-[11px] text-slate-500 sm:inline">
          ↑↓ ⏎ · Esc · Ctrl+B
        </span>
        <button
          onClick={onRefresh}
          className="rounded-md bg-panel px-3 py-2 text-sm text-slate-300 active:bg-edge"
        >
          {loading ? "…" : "↻"}
        </button>
        <button
          onClick={onClose}
          className="rounded-md bg-panel px-3 py-2 text-sm text-slate-300 active:bg-edge"
        >
          ✕
        </button>
      </div>

      <button
        onClick={onNew}
        className="flex items-center gap-2 border-b border-edge px-4 py-3 text-left active:bg-panel"
      >
        <span className="text-lg leading-none text-accent">＋</span>
        <span className="font-medium text-slate-100">New session…</span>
      </button>

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
          className="flex items-center gap-2 border-b border-edge px-4 py-3 text-left active:bg-panel disabled:opacity-60"
          title={restorable.map((r) => `${r.tool}-${r.short} · ${r.dir}`).join("\n")}
        >
          <span className="text-base leading-none text-emerald-400">{restoring ? "…" : "⟲"}</span>
          <span className="min-w-0">
            <span className="block font-medium text-slate-100">
              {restoring
                ? "Restoring…"
                : `Restore ${restorable.length} saved session${restorable.length > 1 ? "s" : ""}`}
            </span>
            <span className="block truncate text-xs text-slate-500">
              resume after a reboot · {restorable.map((r) => r.short).join(", ")}
            </span>
          </span>
        </button>
      )}

      <div className="flex-1 overflow-y-auto">
        {error && <div className="px-4 py-3 text-sm text-red-400">{error}</div>}
        {!error && sessions.length === 0 && (
          <div className="px-4 py-8 text-center text-sm text-slate-500">
            No cc-screen sessions. Start one with <code>cc</code> on the box.
          </div>
        )}
        {sessions.map((s, idx) => {
          const active = s.name === current;
          const focused = idx === cursor;
          const isDeleting = deleting.has(s.name);
          return (
            <div
              key={s.name}
              ref={(el) => {
                rowRefs.current[idx] = el;
              }}
              className={`flex items-stretch border-b border-edge/60 border-l-2 ${
                focused ? "border-l-accent bg-edge/40" : "border-l-transparent"
              } ${active ? "bg-panel" : ""}`}
            >
              <button
                onClick={() => onPick(s.name)}
                className="flex min-w-0 flex-1 items-start gap-3 px-4 py-3 text-left active:bg-panel"
              >
                <span
                  className={`mt-0.5 rounded px-1.5 py-0.5 text-[10px] font-bold uppercase tracking-wide text-bar ${toolColor(
                    s.tool
                  )}`}
                >
                  {s.tool}
                </span>
                <span className="min-w-0 flex-1">
                  <span className="flex items-center gap-2">
                    <span className="truncate font-medium text-slate-100">{s.short}</span>
                    {s.attached && (
                      <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-emerald-400" title="attached" />
                    )}
                    <span className="ml-auto shrink-0 text-xs text-slate-500">{ago(s.activity)}</span>
                  </span>
                  <span className="mt-0.5 block truncate font-mono text-xs text-slate-500">
                    {s.preview || "—"}
                  </span>
                </span>
              </button>

              <div className="flex shrink-0 items-center pr-2">
                {isDeleting ? (
                  <span
                    className="mx-2 inline-block h-4 w-4 animate-spin rounded-full border-2 border-edge border-t-accent"
                    title="ending…"
                  />
                ) : confirmDel === s.name ? (
                  <div className="flex items-center gap-1">
                    <button
                      onClick={() => {
                        onDelete(s.name, "exit");
                        setConfirmDel(null);
                      }}
                      className="rounded-md bg-edge px-2 py-1.5 text-xs text-slate-100 active:opacity-80"
                      title="inject /exit, then wait for it to quit"
                    >
                      /exit
                    </button>
                    <button
                      onClick={() => {
                        onDelete(s.name, "kill");
                        setConfirmDel(null);
                      }}
                      className="rounded-md bg-red-500/80 px-2 py-1.5 text-xs font-semibold text-bar active:opacity-80"
                      title="force kill the process"
                    >
                      kill
                    </button>
                    <button
                      onClick={() => setConfirmDel(null)}
                      className="px-1.5 py-1.5 text-xs text-slate-400"
                    >
                      ✕
                    </button>
                  </div>
                ) : (
                  <button
                    onClick={() => setConfirmDel(s.name)}
                    aria-label={`Delete session ${s.short}`}
                    className="px-2 text-lg text-slate-600 active:text-red-400"
                  >
                    🗑
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
