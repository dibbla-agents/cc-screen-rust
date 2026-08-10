import { memo, useEffect, useRef, useState } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { SearchAddon } from "@xterm/addon-search";
import { wsURL } from "../api";
import { attachRenderer } from "../xtermRenderer";
import TerminalFindBar from "./TerminalFindBar";
import { makeOsc52Handler } from "../osc52Handler";
import {
  ATTACH_QUIET_MS,
  claimClipboardSurface,
  noteSessionInput,
  publishClipboardWrote,
  releaseClipboardSurface,
  sessionKey,
} from "../osc52Bus";

export type ConnState = "connecting" | "open" | "closed";

// The BROWSER's platform, not the session host's: the force-selection modifier
// is xterm.js's, and xterm.js runs here. A Mac browser attached to a Linux
// agent still needs ⌥.
const IS_MAC = typeof navigator !== "undefined" && /Mac|iPad|iPhone|iPod/i.test(navigator.userAgent);
const isCoarse = () =>
  typeof matchMedia !== "undefined" && matchMedia("(pointer: coarse)").matches;

interface Props {
  session: string;
  // The machine (agent) owning this session, threaded onto the WS URL so a hub
  // routes the attach to the right agent. "" / undefined for a single agent.
  machine?: string;
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
  // Bump to open this pane's find bar (Ctrl+B / from App, or the pane menu on
  // touch). Only the active pane reacts, so one counter serves every pane.
  // Terminal output is drawn as pixels under the WebGL renderer, so the
  // browser's own Cmd/Ctrl+F can no longer find it — this replaces it
  // (proposal 0068 Part E; amends 0027's browser-Find policy).
  searchSignal?: number;
}

// One TerminalView per session (parent remounts via key={session}). It owns the
// xterm instance and the WebSocket, reconnecting on drop — because all state
// lives in tmux, a reconnect re-attaches exactly where the agent left off.
function TerminalView({
  session,
  machine,
  fontSize,
  onState,
  active = true,
  onTerm,
  searchSignal = 0,
}: Props) {
  const hostRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const searchRef = useRef<SearchAddon | null>(null);
  const wsRef = useRef<WebSocket | null>(null);
  const fitTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [findOpen, setFindOpen] = useState(false);

  // ── clipboard + gesture state (proposals 0077, 0031) ───────────────────────
  //
  // Everything the OSC 52 handler and the touch ladder read lives in refs: both
  // run outside React's render cycle (an xterm parser callback and a
  // requestAnimationFrame loop) and must see the CURRENT value, not the one
  // captured when their effect was built.
  const activeRef = useRef(active);
  activeRef.current = active;
  const keyRef = useRef(sessionKey(session, machine));
  keyRef.current = sessionKey(session, machine);
  const labelRef = useRef("");
  labelRef.current = machine ? `${session} · ${machine}` : session;
  // A5: this surface's identity in the one-acting-surface arbiter.
  const surfaceToken = useRef({}).current;
  // A10: no OSC 52 handling before this timestamp (attach / Lagged resync).
  const quietUntilRef = useRef(0);
  // 0031 C1: the alternate screen, cached off onBufferChange so flush() (which
  // runs ~40×/s) never walks the buffer object graph.
  const altRef = useRef(false);
  // The last touch point, for the 1-based cell a mouse report carries.
  const lastTouchRef = useRef<{ x: number; y: number } | null>(null);
  // 0077 B2: while on, a plain drag selects even though the application owns
  // the mouse. Mirrored into a ref for the capture-phase pointer shim.
  const [selectMode, setSelectMode] = useState(false);
  const selectModeRef = useRef(false);
  selectModeRef.current = selectMode;
  // 0077 B1: does the attached application have mouse tracking on? Drives the
  // "how do I select text here" affordance.
  const [mouseMode, setMouseMode] = useState(false);

  // Build the terminal once.
  useEffect(() => {
    const host = hostRef.current!;
    const term = new Terminal({
      // Steady cursor, deliberately. A blinking cursor is pure client-side
      // paint — it never touches the PTY in either direction — but under any
      // renderer it keeps the browser's style/paint pipeline running every
      // vsync forever, which is what made an idle tab burn 8-20% of a core.
      // A mirror of a remote agent gains nothing from a blinking caret.
      cursorBlink: false,
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
    const search = new SearchAddon();
    term.loadAddon(search);
    searchRef.current = search;
    term.open(host);
    // WebGL rendering where available, DOM renderer everywhere else (see
    // xtermRenderer.ts). Must come after open() — the addon needs the screen
    // element.
    const disposeRenderer = attachRenderer(term);
    fit.fit();
    termRef.current = term;
    fitRef.current = fit;
    onTerm?.(term);
    // Debug hook: expose the active pane's term as window.__ccTerm so the
    // smoke test (and curious humans) can poke at selection state without
    // having to wire a React ref through the test harness. The last
    // mounted/most-recently active pane wins; harmless in production.
    (window as unknown as { __ccTerm?: Terminal }).__ccTerm = term;

    // OSC 52 — a copy performed INSIDE the session (proposal 0077 Part A).
    // xterm.js 5.5 ships no built-in handler for 52, so without this the
    // assistant's every /copy, copy-on-select and Ctrl+C is parsed and thrown
    // away. The handler is built from the pure payload module plus the runtime
    // gates; it returns `true` synchronously (never a Promise — see A9), so the
    // sequence is consumed and the parser never stalls on a clipboard write.
    // Note the module it comes from has no way to write to the PTY, which is
    // what makes the `?` query form structurally unanswerable (A1).
    const oscSub = term.parser.registerOscHandler(
      52,
      makeOsc52Handler({
        key: () => keyRef.current,
        label: () => labelRef.current,
        token: surfaceToken,
        eligible: () => activeRef.current,
        // A4: DOM focus on THIS terminal and a focused document. xterm 5.5
        // exposes no public focus event, but `textarea` is public API.
        focused: () =>
          !!term.textarea &&
          document.activeElement === term.textarea &&
          (typeof document.hasFocus !== "function" || document.hasFocus()),
        quietUntil: () => quietUntilRef.current,
        onWrote: (bytes) => publishClipboardWrote({ label: labelRef.current, bytes }),
      })
    );

    // 0031 C1: cache the active buffer so the touch ladder can branch on it
    // cheaply, and re-read the mouse mode with it — Claude Code's fullscreen
    // renderer flips `?1049` and the mouse modes in the same burst, so the
    // buffer change is the cheapest reliable trigger for the 0077 B1
    // affordance (there is no mode-change event in xterm 5.5).
    altRef.current = term.buffer.active.type === "alternate";
    const bufSub = term.buffer.onBufferChange(() => {
      altRef.current = term.buffer.active.type === "alternate";
      window.setTimeout(refreshMouseMode, 60);
    });
    refreshMouseMode();

    return () => {
      onTerm?.(null);
      oscSub.dispose();
      bufSub.dispose();
      releaseClipboardSurface(keyRef.current, surfaceToken);
      disposeRenderer();
      term.dispose();
      termRef.current = null;
      fitRef.current = null;
      searchRef.current = null;
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

  // Focus the terminal WITHOUT letting the browser scroll it into view. xterm's
  // term.focus() calls focus() on its hidden .xterm-helper-textarea, which it
  // positions absolutely at the cursor cell — often far down a tall buffer. A
  // plain focus there triggers the browser's "scroll focused element into view"
  // pass, which sets scrollTop on an overflow:hidden ancestor (#root / the app
  // shell) and shoves the header off-screen. Focusing the helper textarea
  // directly with { preventScroll: true } removes that at the source; we fall
  // back to term.focus() if the helper isn't found (selector is stable across
  // xterm 5.x). See cc-screen-saas docs/proposals/archived/0004-scroll-jump-fix.md.
  function focusTerminal() {
    const term = termRef.current;
    if (!term) return;
    const ta = hostRef.current?.querySelector<HTMLTextAreaElement>(".xterm-helper-textarea");
    if (ta) ta.focus({ preventScroll: true });
    else term.focus();
  }

  // Does the attached application have mouse tracking on? When it does, xterm
  // hands a plain drag to the application instead of starting a selection, so
  // the shipped ⌘C/Ctrl+C copy path has nothing to copy unless the user knows
  // the force-selection modifier (proposal 0077 Part B). Re-read on buffer
  // change and on pointer-down — xterm 5.5 has no mode-change event, and
  // polling for one would be exactly the idle work 0068 removed.
  function refreshMouseMode() {
    const term = termRef.current;
    if (!term) return;
    setMouseMode(term.modes.mouseTrackingMode !== "none");
  }

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
    // A grow (rows increasing) is the keyboard closing / more space appearing.
    // The agent repaints on the resize using absolute cursor positioning, and on
    // a grow that can leave the viewport scrolled up with the prompt below the
    // fold (so the user must scroll back down). Re-anchor to the bottom on a grow
    // so the prompt stays visible; the streamed repaint then sticks to the
    // bottom. Don't do this on a shrink (keyboard opening) — that path is fine.
    const grew = dims.rows > term.rows;
    term.resize(cols, dims.rows);
    if (grew) term.scrollToBottom();
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
      const ws = new WebSocket(wsURL(session, machine));
      ws.binaryType = "arraybuffer";
      wsRef.current = ws;

      ws.onopen = () => {
        backoff = 500;
        onState("open");
        const term = termRef.current!;
        // 0077 A10: the attach snapshot replays the session's recent output
        // verbatim, so an OSC 52 emitted minutes ago would otherwise write the
        // clipboard "just now". Stay quiet until the backlog has drained.
        quietUntilRef.current = Date.now() + ATTACH_QUIET_MS;
        applyFit();
        // Always report the size on (re)attach, even if applyFit found no change
        // — the server needs it to register this client in its min-size pool.
        ws.send(JSON.stringify({ t: "r", c: term.cols, r: term.rows }));
        // Don't grab focus on touch devices: it pops the soft keyboard, which
        // then eats the first tap on the compose/image buttons. Tap the
        // terminal to type. On desktop, focus for immediate typing.
        const coarse =
          typeof matchMedia !== "undefined" && matchMedia("(pointer: coarse)").matches;
        if (!coarse) focusTerminal();
      };
      ws.onmessage = (e) => {
        if (e.data instanceof ArrayBuffer) {
          const u8 = new Uint8Array(e.data);
          // A chunk that STARTS with the RIS reset is a snapshot replay — a
          // fresh attach or a Lagged resync. Same rule as onopen: replayed
          // backlog must not write the clipboard (0077 A10).
          if (u8.length >= 2 && u8[0] === 0x1b && u8[1] === 0x63) {
            quietUntilRef.current = Date.now() + ATTACH_QUIET_MS;
          }
          termRef.current?.write(u8);
        } else if (typeof e.data === "string") {
          if (e.data.startsWith("\x1bc")) quietUntilRef.current = Date.now() + ATTACH_QUIET_MS;
          termRef.current?.write(e.data);
        }
      };
      ws.onclose = () => {
        // Only report a drop we didn't cause. On unmount/reconnect-teardown
        // (closedByUs) the cleanup closes this socket deliberately — and because
        // the pane's conn slot is shared, a new session may already own it and
        // have reported "open". Stamping "closed" here would clobber that new
        // session's state, leaving its dot red while its socket is wide open.
        if (closedByUs) return;
        onState("closed");
        retry = setTimeout(connect, backoff);
        backoff = Math.min(backoff * 2, 5000);
      };
      ws.onerror = () => ws.close();
    };

    // PWA resume. When the phone wakes from standby the OS often kills the
    // socket WITHOUT firing onclose — it can even keep reporting readyState
    // OPEN (a zombie) — so the onclose-only reconnect above never runs and the
    // terminal stays frozen on the bytes from before standby. On every
    // hidden->visible transition we re-attach: a fresh connection replays the
    // server snapshot (\x1bc RIS + raw ring) and repaints wherever the agent
    // actually is now.
    const reconnectNow = () => {
      if (closedByUs) return;
      if (retry) {
        clearTimeout(retry);
        retry = null;
      }
      backoff = 500;
      const stale = wsRef.current;
      if (stale) {
        // Detach handlers first so this deliberate close doesn't schedule its
        // own retry — reconnectNow opens the replacement itself.
        stale.onopen = stale.onmessage = stale.onclose = stale.onerror = null;
        try {
          stale.close();
        } catch {
          /* already closing */
        }
      }
      connect();
    };
    const onResume = () => {
      if (document.visibilityState !== "visible") return;
      const ws = wsRef.current;
      if (ws && ws.readyState === WebSocket.CONNECTING) return; // connect in flight
      const open = ws && ws.readyState === WebSocket.OPEN;
      const coarse =
        typeof matchMedia !== "undefined" && matchMedia("(pointer: coarse)").matches;
      // Desktop sockets usually survive a tab switch and fire onclose cleanly,
      // so only reconnect there if the socket is provably down. Touch devices —
      // where standby silently zombifies the socket — always re-attach.
      if (!open || coarse) reconnectNow();
    };
    document.addEventListener("visibilitychange", onResume);
    window.addEventListener("pageshow", onResume);
    window.addEventListener("online", onResume);

    // Direct typing in the terminal -> stdin over the WebSocket.
    const term = termRef.current!;
    const dataSub = term.onData((d) => {
      // 0077 A4: this client is DRIVING this session — keystrokes, pastes and
      // the touch ladder's mouse reports all count, because all three are the
      // user acting on that session with their own hands. Watching a session
      // you never touch never qualifies, which is the whole point of the gate.
      noteSessionInput(keyRef.current);
      const ws = wsRef.current;
      if (ws && ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({ t: "i", d }));
      }
    });

    connect();

    return () => {
      closedByUs = true;
      if (retry) clearTimeout(retry);
      document.removeEventListener("visibilitychange", onResume);
      window.removeEventListener("pageshow", onResume);
      window.removeEventListener("online", onResume);
      dataSub.dispose();
      wsRef.current?.close();
      wsRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [session, machine]);

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
    focusTerminal();
  }, [active]);

  // Open the find bar when the parent bumps searchSignal — but only in the
  // active pane, so one counter can serve every pane. The last-seen value is
  // tracked in a ref (not just effect deps) so that *activating* a pane after
  // someone searched in another one doesn't pop this pane's find bar too.
  const lastSearchSignal = useRef(searchSignal);
  useEffect(() => {
    if (searchSignal === lastSearchSignal.current) return;
    lastSearchSignal.current = searchSignal;
    if (!active) return;
    setFindOpen(true);
  }, [searchSignal, active]);

  function closeFind() {
    setFindOpen(false);
    searchRef.current?.clearDecorations();
    focusTerminal();
  }

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

  // Swipe-to-scroll with inertia (cc-screen-saas proposal 0031, Strategies A
  // and C — they ship together and neither works alone).
  //
  // Phones emit no wheel events, so a finger drag needs a custom handler. Two
  // things were wrong with the old one:
  //
  //   A — it claimed the gesture LATE. `touchstart` was passive and the first
  //   8px of every swipe was swallowed by the tap deadband before
  //   preventDefault() ever fired, and there was no `touch-action` on the host,
  //   so the opening of every swipe leaked to the browser's gesture recognizer
  //   and to whatever xterm layer sat under the start point — the
  //   cursor-positioned .xterm-helper-textarea, a WebLinks span. That is why a
  //   swipe starting at the edge scrolled and one starting in the middle
  //   twitched and stalled. Now: `touch-action: none` on the host (index.css)
  //   plus capture-phase stopPropagation from touchstart. We deliberately do
  //   NOT preventDefault on touchstart — the browser's compatibility mouse
  //   events for a *tap* are how tap-to-focus and Claude Code's own click
  //   targets work; only a classified drag (past the deadband) cancels them.
  //
  //   C — its sink was a no-op wherever it mattered most. `scrollLines()` on
  //   the ALTERNATE buffer does nothing at all (the alt buffer is built with
  //   hasScrollback=false), and Claude Code's fullscreen renderer lives there.
  //   flush() is now the same three-rung precedence ladder [0069] shipped in
  //   the ccs TUI, so both clients behave identically in front of the same
  //   child application: normal buffer → xterm's scrollback; alt screen with
  //   mouse reporting → SGR wheel reports; alt screen without → arrow keys.
  //
  // Direction stays direct-manipulation: drag down reveals older output.
  useEffect(() => {
    const host = hostRef.current!;

    const GAIN = 5; // lines scrolled per finger-line of travel (>1 = faster)
    // Rungs 2 and 3 are not free: every line is a keystroke or a mouse report
    // the APPLICATION must parse and act on, and GAIN plus a momentum fling
    // routinely produces dozens of lines per flush. Matches [0069]'s MOUSE_STEP
    // of 3 reports per wheel notch, which is the feel these apps are tuned for.
    // The surplus is DISCARDED, never carried — a one-second fling must not
    // queue input the TUI keeps chewing through after the finger has left.
    const MAX_STEPS_PER_FLUSH = 3;
    const cellPx = () => {
      const term = termRef.current;
      const h = host.clientHeight;
      return term && term.rows > 0 ? Math.max(8, h / term.rows) : 18;
    };

    // The 1-based cell a mouse report names, clamped inside the pane. The
    // momentum phase flushes AFTER touchend, so the point is cached during the
    // drag; with none (an early flush) we report the pane centre — Claude Code
    // and htop both scroll the pane the report lands in, and the centre is
    // always inside it.
    const reportCell = (term: Terminal) => {
      const clamp = (n: number, hi: number) => Math.max(1, Math.min(Math.max(1, hi), n));
      const rect = term.element?.getBoundingClientRect();
      const pt = lastTouchRef.current;
      if (!rect || rect.width < 1 || rect.height < 1 || !pt) {
        return { col: clamp(Math.ceil(term.cols / 2), term.cols), row: clamp(Math.ceil(term.rows / 2), term.rows) };
      }
      const cw = rect.width / Math.max(1, term.cols);
      const ch = rect.height / Math.max(1, term.rows);
      return {
        col: clamp(Math.floor((pt.x - rect.left) / cw) + 1, term.cols),
        row: clamp(Math.floor((pt.y - rect.top) / ch) + 1, term.rows),
      };
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
      if (whole === 0) return;
      pending -= whole; // the surplus over the clamp below is DISCARDED with it
      lastFlush = now;
      const term = termRef.current;
      if (!term) return;

      // Rung 1 — the normal buffer, or an alt viewport the user has scrolled
      // back locally: xterm's own scrollback, exactly as before. This rung
      // fails CLOSED: anything the ladder doesn't recognise moves pixels, never
      // bytes. whole>0 = back into history (older); scrollLines(negative)
      // scrolls up toward older output.
      const buf = term.buffer.active;
      if (!altRef.current || buf.viewportY !== buf.baseY) {
        term.scrollLines(-whole);
        return;
      }

      const up = whole > 0; // finger down => older output => wheel up
      const n = Math.min(Math.abs(whole), MAX_STEPS_PER_FLUSH);

      // Rung 2 — the alternate screen AND the application is reading the mouse
      // (Claude Code's fullscreen renderer, htop, lazygit): speak its protocol.
      // term.input() fires onData, which is the same {t:"i"} path a keystroke
      // takes — no new wire message, no hub or agent change.
      if (term.modes.mouseTrackingMode !== "none") {
        const { col, row } = reportCell(term);
        const btn = up ? 64 : 65; // SGR wheel-up / wheel-down
        term.input(`\x1b[<${btn};${col};${row}M`.repeat(n), false);
        return;
      }

      // Rung 3 — the alternate screen without mouse reporting (less, a plain
      // vim): alternate-scroll arrows. xterm.js 5.5 does not implement DECSET
      // ?1007 at all, so the client owns this rung; the encoding follows the
      // application-cursor-keys mode the child asked for.
      const app = term.modes.applicationCursorKeysMode;
      term.input((up ? (app ? "\x1bOA" : "\x1b[A") : (app ? "\x1bOB" : "\x1b[B")).repeat(n), false);
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
      lastTouchRef.current = { x: e.touches[0].clientX, y: e.touches[0].clientY };
      scrolling = false;
      samples = [{ t: performance.now(), y: lastY }];
      // Strategy A: own the gesture from the FIRST touch. Capture-phase
      // stopPropagation keeps xterm's own touch handlers, the
      // cursor-positioned .xterm-helper-textarea and any WebLinks span from
      // starting a competing interaction. No preventDefault here — see the
      // header comment: a tap's compatibility mouse events are load-bearing.
      e.stopPropagation();
    };
    const onMove = (e: TouchEvent) => {
      if (e.touches.length !== 1) return;
      const y = e.touches[0].clientY;
      const dy = y - lastY;
      lastY = y;
      lastTouchRef.current = { x: e.touches[0].clientX, y };
      // Suppression starts at touchstart; the deadband now only CLASSIFIES.
      // Below it we still keep the event to ourselves, but leave it
      // cancellable-but-uncancelled so a tap still becomes a click report —
      // that is how tapping a permission prompt inside Claude Code works.
      e.stopPropagation();
      if (!scrolling && Math.abs(y - startY) < 8) return; // still could be a tap
      scrolling = true;
      e.preventDefault(); // classified as a drag: no click, no rubber-band
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

    const cap = { capture: true } as EventListenerOptions;
    host.addEventListener("touchstart", onStart, { capture: true, passive: false });
    host.addEventListener("touchmove", onMove, { capture: true, passive: false });
    host.addEventListener("touchend", onEnd, { passive: true });
    host.addEventListener("touchcancel", onEnd, { passive: true });
    return () => {
      stopMomentum();
      if (raf) cancelAnimationFrame(raf);
      host.removeEventListener("touchstart", onStart, cap);
      host.removeEventListener("touchmove", onMove, cap);
      host.removeEventListener("touchend", onEnd);
      host.removeEventListener("touchcancel", onEnd);
    };
  }, []);

  // 0077 B2 — the "Select text" toggle. xterm.js exposes no option to suppress
  // mouse reporting, and its force-selection test reads e.altKey/e.shiftKey
  // directly, so the only way in is to re-dispatch each mouse event with the
  // platform's force-selection modifier set and cancel the original. Scoped to
  // the host, off by default, and a no-op the rest of the time — when the
  // toggle is off the mouse reports a TUI legitimately needs still reach it.
  useEffect(() => {
    const host = hostRef.current!;
    const FORCED = "__ccForcedSelection";
    const relay = (e: MouseEvent) => {
      if (!selectModeRef.current) return;
      const marked = e as MouseEvent & { [FORCED]?: boolean };
      if (marked[FORCED]) return; // our own clone, on its way to xterm
      if (IS_MAC ? e.altKey : e.shiftKey) return; // already a force-selection drag
      e.preventDefault();
      e.stopPropagation();
      const clone = new MouseEvent(e.type, {
        bubbles: true,
        cancelable: true,
        view: window,
        clientX: e.clientX,
        clientY: e.clientY,
        screenX: e.screenX,
        screenY: e.screenY,
        button: e.button,
        buttons: e.buttons,
        detail: e.detail,
        ctrlKey: e.ctrlKey,
        metaKey: e.metaKey,
        altKey: IS_MAC ? true : e.altKey,
        shiftKey: IS_MAC ? e.shiftKey : true,
      }) as MouseEvent & { [FORCED]?: boolean };
      clone[FORCED] = true;
      e.target?.dispatchEvent(clone);
    };
    const types: (keyof HTMLElementEventMap)[] = ["mousedown", "mousemove", "mouseup"];
    types.forEach((t) => host.addEventListener(t, relay as EventListener, true));
    return () => types.forEach((t) => host.removeEventListener(t, relay as EventListener, true));
  }, []);

  // Keep the B1 affordance honest without polling: a pointer-down is the moment
  // just before a user tries to select, and it is free.
  useEffect(() => {
    const host = hostRef.current!;
    const onDown = () => refreshMouseMode();
    host.addEventListener("pointerdown", onDown, true);
    return () => host.removeEventListener("pointerdown", onDown, true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // A5: the active pane is this session's acting clipboard surface. The editor
  // overlay's AgentMirror claims it while it is open (it is what the user is
  // actually looking at) and hands it back on close.
  useEffect(() => {
    if (!active) return;
    const key = sessionKey(session, machine);
    claimClipboardSurface(key, surfaceToken);
    return () => releaseClipboardSurface(key, surfaceToken);
  }, [active, session, machine, surfaceToken]);

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
  // 0077 B1: when the application owns the mouse, a plain drag is a mouse
  // report and creates no selection — so the shipped ⌘C/Ctrl+C copy has nothing
  // to copy. The escape hatch has always existed; it was discoverable only
  // through a one-shot toast. Name it, for the BROWSER's platform, whenever it
  // applies. Meaningless on touch, so it does not render there.
  const showSelectHint = active && mouseMode && !isCoarse();

  return (
    <div className="relative h-full w-full bg-bar pl-2 pt-1.5">
      <div ref={hostRef} className="cc-term-host h-full w-full" />
      {showSelectHint && (
        <div className="pointer-events-none absolute bottom-1 right-2 z-10 flex items-center gap-1.5 text-[10px] text-slate-500">
          <span>{IS_MAC ? "⌥" : "Shift"}-drag to select</span>
          <button
            type="button"
            onClick={() => setSelectMode((v) => !v)}
            title={
              selectMode
                ? "Plain drag selects text (the app stops seeing the mouse)"
                : "Let a plain drag select text in this pane"
            }
            className={`pointer-events-auto rounded border border-edge px-1.5 py-0.5 ${
              selectMode ? "bg-accent text-bar" : "text-slate-400 hover:text-slate-200"
            }`}
          >
            Select text
          </button>
        </div>
      )}
      {findOpen && <TerminalFindBar search={searchRef} onClose={closeFind} />}
    </div>
  );
}

// Memoized: the 4s session poll re-renders App, and without this every mounted
// xterm pane re-renders with it. Props are stabilized by the parent
// (TileGrid/App useCallback), so the default shallow compare holds.
export default memo(TerminalView);
