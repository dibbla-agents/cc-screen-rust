import { useCallback, useEffect, useMemo, useRef } from "react";
import { watchURL } from "../api";

// A filesystem-change event from the server: a watched directory changed, with
// the (debounced, coalesced) set of child paths touched.
export type FsListener = (dir: string, paths: string[]) => void;

export interface FileWatch {
  // Ref-counted: each consumer subscribes/unsubscribes a directory; the server
  // only watches it while at least one consumer wants it.
  subscribe: (dir: string) => void;
  unsubscribe: (dir: string) => void;
  // Register an event listener; returns an unsubscribe function.
  addListener: (fn: FsListener) => () => void;
}

// useFileWatch — one reconnecting WebSocket to /api/watch shared across the
// editor (file tree + open file). Subscriptions are ref-counted and replayed on
// reconnect; `enabled` gates the whole connection (the editor only watches while
// it's open). The wanted set lives in a ref so it survives reconnects and never
// re-runs the connect effect.
export function useFileWatch(enabled: boolean, machine = ""): FileWatch {
  const wsRef = useRef<WebSocket | null>(null);
  const want = useRef<Map<string, number>>(new Map()); // dir -> refcount
  const listeners = useRef<Set<FsListener>>(new Set());

  const send = (t: "sub" | "unsub", dir: string) => {
    const ws = wsRef.current;
    if (ws && ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify({ t, dirs: [dir] }));
  };

  useEffect(() => {
    if (!enabled) return;
    let closedByUs = false;
    let retry: ReturnType<typeof setTimeout> | null = null;
    let backoff = 500;

    const connect = () => {
      const ws = new WebSocket(watchURL(machine));
      wsRef.current = ws;
      ws.onopen = () => {
        backoff = 500;
        // Replay the whole wanted set so a reconnect re-establishes every watch.
        const dirs = [...want.current.keys()];
        if (dirs.length) ws.send(JSON.stringify({ t: "sub", dirs }));
      };
      ws.onmessage = (e) => {
        if (typeof e.data !== "string") return;
        try {
          const m = JSON.parse(e.data);
          if (m && m.t === "fs" && typeof m.dir === "string") {
            const paths: string[] = Array.isArray(m.paths) ? m.paths : [];
            listeners.current.forEach((fn) => fn(m.dir, paths));
          }
        } catch {
          /* ignore malformed frames */
        }
      };
      ws.onclose = () => {
        wsRef.current = null;
        if (closedByUs) return;
        retry = setTimeout(connect, backoff);
        backoff = Math.min(backoff * 2, 5000);
      };
      ws.onerror = () => ws.close();
    };
    connect();

    return () => {
      closedByUs = true;
      if (retry) clearTimeout(retry);
      wsRef.current?.close();
      wsRef.current = null;
    };
    // Reconnect when the target machine changes (editor machine switcher) so the
    // watch follows the browsed agent.
  }, [enabled, machine]);

  const subscribe = useCallback((dir: string) => {
    if (!dir) return;
    const n = (want.current.get(dir) || 0) + 1;
    want.current.set(dir, n);
    if (n === 1) send("sub", dir);
  }, []);

  const unsubscribe = useCallback((dir: string) => {
    if (!dir) return;
    const n = (want.current.get(dir) || 0) - 1;
    if (n <= 0) {
      want.current.delete(dir);
      send("unsub", dir);
    } else {
      want.current.set(dir, n);
    }
  }, []);

  const addListener = useCallback((fn: FsListener) => {
    listeners.current.add(fn);
    return () => {
      listeners.current.delete(fn);
    };
  }, []);

  return useMemo(() => ({ subscribe, unsubscribe, addListener }), [subscribe, unsubscribe, addListener]);
}
