import { useEffect, useRef } from "react";
import { Terminal } from "@xterm/xterm";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { wsURL } from "../api";

export type ConnState = "connecting" | "open" | "closed";

// Per-font cell metrics expressed per 1px of font size (multiply by fontSize to
// get CSS px). Width is a single 'M' advance; height is the font's bounding box
// — the same metric xterm derives its cell dimensions from, so the fitted font
// lands within a pixel of xterm's actual layout. Cached: the family never
// changes at runtime.
export interface CellRatios {
  w: number;
  h: number;
}
let cachedFamily = "";
let cachedRatios: CellRatios = { w: 0.6, h: 1.2 };
export function cellRatios(family: string): CellRatios {
  if (family === cachedFamily) return cachedRatios;
  if (typeof document !== "undefined") {
    const ctx = document.createElement("canvas").getContext("2d");
    if (ctx) {
      const ref = 100;
      ctx.font = `${ref}px ${family}`;
      const m = ctx.measureText("M");
      const asc = (m as TextMetrics & { fontBoundingBoxAscent?: number }).fontBoundingBoxAscent;
      const desc = (m as TextMetrics & { fontBoundingBoxDescent?: number }).fontBoundingBoxDescent;
      cachedRatios = {
        w: m.width / ref || 0.6,
        h: asc != null && desc != null ? (asc + desc) / ref : 1.2,
      };
    }
  }
  cachedFamily = family;
  return cachedRatios;
}

// Largest integer font size at which a `cols`×`rows` grid still fits inside an
// `availW`×`availH` box, capped at `maxFont`. Flooring keeps the rendered width
// at or under the box, so the worst case is a one-pixel right-edge clip (the
// host is overflow-hidden), never a wrap. The 0.997 nudge absorbs xterm's
// per-cell pixel rounding.
export function fitFontSize(
  cols: number,
  rows: number,
  availW: number,
  availH: number,
  ratios: CellRatios,
  maxFont: number
): number {
  if (cols < 1 || rows < 1 || availW < 1 || availH < 1) return Math.min(maxFont, 12);
  const byWidth = (availW * 0.997) / (cols * ratios.w);
  const byHeight = availH / (rows * ratios.h);
  return Math.max(4, Math.min(maxFont, Math.floor(Math.min(byWidth, byHeight))));
}

// An agent stays usable down to roughly this width; we never report narrower
// than this even in a very thin column (the font just shrinks to fit instead).
export const MIN_AGENT_COLS = 40;

// The number of columns to render — and REPORT — for a column `hostW` px wide:
// as many as fit at the readable target font, clamped to [MIN_AGENT_COLS, the
// grid's own width]. Reporting this (rather than the grid width) is Option A:
// the server pins the shared PTY to the minimum across clients, so a narrower
// report makes the agent genuinely reflow narrower, letting the mirror render
// at a legible font instead of shrinking 120 cols into 5px. Capped at gridCols
// so we only ever narrow the PTY, never widen it past what the grid asks for;
// floored at MIN so a thin column doesn't strangle the agent.
export function readableCols(
  hostW: number,
  gridCols: number,
  ratios: CellRatios,
  targetFont: number
): number {
  const grid = gridCols > 0 ? gridCols : 80;
  if (hostW < 1 || targetFont < 1) return grid;
  const fit = Math.floor((hostW * 0.997) / (targetFont * ratios.w));
  return Math.min(grid, Math.max(MIN_AGENT_COLS, fit));
}

const FONT_FAMILY = "ui-monospace, SFMono-Regular, Menlo, monospace";

interface Props {
  session: string;
  // The active grid pane's xterm cols/rows. `cols` is the UPPER BOUND on how
  // wide we report (we never widen the PTY past the grid); `rows` is reported
  // as-is so the agent's height isn't perturbed.
  cols: number;
  rows: number;
  // Readable target font — the size we aim for when choosing the column count
  // at calibration. Splitter-drag font-zoom is allowed above this.
  maxFontSize: number;
  // Phase 2: forward keystrokes to the agent + hold keyboard focus. False = a
  // pure read-only monitor (keystrokes are swallowed, no focus stealing).
  control: boolean;
  // Bump this to force a re-fit of the column count to the current column width
  // (the editor's "double-click the splitter" gesture). Between bumps the count
  // stays locked and dragging only zooms the font.
  recalibrateSignal: number;
  onState: (s: ConnState) => void;
}

// AgentMirror — a live (read-only, or interactive under `control`) view of an
// agent session for the editor's right column.
//
// It attaches a second WebSocket and CALIBRATES ONCE: it picks a readable-narrow
// column count and REPORTS it, so the server's min-size pinning reflows the
// agent to fit the column legibly (Option A). The wrong-width attach snapshot is
// discarded with a reset() at the same moment, so the reflow isn't stacked on a
// wrapped frame. After that the count is LOCKED — dragging the splitter only
// scales the font (no further reflow), and an explicit recalibrate (double-click
// the splitter) re-fits the count to the current width. On unmount — closing the
// editor or switching session (keyed by session) — the socket drops and the
// server re-pins the PTY back up to the grid's full width.
export default function AgentMirror({
  session,
  cols,
  rows,
  maxFontSize,
  control,
  recalibrateSignal,
  onState,
}: Props) {
  const hostRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const wsRef = useRef<WebSocket | null>(null);
  const controlRef = useRef(control);
  controlRef.current = control;
  // Grid size + target font in a ref so the calibrate/zoom helpers read fresh
  // values without re-subscribing anything.
  const sizeRef = useRef({ cols, rows, maxFontSize });
  sizeRef.current = { cols, rows, maxFontSize };
  // The locked column count (0 until first calibration). Splitter drags zoom
  // the font at this fixed count rather than reflowing the agent.
  const lockedColsRef = useRef(0);

  // Font-zoom ceiling: comfortably above the readable target so dragging the
  // column wider visibly enlarges the text (height still caps it so the whole
  // frame stays visible).
  const zoomCeiling = () => Math.max(24, sizeRef.current.maxFontSize + 14);

  // Scale the font to the current column at the LOCKED column count. No resize,
  // no report, no reflow — this is what a splitter drag does.
  const zoomFont = () => {
    const term = termRef.current;
    const host = hostRef.current;
    if (!term || !host) return;
    const c = lockedColsRef.current || term.cols;
    const font = fitFontSize(c, term.rows, host.clientWidth, host.clientHeight, cellRatios(FONT_FAMILY), zoomCeiling());
    if (term.options.fontSize !== font) term.options.fontSize = font;
  };

  // Tell the server our width so it re-pins the PTY (min across clients).
  const sendResize = () => {
    const term = termRef.current;
    const ws = wsRef.current;
    if (!term || !ws || ws.readyState !== WebSocket.OPEN) return;
    ws.send(JSON.stringify({ t: "r", c: term.cols, r: term.rows }));
  };

  // Pick a readable-narrow grid for the current column and apply it. When that
  // changes the grid we reset() first — discarding the wrong-width frame (the
  // wide attach snapshot, or the pre-drag frame on a manual recalibrate) so the
  // agent's reflow-repaint lands on a clean screen instead of a wrapped one —
  // then resize + report so the server reflows the agent. Locks the count for
  // subsequent font-only zooming.
  const calibrate = () => {
    const term = termRef.current;
    const host = hostRef.current;
    if (!term || !host) return;
    const { cols: gridCols, rows: gridRows, maxFontSize: maxFont } = sizeRef.current;
    const targetCols = readableCols(host.clientWidth, gridCols, cellRatios(FONT_FAMILY), maxFont);
    const targetRows = gridRows > 0 ? gridRows : 24;
    if (targetCols !== term.cols || targetRows !== term.rows) {
      term.reset();
      term.resize(targetCols, targetRows);
      sendResize();
    }
    lockedColsRef.current = targetCols;
    zoomFont();
  };

  // Build the terminal once, at the grid's full width so the attach snapshot
  // renders correctly (calibrate narrows it a frame later).
  useEffect(() => {
    const host = hostRef.current!;
    const c = sizeRef.current.cols > 0 ? sizeRef.current.cols : 80;
    const r = sizeRef.current.rows > 0 ? sizeRef.current.rows : 24;
    const term = new Terminal({
      cursorBlink: false,
      fontFamily: FONT_FAMILY,
      fontSize: fitFontSize(c, r, host.clientWidth, host.clientHeight, cellRatios(FONT_FAMILY), zoomCeiling()),
      scrollback: 5000,
      allowProposedApi: true,
      // Same palette as the grid terminals (TerminalView) so the mirror is
      // visually identical to what you'd see in a pane.
      theme: {
        background: "#0f1720",
        foreground: "#d7dee8",
        cursor: "#38bdf8",
        selectionBackground: "rgba(56, 189, 248, 0.4)",
        selectionInactiveBackground: "rgba(56, 189, 248, 0.18)",
        black: "#0f1720",
        brightBlack: "#3b4759",
      },
    });
    term.loadAddon(new WebLinksAddon());
    term.open(host);
    term.resize(c, r);
    termRef.current = term;

    // Typing → stdin, but only while in control. Wired once and gated by the
    // ref so toggling control never tears the socket down. Read-only mode
    // swallows input even if a click focuses the terminal.
    const dataSub = term.onData((d) => {
      if (!controlRef.current) return;
      const ws = wsRef.current;
      if (ws && ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify({ t: "i", d }));
    });

    return () => {
      dataSub.dispose();
      term.dispose();
      termRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Connect (and reconnect) for the session's lifetime. Calibrate once per
  // connection, right after the first message (the attach snapshot) lands — so
  // the snapshot is on screen to be reset, and a reconnect (which re-snapshots
  // at the restored full width) re-narrows.
  useEffect(() => {
    let closedByUs = false;
    let retry: ReturnType<typeof setTimeout> | null = null;
    let backoff = 500;
    let pendingCalibrate = false;

    const connect = () => {
      onState("connecting");
      const ws = new WebSocket(wsURL(session));
      ws.binaryType = "arraybuffer";
      wsRef.current = ws;
      ws.onopen = () => {
        backoff = 500;
        onState("open");
        pendingCalibrate = true;
      };
      ws.onmessage = (e) => {
        const term = termRef.current;
        if (e.data instanceof ArrayBuffer) term?.write(new Uint8Array(e.data));
        else if (typeof e.data === "string") term?.write(e.data);
        if (pendingCalibrate) {
          pendingCalibrate = false;
          calibrate();
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
    connect();

    return () => {
      closedByUs = true;
      if (retry) clearTimeout(retry);
      wsRef.current?.close();
      wsRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [session]);

  // Explicit recalibrate (double-click the splitter): re-fit the locked count to
  // the current column width. Skips the initial mount.
  const firstSignal = useRef(true);
  useEffect(() => {
    if (firstSignal.current) {
      firstSignal.current = false;
      return;
    }
    calibrate();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [recalibrateSignal]);

  // Focus follows control: grab the keyboard when control engages, drop it when
  // it releases (so the editor's shortcuts resume).
  useEffect(() => {
    const term = termRef.current;
    if (!term) return;
    term.options.cursorBlink = control;
    if (control) term.focus();
    else term.blur();
  }, [control]);

  // Splitter drag / window resize → zoom the font at the locked count (no
  // reflow). Live (not debounced) so the text scales smoothly under the drag.
  useEffect(() => {
    const host = hostRef.current!;
    const ro = new ResizeObserver(() => zoomFont());
    ro.observe(host);
    return () => ro.disconnect();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Suppress the browser context menu inside the terminal (same as the grid).
  useEffect(() => {
    const host = hostRef.current!;
    const onCtx = (e: MouseEvent) => e.preventDefault();
    host.addEventListener("contextmenu", onCtx);
    return () => host.removeEventListener("contextmenu", onCtx);
  }, []);

  return (
    <div className="h-full w-full overflow-hidden bg-bar pl-1.5 pt-1">
      <div ref={hostRef} className="h-full w-full" />
    </div>
  );
}
