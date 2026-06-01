import { useMemo, useRef, useState, type ReactNode } from "react";
import type { Terminal } from "@xterm/xterm";
import TerminalView, { type ConnState } from "./TerminalView";
import type { Session } from "../api";
import { toolColor } from "../util";
import { FileEditIcon, PlusIcon } from "../icons";

export type Layout = 1 | 2 | 3 | 4 | 5 | 6;

// Per layout: a CSS grid template laid out with named areas, plus the area name
// each pane index occupies. Pane 0 spans both rows in the L-shapes (layouts 3
// and 6), which falls out of grid-template-areas for free — the same letter
// repeated across two rows stretches that area to span them.
//
// Pane-index convention: pane 0 is the "main" pane of the layout (the tall
// one in the L-shapes, top in the stack). Keeping that consistent matters
// because `setLayout` migrates the active pane's session into slot 0 when
// shrinking — see App.tsx.
const TEMPLATES: Record<
  Layout,
  { cols: string; rows: string; areas: string; pane: (i: number) => string }
> = {
  1: { cols: "1fr", rows: "1fr", areas: '"a"', pane: () => "a" },
  2: { cols: "1fr 1fr", rows: "1fr", areas: '"a b"', pane: (i) => "ab"[i]! },
  3: {
    cols: "1fr 1fr",
    rows: "1fr 1fr",
    // Left tall (a), right column split top (b) / bottom (c).
    areas: '"a b" "a c"',
    pane: (i) => "abc"[i]!,
  },
  4: {
    cols: "1fr 1fr",
    rows: "1fr 1fr",
    areas: '"a b" "c d"',
    pane: (i) => "abcd"[i]!,
  },
  5: {
    // Stacked: one column, two rows (top = pane 0, bottom = pane 1).
    cols: "1fr",
    rows: "1fr 1fr",
    areas: '"a" "b"',
    pane: (i) => "ab"[i]!,
  },
  6: {
    // Right-tall L (mirror of layout 3): right column tall (a),
    // left column split top (b) / bottom (c).
    cols: "1fr 1fr",
    rows: "1fr 1fr",
    areas: '"b a" "c a"',
    pane: (i) => "abc"[i]!,
  },
};

// How many panes a layout has. `panes[]` length must match this — App.tsx's
// loadPaneState / setLayout / setActive read this to keep the array sized
// correctly and to clamp the active index.
const PANE_COUNT: Record<Layout, number> = { 1: 1, 2: 2, 3: 3, 4: 4, 5: 2, 6: 3 };
export function paneCount(l: Layout): number {
  return PANE_COUNT[l];
}

interface Props {
  layout: Layout;
  panes: (string | null)[];
  active: number;
  sessions: Session[];
  fontSize: number;
  onActivate: (idx: number) => void;
  onConn: (idx: number, c: ConnState) => void;
  onPickFor: (idx: number, name: string) => void;
  onOpenDrawerFor: (idx: number) => void;
  onNewFor: (idx: number) => void;
  onOpenEditor: () => void; // opens the file-editor overlay (the single file
  // view: browse / view / edit / download). The active pane — which the
  // pointerdown above has just set — is the implicit target, so the tree roots
  // at this session.
  // Pane-indexed xterm registration — see TerminalView.onTerm. Lets the
  // app's global copy shortcut read the active pane's current selection.
  onTermFor?: (idx: number, term: Terminal | null) => void;
  // File drop on this pane (drag-and-drop upload). The pane must hold a
  // session to be a valid drop target — empty panes ignore the drop. The
  // DataTransfer is handed up; the parent flattens it (folders included)
  // and opens the UploadSheet targeting `panes[idx]`.
  onDropFiles?: (idx: number, dt: DataTransfer) => void;
  // Pane-scoped overlay (the session switcher on desktop): rendered as an
  // `absolute inset-0` child of pane `paneOverlayIdx`, so it covers exactly
  // that terminal box rather than the whole screen. Null = nothing to show.
  paneOverlay?: ReactNode;
  paneOverlayIdx?: number | null;
}

// Tile up to four <TerminalView>s in one of four fixed CSS-grid layouts.
// Each pane is independently attached (its own WebSocket + xterm), so panes
// can hold different sessions without cross-talk. Mounting the same session
// twice would make the two clients fight over tmux's single pane width, so
// the parent dedupes; this component just renders what it's given.
export default function TileGrid({
  layout,
  panes,
  active,
  sessions,
  fontSize,
  onActivate,
  onConn,
  onPickFor,
  onOpenDrawerFor,
  onNewFor,
  onOpenEditor,
  onTermFor,
  onDropFiles,
  paneOverlay,
  paneOverlayIdx,
}: Props) {
  const tpl = TEMPLATES[layout];

  return (
    <div
      className="grid h-full w-full gap-1 bg-edge/40"
      style={{
        gridTemplateColumns: tpl.cols,
        gridTemplateRows: tpl.rows,
        gridTemplateAreas: tpl.areas,
      }}
    >
      {panes.map((session, idx) => (
        <PaneBox
          key={idx}
          area={tpl.pane(idx)}
          index={idx}
          active={idx === active}
          session={session}
          sessions={sessions}
          fontSize={fontSize}
          onActivate={() => onActivate(idx)}
          onConn={(c) => onConn(idx, c)}
          onPick={(name) => onPickFor(idx, name)}
          onOpenDrawer={() => onOpenDrawerFor(idx)}
          onNew={() => onNewFor(idx)}
          onOpenEditor={onOpenEditor}
          onTerm={(t) => onTermFor?.(idx, t)}
          onDropFiles={onDropFiles ? (dt) => onDropFiles(idx, dt) : undefined}
          overlay={paneOverlayIdx === idx ? paneOverlay : null}
        />
      ))}
    </div>
  );
}

interface PaneProps {
  area: string;
  index: number;
  active: boolean;
  session: string | null;
  sessions: Session[];
  fontSize: number;
  onActivate: () => void;
  onConn: (c: ConnState) => void;
  onPick: (name: string) => void;
  onOpenDrawer: () => void;
  onNew: () => void;
  onOpenEditor: () => void;
  onTerm?: (term: Terminal | null) => void;
  onDropFiles?: (dt: DataTransfer) => void;
  overlay?: ReactNode;
}

function PaneBox({
  area,
  index,
  active,
  session,
  sessions,
  fontSize,
  onActivate,
  onConn,
  onPick,
  onOpenDrawer,
  onNew,
  onOpenEditor,
  onTerm,
  onDropFiles,
  overlay,
}: PaneProps) {
  const meta = sessions.find((s) => s.name === session);

  // Drag-and-drop overlay state. We track a counter (incremented on
  // dragenter, decremented on dragleave) because dragenter/leave also fire
  // for every child element the cursor crosses — naive boolean state
  // flickers as you move over the xterm canvas vs the padding wrapper.
  // The classic "drag enter/leave counter" trick keeps the overlay stable.
  // Only show the overlay when the drag actually carries files (so dragging
  // a selection from another tab doesn't paint a spurious target).
  const dragCounterRef = useRef(0);
  const [dragHover, setDragHover] = useState(false);
  const droppable = !!session && !!onDropFiles; // empty panes ignore drops
  const isFileDrag = (e: React.DragEvent) =>
    Array.from(e.dataTransfer.types || []).includes("Files");

  const onPaneDragEnter = (e: React.DragEvent) => {
    if (!droppable || !isFileDrag(e)) return;
    e.preventDefault();
    onActivate(); // mirror the click/pointerdown promotion path
    dragCounterRef.current++;
    if (dragCounterRef.current === 1) setDragHover(true);
  };
  const onPaneDragOver = (e: React.DragEvent) => {
    if (!droppable || !isFileDrag(e)) return;
    // preventDefault is what tells the browser this is a valid drop
    // target — without it `drop` never fires and the cursor stays at
    // "no entry".
    e.preventDefault();
    e.dataTransfer.dropEffect = "copy";
  };
  const onPaneDragLeave = (e: React.DragEvent) => {
    if (!droppable || !isFileDrag(e)) return;
    dragCounterRef.current = Math.max(0, dragCounterRef.current - 1);
    if (dragCounterRef.current === 0) setDragHover(false);
  };
  const onPaneDrop = (e: React.DragEvent) => {
    if (!droppable || !isFileDrag(e)) return;
    e.preventDefault();
    dragCounterRef.current = 0;
    setDragHover(false);
    onDropFiles!(e.dataTransfer);
  };

  // Per-pane chrome (the [pane-number / tool / name] chip + download icon)
  // auto-hides: visible while the mouse is moving over this pane, faded
  // out after ~2.5s of stillness, hidden on leave. Tracked with a ref so
  // mousemove doesn't trigger a setState on every frame — only the
  // transition hidden→visible does.
  const [chromeVisible, setChromeVisible] = useState(false);
  const chromeVisibleRef = useRef(false);
  const chromeTimerRef = useRef<number | null>(null);
  const chromeActivity = () => {
    if (chromeTimerRef.current != null) window.clearTimeout(chromeTimerRef.current);
    if (!chromeVisibleRef.current) {
      chromeVisibleRef.current = true;
      setChromeVisible(true);
    }
    chromeTimerRef.current = window.setTimeout(() => {
      chromeVisibleRef.current = false;
      setChromeVisible(false);
      chromeTimerRef.current = null;
    }, 2500);
  };
  const chromeLeave = () => {
    if (chromeTimerRef.current != null) {
      window.clearTimeout(chromeTimerRef.current);
      chromeTimerRef.current = null;
    }
    if (chromeVisibleRef.current) {
      chromeVisibleRef.current = false;
      setChromeVisible(false);
    }
  };

  return (
    <div
      // Capture-phase pointerdown so clicking inside the xterm canvas still
      // promotes this pane to active *before* xterm processes the click.
      onPointerDownCapture={onActivate}
      onMouseEnter={chromeActivity}
      onMouseMove={chromeActivity}
      onMouseLeave={chromeLeave}
      // Drag-and-drop file upload: handlers attached on the outer pane so
      // they cover the entire surface (including the xterm canvas inside).
      // xterm.js doesn't register its own drop handlers, so a bubble-phase
      // listener here is sufficient — no capture-phase juggling needed
      // (unlike the keydown paths in App.tsx). The visual drop overlay
      // lives below as a separate absolute div with pointer-events-none,
      // so it never steals these events.
      onDragEnter={onPaneDragEnter}
      onDragOver={onPaneDragOver}
      onDragLeave={onPaneDragLeave}
      onDrop={onPaneDrop}
      // Square corners (no rounded-*): rounding the pane boxes makes the
      // gap-area where panes meet look bubbly, which fights the
      // tiling-window-manager vibe of the layout. Floating chrome (chip,
      // download button) keeps its own subtle rounding.
      // Highlight border is rendered as a separate overlay div below, not
      // on this element — see the long comment there for why.
      className="relative flex min-h-0 min-w-0 flex-col overflow-hidden bg-bar"
      style={{ gridArea: area }}
    >
      {/* Auto-hide chrome cluster, top-right: pane chip + (if mounted)
          download button. Fades in on mouse activity inside the pane and
          out after ~2.5s of stillness; leaves immediately on mouseleave.
          One container, one fade, so they move together.
          The download button click goes through the surrounding PaneBox
          onPointerDownCapture first → currentSession reflects this pane
          by the time onClick fires → files sheet roots itself here
          automatically, no extra prop wiring. */}
      <div
        className={`absolute right-1.5 top-1 z-10 flex items-center gap-1.5 transition-opacity duration-200 ${
          chromeVisible ? "opacity-100" : "pointer-events-none opacity-0"
        }`}
      >
        <div className="pointer-events-none flex items-center gap-1.5 rounded bg-bar/70 px-1.5 py-0.5 text-[10px] backdrop-blur-sm">
          <span className="font-mono text-slate-500">{index + 1}</span>
          {meta && (
            <>
              <span
                className={`rounded px-1 py-px text-[9px] font-bold uppercase text-bar ${toolColor(
                  meta.tool
                )}`}
              >
                {meta.tool}
              </span>
              <span className="max-w-[10rem] truncate text-slate-300">{meta.short}</span>
            </>
          )}
        </div>
        {session && (
          <button
            onClick={onOpenEditor}
            aria-label="Open file browser / editor"
            title="Files — browse, view, edit, download"
            className="flex items-center justify-center rounded-md border border-edge bg-bar/70 p-1.5 text-accent backdrop-blur-sm hover:bg-bar hover:text-slate-100"
          >
            <FileEditIcon className="h-4 w-4" />
          </button>
        )}
      </div>

      {session ? (
        <TerminalView
          key={session}
          session={session}
          fontSize={fontSize}
          onState={onConn}
          active={active}
          onTerm={onTerm}
        />
      ) : (
        <EmptyPanePicker
          sessions={sessions}
          onPick={onPick}
          onOpenDrawer={onOpenDrawer}
          onNew={onNew}
        />
      )}

      {/* Highlight overlay — a separate borrowed div drawn on top of the
          terminal so the border is visible regardless of where the pane
          sits relative to the viewport.
          - `outline` would be drawn outside the box and get clipped at the
            viewport edge on the three sides where the pane is flush with
            it (we observed that — only the centre line between cols
            showed in the 2-col layout).
          - `ring-inset` / inset box-shadow is drawn inside the box, but
            it's painted *below* children — the xterm canvas fills 100% of
            the pane, so it covers the inset shadow and you see nothing.
          - A pointer-events-none overlay div with a `border` sits on top
            of the terminal (no click stealing) and the border is drawn
            inward from the pane's edges (box-sizing: border-box), so it's
            fully visible on every side and in every layout. */}
      <div
        aria-hidden
        className={`pointer-events-none absolute inset-0 z-10 ${
          active ? "border-2 border-accent" : "border border-edge/70"
        }`}
      />

      {/* Drop-target overlay. pointer-events-none keeps the drag events
          flowing to the outer PaneBox handlers; this layer is purely
          visual. z-20 so it floats above the highlight border and the
          xterm canvas. Renders only while a file drag is hovering THIS
          pane — guarded by `droppable` so empty panes never offer a
          drop target (the parent ignores `onDropFiles` without a
          session anyway, but visually announcing it would be a lie). */}
      {dragHover && droppable && (
        <div
          aria-hidden
          className="pointer-events-none absolute inset-0 z-20 flex flex-col items-center justify-center gap-2 bg-accent/15 backdrop-blur-[2px]"
        >
          <div className="rounded-xl border-2 border-dashed border-accent bg-bar/85 px-6 py-4 text-center shadow-lg">
            <div className="text-2xl">⬇︎</div>
            <div className="mt-1 text-sm font-semibold text-slate-100">
              Drop to upload
            </div>
            {meta && (
              <div className="mt-0.5 text-xs text-slate-400">
                into{" "}
                <span className={`rounded px-1 py-px text-[9px] font-bold uppercase text-bar ${toolColor(meta.tool)}`}>
                  {meta.tool}
                </span>{" "}
                <span className="font-mono text-slate-300">{meta.short}</span>
              </div>
            )}
          </div>
        </div>
      )}

      {/* Pane-scoped overlay (desktop session switcher). The drawer positions
          itself `absolute inset-0 z-30` so it covers this terminal box only —
          PaneBox is `relative overflow-hidden`, which clips it to the pane.
          Rendered last so it also wins DOM order over the highlight/drop
          layers. */}
      {overlay}
    </div>
  );
}

interface PickerProps {
  sessions: Session[];
  onPick: (name: string) => void;
  onOpenDrawer: () => void;
  onNew: () => void;
}

// Inline picker shown inside an empty pane. Lists existing sessions one-tap
// away, with shortcuts to the full drawer and the new-session panel — so
// mounting a session into a freshly-split pane is a single click without
// ever opening any sheet.
function EmptyPanePicker({ sessions, onPick, onOpenDrawer, onNew }: PickerProps) {
  // Most-recently-active first — what you probably want to bring up.
  const sorted = useMemo(
    () => [...sessions].sort((a, b) => b.activity - a.activity),
    [sessions]
  );

  return (
    <div className="flex h-full w-full flex-col items-stretch justify-center gap-2 p-6 pt-10">
      <div className="mb-2 text-center text-[10px] font-semibold uppercase tracking-[0.14em] text-slate-600">
        Empty pane — pick a session
      </div>

      <div className="mx-auto flex w-full max-w-sm flex-col gap-0.5 overflow-y-auto">
        <button
          onClick={onNew}
          className="flex items-center gap-2 rounded-md py-2 pl-2 pr-2 text-left text-[13px] text-slate-200 transition-colors hover:bg-edge/50"
        >
          <span className="flex h-5 w-5 shrink-0 items-center justify-center">
            <PlusIcon className="h-4 w-4 text-accent" />
          </span>
          <span className="font-medium">New session…</span>
        </button>

        {sorted.length === 0 && (
          <div className="rounded-md px-3 py-2 text-center text-[12px] text-slate-600">
            No sessions yet.
          </div>
        )}

        {sorted.map((s) => (
          <button
            key={s.name}
            onClick={() => onPick(s.name)}
            className="flex items-center gap-2 rounded-md py-1.5 pl-2 pr-2 text-left text-[13px] transition-colors hover:bg-edge/40"
            title={s.preview}
          >
            <span className="flex h-5 w-5 shrink-0 items-center justify-center">
              <span className={`h-2 w-2 rounded-full ${toolColor(s.tool)}`} title={s.tool} />
            </span>
            <span className="min-w-0 flex-1 truncate font-medium text-slate-100">{s.short}</span>
            {s.attached && (
              <span
                className="h-1.5 w-1.5 shrink-0 rounded-full bg-amber"
                title="already shown in another pane"
              />
            )}
          </button>
        ))}

        <button
          onClick={onOpenDrawer}
          className="mt-2 rounded-md px-3 py-2 text-center text-[11px] text-slate-600 transition-colors hover:text-slate-300"
        >
          Open the session switcher ⌃B
        </button>
      </div>
    </div>
  );
}
