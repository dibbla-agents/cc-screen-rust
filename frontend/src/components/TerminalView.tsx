import { useEffect, useRef } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { wsURL } from "../api";

export type ConnState = "connecting" | "open" | "closed";

interface Props {
  session: string;
  fontSize: number;
  onState: (s: ConnState) => void;
  // True when this pane is the active one in the parent's TileGrid. Used to
  // move DOM focus to this terminal whenever the parent flips the flag —
  // otherwise keyboard pane-nav (Ctrl+B + arrow) updates the React active
  // state without moving focus, and your next keystroke still lands in the
  // previously-clicked terminal. Mouse clicks self-focus via xterm's own
  // canvas click handler, so they don't need this; keyboard nav does.
  // Defaults to true so the single-pane (phone) path works unchanged.
  active?: boolean;
  // Surface the underlying xterm.js Terminal to the parent so a global
  // shortcut (Cmd+C / Ctrl+C copy in App.tsx) can read the active pane's
  // current selection. Called with the live instance on mount and `null`
  // on unmount — the parent stores it by pane index.
  onTerm?: (term: Terminal | null) => void;
}

// One TerminalView per session (parent remounts via key={session}). It owns the
// xterm instance and the WebSocket, reconnecting on drop — because all state
// lives in tmux, a reconnect re-attaches exactly where the agent left off.
export default function TerminalView({ session, fontSize, onState, active = true, onTerm }: Props) {
  const hostRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const wsRef = useRef<WebSocket | null>(null);
  const fitTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Build the terminal once.
  useEffect(() => {
    const host = hostRef.current!;
    const term = new Terminal({
      cursorBlink: true,
      fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
      fontSize,
      scrollback: 5000,
      allowProposedApi: true,
      // Force-selection modifier on Mac. xterm.js's shouldForceSelection
      // hard-codes `shiftKey` on Linux/Windows but only checks `altKey`
      // (Option ⌥) on Mac — *and* requires this option to be enabled. Without
      // it, Shift+drag on Mac silently does nothing because mouse mode is on
      // and tmux eats every drag. With it, Option+drag becomes the standard
      // "select past mouse mode" gesture, matching the iTerm2/xterm.js
      // convention. The hint toast in App.tsx tells Mac users to use Option;
      // Linux/Windows users still use Shift (no opt-in needed on those).
      macOptionClickForcesSelection: true,
      theme: {
        background: "#0f1720",
        foreground: "#d7dee8",
        cursor: "#38bdf8",
        // Selection background: semi-transparent accent cyan. The previous
        // value (#243042 — the panel-edge color) is only marginally brighter
        // than the terminal background and the selection was effectively
        // invisible, which made Shift+drag-to-select feel broken. rgba() is
        // supported from xterm.js v5+; the alpha lets the cell's own
        // foreground colour shine through so text stays legible without
        // setting an opaque selectionForeground.
        selectionBackground: "rgba(56, 189, 248, 0.4)",
        selectionInactiveBackground: "rgba(56, 189, 248, 0.18)",
        black: "#0f1720",
        brightBlack: "#3b4759",
      },
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.loadAddon(new WebLinksAddon());
    term.open(host);
    fit.fit();
    termRef.current = term;
    fitRef.current = fit;
    onTerm?.(term);
    // Debug hook: expose the active pane's term as window.__ccTerm so the
    // smoke test (and curious humans) can poke at selection state without
    // having to wire a React ref through the test harness. The last
    // mounted/most-recently active pane wins; harmless in production.
    (window as unknown as { __ccTerm?: Terminal }).__ccTerm = term;

    return () => {
      onTerm?.(null);
      term.dispose();
      termRef.current = null;
      fitRef.current = null;
    };
    // fontSize change handled separately to avoid tearing down the socket.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Apply font-size changes live.
  useEffect(() => {
    const term = termRef.current;
    const fit = fitRef.current;
    if (!term || !fit) return;
    term.options.fontSize = fontSize;
    // A deliberate font change legitimately changes cols, so let it reflow now
    // (applyFit resizes + reports). Unlike incidental jitter, the user asked for
    // this, and the agent repaints its visible frame crisply at the new width.
    applyFit();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [fontSize]);

  function sendResize() {
    const term = termRef.current;
    const ws = wsRef.current;
    if (!term || !ws || ws.readyState !== WebSocket.OPEN) return;
    ws.send(JSON.stringify({ t: "r", c: term.cols, r: term.rows }));
  }

  // Resize the grid to fit the host — but only when the column/row count
  // genuinely changes, and with a one-column deadband. xterm reflows its ENTIRE
  // buffer whenever `cols` changes, and the agent emits width-locked output
  // (each word placed with an absolute cursor-column escape, computed for the
  // PTY width), so reflowing that to a different width shreds the scrollback
  // into the per-word "staircase" the phone was showing. Incidental viewport
  // churn — soft-keyboard show/hide, address-bar collapse, sub-pixel rounding —
  // must therefore NOT trigger a reflow. The PTY tracks whatever width we settle
  // on (the server pins it to the narrowest attached client), so swallowing a
  // ±1-column wobble costs at most a sliver of right-edge padding, never
  // correctness. Rows may change freely: vertical growth/shrink adds or removes
  // lines, it never rewraps.
  function applyFit() {
    const term = termRef.current;
    const fit = fitRef.current;
    if (!term || !fit) return;
    const dims = fit.proposeDimensions();
    if (!dims || !Number.isFinite(dims.cols) || !Number.isFinite(dims.rows)) return;
    if (dims.cols < 1 || dims.rows < 1) return;
    let cols = dims.cols;
    if (term.cols > 0 && Math.abs(cols - term.cols) < 2) cols = term.cols; // deadband
    if (cols === term.cols && dims.rows === term.rows) return;
    term.resize(cols, dims.rows);
    sendResize();
  }

  // Debounce viewport-driven fits: a keyboard animation or rotation fires a
  // burst of resize events, and we want a single fit once the layout settles —
  // not a reflow per intermediate frame.
  function scheduleFit() {
    if (fitTimer.current) clearTimeout(fitTimer.current);
    fitTimer.current = setTimeout(() => {
      fitTimer.current = null;
      applyFit();
    }, 150);
  }

  // Connect (and reconnect) the WebSocket for the lifetime of this session.
  useEffect(() => {
    let closedByUs = false;
    let retry: ReturnType<typeof setTimeout> | null = null;
    let backoff = 500;

    const connect = () => {
      onState("connecting");
      const ws = new WebSocket(wsURL(session));
      ws.binaryType = "arraybuffer";
      wsRef.current = ws;

      ws.onopen = () => {
        backoff = 500;
        onState("open");
        const term = termRef.current!;
        applyFit();
        // Always report the size on (re)attach, even if applyFit found no change
        // — the server needs it to register this client in its min-size pool.
        ws.send(JSON.stringify({ t: "r", c: term.cols, r: term.rows }));
        // Don't grab focus on touch devices: it pops the soft keyboard, which
        // then eats the first tap on the compose/image buttons. Tap the
        // terminal to type. On desktop, focus for immediate typing.
        const coarse =
          typeof matchMedia !== "undefined" && matchMedia("(pointer: coarse)").matches;
        if (!coarse) term.focus();
      };
      ws.onmessage = (e) => {
        if (e.data instanceof ArrayBuffer) {
          termRef.current?.write(new Uint8Array(e.data));
        } else if (typeof e.data === "string") {
          termRef.current?.write(e.data);
        }
      };
      ws.onclose = () => {
        onState("closed");
        if (closedByUs) return;
        retry = setTimeout(connect, backoff);
        backoff = Math.min(backoff * 2, 5000);
      };
      ws.onerror = () => ws.close();
    };

    // Direct typing in the terminal -> stdin over the WebSocket.
    const term = termRef.current!;
    const dataSub = term.onData((d) => {
      const ws = wsRef.current;
      if (ws && ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({ t: "i", d }));
      }
    });

    connect();

    return () => {
      closedByUs = true;
      if (retry) clearTimeout(retry);
      dataSub.dispose();
      wsRef.current?.close();
      wsRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [session]);

  // Pull keyboard focus into this terminal whenever the parent marks it
  // active. This is the missing half of pane navigation: Ctrl+B + arrow
  // moves React's `active` index but does not touch the DOM, so without
  // this effect the previously-clicked terminal's xterm-helper-textarea
  // still owns focus and eats the next keystroke. Skipped on coarse
  // pointers (phones) so attaching a session doesn't pop the soft
  // keyboard — that case already takes its cue from the WS onopen
  // handler above.
  useEffect(() => {
    if (!active) return;
    const coarse =
      typeof matchMedia !== "undefined" && matchMedia("(pointer: coarse)").matches;
    if (coarse) return;
    termRef.current?.focus();
  }, [active]);

  // Suppress the browser's native context menu inside the terminal. xterm.js
  // defaults `rightClickSelectsWord: true` on Mac (where Ctrl+click is the
  // OS right-click) — useful, but Chrome *also* shows its own context menu
  // on the same gesture, which covers the very word you just selected and
  // generally has no useful entries here (the canvas has no real DOM
  // selection so "Copy" is a no-op). Killing it lets right-click-to-select
  // → ⌘C feel clean. Real inputs elsewhere keep their menus because this
  // listener is scoped to the terminal host.
  useEffect(() => {
    const host = hostRef.current!;
    const onCtx = (e: MouseEvent) => e.preventDefault();
    host.addEventListener("contextmenu", onCtx);
    return () => host.removeEventListener("contextmenu", onCtx);
  }, []);

  // Refit on container resize (rotation, keyboard show/hide, drawer close),
  // debounced + change-gated so incidental viewport churn never reflows the
  // buffer (see applyFit). The keyboard, in particular, only changes height, so
  // this settles to a rows-only resize with no horizontal rewrap.
  useEffect(() => {
    const host = hostRef.current!;
    const ro = new ResizeObserver(() => scheduleFit());
    ro.observe(host);
    return () => {
      ro.disconnect();
      if (fitTimer.current) clearTimeout(fitTimer.current);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Swipe-to-scroll with inertia. Phones emit no wheel events, and because the
  // agents run under `tmux mouse on`, xterm forwards wheel-to-app rather than
  // scrolling its own viewport — so a finger drag does nothing on its own. We
  // map the drag to tmux scrollback directly: finger pixels convert ~1:1 to
  // lines (content tracks the finger), batched per animation frame and sent as
  // {t:"s",n} so tmux scrolls an exact n lines. On release, leftover velocity
  // coasts with decay — a long/fast flick travels far, a short drag barely
  // moves. Direction is direct-manipulation: drag down reveals older output.
  useEffect(() => {
    const host = hostRef.current!;

    const GAIN = 5; // lines scrolled per finger-line of travel (>1 = faster)
    const cellPx = () => {
      const term = termRef.current;
      const h = host.clientHeight;
      return term && term.rows > 0 ? Math.max(8, h / term.rows) : 18;
    };

    let pending = 0; // fractional lines awaiting flush (+ = back into history)
    let raf = 0;
    let lastFlush = 0;
    const flush = (now: number) => {
      raf = 0;
      if (now - lastFlush < 24) {
        // throttle to ~40 msgs/s; keep accumulating until the window opens
        raf = requestAnimationFrame(flush);
        return;
      }
      const whole = Math.trunc(pending);
      if (whole !== 0) {
        pending -= whole;
        lastFlush = now;
        // tmux-free backend: scrollback lives in xterm.js (fed by the live byte
        // stream), so scroll the viewport directly instead of the old
        // server-side {t:"s"} copy-mode round-trip. whole>0 = back into history
        // (older); xterm scrollLines(negative) scrolls up toward older output.
        termRef.current?.scrollLines(-whole);
      }
    };
    const schedule = () => {
      if (!raf) raf = requestAnimationFrame(flush);
    };

    let momentum = 0;
    const stopMomentum = () => {
      if (momentum) {
        cancelAnimationFrame(momentum);
        momentum = 0;
      }
    };

    let startY = 0;
    let lastY = 0;
    let scrolling = false;
    let samples: { t: number; y: number }[] = [];

    const onStart = (e: TouchEvent) => {
      if (e.touches.length !== 1) return;
      stopMomentum(); // a new touch halts a coasting fling
      startY = lastY = e.touches[0].clientY;
      scrolling = false;
      samples = [{ t: performance.now(), y: lastY }];
    };
    const onMove = (e: TouchEvent) => {
      if (e.touches.length !== 1) return;
      const y = e.touches[0].clientY;
      const dy = y - lastY;
      lastY = y;
      if (!scrolling && Math.abs(y - startY) < 8) return; // let taps focus
      scrolling = true;
      // Capture phase + stop/prevent so xterm doesn't start a selection drag
      // or the page rubber-band.
      e.preventDefault();
      e.stopPropagation();
      pending += (dy / cellPx()) * GAIN; // finger down (dy>0) => scroll back
      samples.push({ t: performance.now(), y });
      if (samples.length > 6) samples.shift();
      schedule();
    };
    const onEnd = () => {
      if (!scrolling) return;
      scrolling = false;
      const now = performance.now();
      const recent = samples.filter((s) => now - s.t < 120);
      if (recent.length < 2) return;
      const a = recent[0];
      const b = recent[recent.length - 1];
      const dt = b.t - a.t;
      if (dt <= 0) return;
      let vLines = ((b.y - a.y) / dt / cellPx()) * GAIN; // lines/ms (+ = back)
      if (Math.abs(vLines) * 16 < 0.3) return; // too slow to coast
      let prev = now;
      const step = (ts: number) => {
        const fdt = Math.min(40, ts - prev);
        prev = ts;
        pending += vLines * fdt;
        schedule();
        vLines *= Math.pow(0.94, fdt / 16); // friction
        momentum = Math.abs(vLines) * 16 > 0.15 ? requestAnimationFrame(step) : 0;
      };
      momentum = requestAnimationFrame(step);
    };

    host.addEventListener("touchstart", onStart, { passive: true });
    host.addEventListener("touchmove", onMove, { capture: true, passive: false });
    host.addEventListener("touchend", onEnd, { passive: true });
    host.addEventListener("touchcancel", onEnd, { passive: true });
    return () => {
      stopMomentum();
      if (raf) cancelAnimationFrame(raf);
      host.removeEventListener("touchstart", onStart);
      host.removeEventListener("touchmove", onMove, { capture: true } as EventListenerOptions);
      host.removeEventListener("touchend", onEnd);
      host.removeEventListener("touchcancel", onEnd);
    };
  }, []);

  // Padding only on top + left: visual breathing room between the pane
  // border and the first character / first line. The padding is on a
  // WRAPPER, not the xterm host itself — FitAddon reads dimensions
  // from the host's parent and padding from the inner xterm element,
  // so padding on the host would not be subtracted and the terminal
  // would overflow (last row half-cut, last col bleeding past the
  // edge). Wrapping it pushes the host into the wrapper's content
  // box; the host's h-full w-full then reports correct dimensions.
  // bg-bar (= #0f1720, the xterm theme background) makes the padding
  // strip blend in — no two-tone gutter.
  return (
    <div className="h-full w-full bg-bar pl-2 pt-1.5">
      <div ref={hostRef} className="h-full w-full" />
    </div>
  );
}
