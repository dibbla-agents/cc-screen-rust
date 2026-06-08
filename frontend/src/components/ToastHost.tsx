// Proposal 0017 — in-app session toasts.
//
// A transient, clickable toast that surfaces when a session the user does NOT
// have open finishes a turn worth being told about — the *foreground* case OS
// Web Push (0002) deliberately skips. The gated busy→waiting edge is computed in
// readyEdges.ts; this component only renders + owns dismissal.
//
// Desktop: a bottom-right stack (clear of the header/grid), capped at 3 with a
// "+N more ready" overflow that opens the drawer.
// Mobile: a single non-focus-stealing top banner below the header. It must never
// call focus() and is pointer-events-scoped so it can't overlap or steal the
// terminal input / ControlBar — proposal 0009 fixed exactly that regression and
// this must not bring it back.

import {
  forwardRef,
  useCallback,
  useEffect,
  useImperativeHandle,
  useRef,
  useState,
} from "react";
import type { ReadyEdge } from "../readyEdges";
import { sessionKey } from "../readyEdges";
import { toolColor } from "../util";
import { XIcon } from "../icons";

// Auto-dismiss window. Long enough to read + click, short enough to stay
// transient. Paused while hovered/focused (desktop) so it can't vanish under
// the pointer.
const TOAST_TTL_MS = 7000;
// Desktop visible cap; the rest collapse into a "+N more ready" affordance.
const MAX_VISIBLE = 3;

export interface ToastHostHandle {
  // Ingest freshly-detected ready edges. Re-firing an edge for a session that
  // already has a toast replaces it in place (coalesce, mirroring the SW's
  // per-session `tag`) rather than stacking a duplicate.
  push(edges: ReadyEdge[]): void;
}

interface ToastEntry extends ReadyEdge {
  id: string; // sessionKey — stable per (machine, name)
  seq: number; // monotonic, for stable newest-first ordering
}

interface Props {
  isDesktop: boolean;
  // Mount the toasted session (the existing openSessionByName path).
  onOpen: (name: string, machine: string) => void;
  // Desktop overflow: open the drawer to see every ready session.
  onOverflow: () => void;
  // Sessions currently on screen in a pane (sessionKey()s). A toast for a
  // now-mounted session is retracted — covers both "user mounted it before the
  // toast expired" and the click-to-open path (which mounts then auto-clears).
  mountedKeys: Set<string>;
}

const ToastHost = forwardRef<ToastHostHandle, Props>(function ToastHost(
  { isDesktop, onOpen, onOverflow, mountedKeys },
  ref
) {
  const [entries, setEntries] = useState<ToastEntry[]>([]);
  // Per-entry dismissal timers, keyed by id. Cleared on hover, restarted on
  // leave, and torn down on unmount.
  const timers = useRef<Map<string, number>>(new Map());
  const seqRef = useRef(0);
  // Suspend auto-dismiss while the pointer/focus is on the stack (desktop only).
  const hovering = useRef(false);

  const clearTimer = useCallback((id: string) => {
    const t = timers.current.get(id);
    if (t != null) {
      window.clearTimeout(t);
      timers.current.delete(id);
    }
  }, []);

  const dismiss = useCallback(
    (id: string) => {
      clearTimer(id);
      setEntries((es) => es.filter((e) => e.id !== id));
    },
    [clearTimer]
  );

  const armTimer = useCallback(
    (id: string) => {
      if (hovering.current) return; // paused — re-armed on mouse leave
      clearTimer(id);
      const handle = window.setTimeout(() => dismiss(id), TOAST_TTL_MS);
      timers.current.set(id, handle);
    },
    [clearTimer, dismiss]
  );

  useImperativeHandle(
    ref,
    () => ({
      push(edges: ReadyEdge[]) {
        if (edges.length === 0) return;
        setEntries((es) => {
          const byId = new Map(es.map((e) => [e.id, e]));
          for (const edge of edges) {
            const id = sessionKey(edge);
            byId.set(id, { ...edge, id, seq: ++seqRef.current }); // upsert / coalesce
          }
          return Array.from(byId.values()).sort((a, b) => b.seq - a.seq);
        });
        // (Re)arm a fresh dismissal window for each pushed session.
        for (const edge of edges) armTimer(sessionKey(edge));
      },
    }),
    [armTimer]
  );

  // Retract any toast whose session is now mounted in a pane (it carries its own
  // status on screen). Also the click-to-open path lands here once panes update.
  useEffect(() => {
    if (mountedKeys.size === 0) return;
    setEntries((es) => {
      const keep = es.filter((e) => !mountedKeys.has(e.id));
      if (keep.length === es.length) return es;
      for (const e of es) if (mountedKeys.has(e.id)) clearTimer(e.id);
      return keep;
    });
  }, [mountedKeys, clearTimer]);

  // Tear down every pending timer on unmount.
  useEffect(() => {
    const map = timers.current;
    return () => {
      for (const t of map.values()) window.clearTimeout(t);
      map.clear();
    };
  }, []);

  const onClick = useCallback(
    (e: ToastEntry) => {
      onOpen(e.name, e.machine);
      dismiss(e.id);
    },
    [onOpen, dismiss]
  );

  if (entries.length === 0) return null;

  if (!isDesktop) {
    // Mobile: a single top banner (the newest). pointer-events scoped to the
    // banner so the rest of the screen — terminal, ControlBar — stays live.
    const top = entries[0]!;
    return (
      <div
        role="status"
        aria-live="polite"
        className="pointer-events-none absolute inset-x-0 top-0 z-40 flex justify-center px-3 pt-safe"
      >
        <MobileBanner entry={top} onClick={onClick} onDismiss={dismiss} />
      </div>
    );
  }

  // Desktop: bottom-right stack, newest on top, capped with an overflow row.
  const visible = entries.slice(0, MAX_VISIBLE);
  const extra = entries.length - visible.length;
  return (
    <div
      role="status"
      aria-live="polite"
      className="pointer-events-none absolute bottom-4 right-4 z-40 flex w-80 max-w-[calc(100vw-2rem)] flex-col items-stretch gap-2"
      onMouseEnter={() => {
        hovering.current = true;
        for (const id of timers.current.keys()) clearTimer(id);
      }}
      onMouseLeave={() => {
        hovering.current = false;
        for (const e of entries) armTimer(e.id);
      }}
    >
      {extra > 0 && (
        <button
          type="button"
          onClick={onOverflow}
          className="pointer-events-auto self-end rounded-full bg-panel/95 px-3 py-1 text-xs font-medium text-slate-300 shadow-lg ring-1 ring-edge backdrop-blur-sm active:bg-edge"
        >
          +{extra} more ready
        </button>
      )}
      {visible.map((e) => (
        <DesktopToast key={e.id} entry={e} onClick={onClick} onDismiss={dismiss} />
      ))}
    </div>
  );
});

export default ToastHost;

// One desktop toast: a tool-color dot + short name + "ready for input", the
// whole row a button that mounts the session; a separate close affordance.
// motion-safe slide-in; reduced motion gets a plain fade (motion-reduce variant
// drops the translate).
// The toast's status line: the LLM summary detail (or headline) when present —
// the whole point of proposal 0022 is that the buzz says what's needed — else
// the generic "ready for input". A longer detail is clamped to two lines.
function ToastStatus({ entry }: { entry: ToastEntry }) {
  const summary = entry.detail || entry.headline;
  if (summary) {
    return <span className="line-clamp-2 text-xs text-slate-300">{summary}</span>;
  }
  return <span className="text-xs text-emerald-400">ready for input</span>;
}

function DesktopToast({
  entry,
  onClick,
  onDismiss,
}: {
  entry: ToastEntry;
  onClick: (e: ToastEntry) => void;
  onDismiss: (id: string) => void;
}) {
  return (
    <div className="pointer-events-auto flex items-stretch overflow-hidden rounded-xl bg-panel/95 shadow-lg ring-1 ring-edge backdrop-blur-sm motion-safe:animate-toastIn">
      <button
        type="button"
        onClick={() => onClick(entry)}
        className="flex min-w-0 flex-1 items-center gap-2.5 px-3.5 py-3 text-left active:bg-edge"
      >
        <span className={`h-2.5 w-2.5 flex-none rounded-full ${toolColor(entry.tool)}`} />
        <span className="flex min-w-0 flex-col">
          <span className="truncate text-sm font-medium text-slate-100">{entry.short}</span>
          <ToastStatus entry={entry} />
        </span>
      </button>
      <button
        type="button"
        onClick={() => onDismiss(entry.id)}
        aria-label={`Dismiss ${entry.short} notification`}
        className="flex flex-none items-center px-2 text-slate-500 hover:text-slate-300 active:bg-edge"
      >
        <XIcon className="h-4 w-4" />
      </button>
    </div>
  );
}

// Mobile banner: tap to switch, swipe horizontally (or tap ✕) to dismiss. It
// never calls focus(); pointer-events are scoped by the wrapper above.
function MobileBanner({
  entry,
  onClick,
  onDismiss,
}: {
  entry: ToastEntry;
  onClick: (e: ToastEntry) => void;
  onDismiss: (id: string) => void;
}) {
  const startX = useRef<number | null>(null);
  const [dx, setDx] = useState(0);

  const onTouchStart = (e: React.TouchEvent) => {
    startX.current = e.touches[0]?.clientX ?? null;
  };
  const onTouchMove = (e: React.TouchEvent) => {
    if (startX.current == null) return;
    setDx((e.touches[0]?.clientX ?? startX.current) - startX.current);
  };
  const onTouchEnd = () => {
    if (Math.abs(dx) > 80) onDismiss(entry.id);
    startX.current = null;
    setDx(0);
  };

  return (
    <div
      className="pointer-events-auto mt-2 flex w-full max-w-md items-stretch overflow-hidden rounded-xl bg-panel/95 shadow-lg ring-1 ring-edge backdrop-blur-sm motion-safe:animate-toastIn"
      style={{ transform: dx ? `translateX(${dx}px)` : undefined, opacity: dx ? Math.max(0.3, 1 - Math.abs(dx) / 200) : 1 }}
      onTouchStart={onTouchStart}
      onTouchMove={onTouchMove}
      onTouchEnd={onTouchEnd}
    >
      <button
        type="button"
        onClick={() => onClick(entry)}
        className="flex min-w-0 flex-1 items-center gap-2.5 px-4 py-3 text-left active:bg-edge"
      >
        <span className={`h-2.5 w-2.5 flex-none rounded-full ${toolColor(entry.tool)}`} />
        <span className="flex min-w-0 flex-col">
          <span className="truncate text-sm font-medium text-slate-100">{entry.short}</span>
          <ToastStatus entry={entry} />
        </span>
      </button>
      <button
        type="button"
        onClick={() => onDismiss(entry.id)}
        aria-label={`Dismiss ${entry.short} notification`}
        className="flex flex-none items-center px-3 text-slate-500 active:bg-edge"
      >
        <XIcon className="h-4 w-4" />
      </button>
    </div>
  );
}
