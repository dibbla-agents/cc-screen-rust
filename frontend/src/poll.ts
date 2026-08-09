import { useEffect, useRef } from "react";

// The app's recurring-poll primitive (proposal 0068 Part C).
//
// Every interval in the app used to run at full cadence whether or not anyone
// could see the result, so a backgrounded tab kept fetching, re-rendering and
// repainting. This hook keeps the *visible* cadences exactly as they were
// (0017/0023's 4s sessions poll, 0041/0065's 8s dashboard polls, …) and only
// changes what happens while `document.visibilityState === "hidden"`:
//
//   hiddenMs: null    → pause entirely, and refetch once on return (the
//                       default; right for anything that only feeds a badge or
//                       a card you cannot see).
//   hiddenMs: 60_000  → keep a slow heartbeat, for the one poll whose result is
//                       read *while* hidden (the tab title + PWA app badge).
//
// `focus` and `visibilitychange` are deduped against each other and against the
// interval, so returning to a tab triggers exactly one fetch.
export interface PollOpts {
  // Skip the poll entirely (auth not settled, feature off, …).
  enabled?: boolean;
  // Cadence while the tab is hidden; null pauses.
  hiddenMs?: number | null;
  // Run once immediately on mount (in addition to the interval).
  immediate?: boolean;
  // Also refetch on window `focus` — for polls whose staleness is felt the
  // instant the user looks at the app again.
  onFocus?: boolean;
}

const DEDUPE_MS = 800;

export function usePoll(fn: () => void, ms: number, opts: PollOpts = {}): void {
  const { enabled = true, hiddenMs = null, immediate = false, onFocus = false } = opts;
  // The callback is read through a ref so a fresh closure each render doesn't
  // tear down and re-arm the interval (which would reset its phase forever).
  const cb = useRef(fn);
  cb.current = fn;
  const last = useRef(0);

  useEffect(() => {
    if (!enabled) return;
    let id: number | undefined;
    const hidden = () => document.visibilityState === "hidden";
    const run = () => {
      last.current = Date.now();
      cb.current();
    };
    const runDeduped = () => {
      if (Date.now() - last.current < DEDUPE_MS) return;
      run();
    };
    const arm = () => {
      if (id !== undefined) {
        clearInterval(id);
        id = undefined;
      }
      const period = hidden() ? hiddenMs : ms;
      if (period == null) return; // paused while hidden
      id = window.setInterval(run, period);
    };
    const onVisibility = () => {
      arm();
      if (!hidden()) runDeduped();
    };

    if (immediate) run();
    arm();
    document.addEventListener("visibilitychange", onVisibility);
    if (onFocus) window.addEventListener("focus", runDeduped);
    return () => {
      if (id !== undefined) clearInterval(id);
      document.removeEventListener("visibilitychange", onVisibility);
      if (onFocus) window.removeEventListener("focus", runDeduped);
    };
  }, [enabled, ms, hiddenMs, immediate, onFocus]);
}

// The one poll that keeps running (slowly) in a hidden tab: sessions. 60s
// matches the browser's own background-timer throttle, so this costs nothing
// versus today while keeping the tab title and app badge fresh.
export const HIDDEN_SESSIONS_MS = 60_000;
