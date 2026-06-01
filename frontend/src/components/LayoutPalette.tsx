import { Fragment, useEffect, useRef, useState } from "react";
import type { Layout } from "./TileGrid";

interface Props {
  current: Layout;
  onPick: (l: Layout) => void;
  onClose: () => void;
}

interface LayoutOption {
  layout: Layout;
  label: string;
  cols: 1 | 2; // for the visible 1-col / 2-col separator only
  draw: React.ReactNode;
}

// Canonical visual order: 1-col group first (single, stacked), then 2-col
// group (side-by-side, the two L-shapes, quad). Within each group, ordered
// from "no extra rows" to "more rows" so the eye reads complexity left to
// right. The visible gap between groups reinforces the "single column vs
// two columns" first decision the user makes when picking a layout.
//
// Position 1..N here is also the digit shortcut (`1` selects the leftmost,
// `6` the rightmost) — keep the order stable.
const ORDER: LayoutOption[] = [
  { layout: 1, label: "Single", cols: 1, draw: null },
  {
    layout: 5,
    label: "Stacked",
    cols: 1,
    draw: <line x1="1" y1="8" x2="15" y2="8" stroke="currentColor" strokeWidth="1.2" />,
  },
  {
    layout: 2,
    label: "Side-by-side",
    cols: 2,
    draw: <line x1="8" y1="1" x2="8" y2="15" stroke="currentColor" strokeWidth="1.2" />,
  },
  {
    layout: 3,
    label: "Left-tall L",
    cols: 2,
    draw: (
      <>
        <line x1="8" y1="1" x2="8" y2="15" stroke="currentColor" strokeWidth="1.2" />
        <line x1="8" y1="8" x2="15" y2="8" stroke="currentColor" strokeWidth="1.2" />
      </>
    ),
  },
  {
    layout: 6,
    label: "Right-tall L",
    cols: 2,
    draw: (
      <>
        <line x1="8" y1="1" x2="8" y2="15" stroke="currentColor" strokeWidth="1.2" />
        <line x1="1" y1="8" x2="8" y2="8" stroke="currentColor" strokeWidth="1.2" />
      </>
    ),
  },
  {
    layout: 4,
    label: "Quad",
    cols: 2,
    draw: (
      <>
        <line x1="8" y1="1" x2="8" y2="15" stroke="currentColor" strokeWidth="1.2" />
        <line x1="1" y1="8" x2="15" y2="8" stroke="currentColor" strokeWidth="1.2" />
      </>
    ),
  },
];

// LAYOUT_GLYPHS is re-exported so the header trigger (LayoutPicker) can show
// the same icon for the active layout without duplicating the SVG data.
export const LAYOUT_GLYPHS: Record<Layout, React.ReactNode> = Object.fromEntries(
  ORDER.map((o) => [o.layout, o.draw])
) as Record<Layout, React.ReactNode>;

// Floating popover anchored under the layout trigger button. Opens with the
// current layout pre-highlighted so Enter is a no-op (cheap back-out). ← →
// move the highlight (wraps), Enter applies and closes, Esc closes without
// changing, digits 1-6 jump-select. Mouse clicks apply directly.
//
// Closes on outside click via a capture-phase pointerdown listener — same
// pattern as the global paste / clipboard hooks, so the popover doesn't
// linger after focus moves elsewhere.
export default function LayoutPalette({ current, onPick, onClose }: Props) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const [highlight, setHighlight] = useState(() => {
    const i = ORDER.findIndex((o) => o.layout === current);
    return i >= 0 ? i : 0;
  });

  // Focus the container so its onKeyDown receives arrows/Enter/Esc without
  // the user needing to click. tabIndex=-1 makes the div focusable
  // programmatically but keeps it out of the tab order.
  useEffect(() => {
    containerRef.current?.focus();
  }, []);

  // Capture-phase pointerdown for outside-click close. We exempt the
  // trigger button (any element marked `data-layout-trigger`) so the
  // parent can wire it as a real toggle — without this, clicking the
  // button while the palette is open would race: our listener would
  // close first, then the button's onClick would immediately re-open.
  useEffect(() => {
    const onPointer = (e: PointerEvent) => {
      const el = containerRef.current;
      if (!el) return;
      const target = e.target as HTMLElement | null;
      if (el.contains(target as Node)) return;
      if (target?.closest("[data-layout-trigger]")) return;
      onClose();
    };
    window.addEventListener("pointerdown", onPointer, { capture: true });
    return () => window.removeEventListener("pointerdown", onPointer, { capture: true });
  }, [onClose]);

  const apply = (i: number) => {
    onPick(ORDER[i]!.layout);
    onClose();
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    // stopPropagation on every handled key so the parent chord handler
    // (capture-phase on window) — already gated on paletteOpen via a ref —
    // can't accidentally re-arm a prefix from inside the palette.
    if (e.key === "ArrowLeft") {
      e.preventDefault();
      setHighlight((h) => (h - 1 + ORDER.length) % ORDER.length);
    } else if (e.key === "ArrowRight") {
      e.preventDefault();
      setHighlight((h) => (h + 1) % ORDER.length);
    } else if (e.key === "Enter") {
      e.preventDefault();
      apply(highlight);
    } else if (e.key === "Escape") {
      e.preventDefault();
      onClose();
    } else if (e.key >= "1" && e.key <= String(ORDER.length)) {
      e.preventDefault();
      apply(parseInt(e.key, 10) - 1);
    }
  };

  return (
    <div
      ref={containerRef}
      role="dialog"
      aria-label="Pick a layout"
      tabIndex={-1}
      onKeyDown={onKeyDown}
      className="absolute right-0 top-full z-40 mt-2 rounded-lg border border-edge bg-bar shadow-xl outline-none"
    >
      <div className="flex items-center justify-between gap-6 border-b border-edge/60 px-3 py-1.5 text-[10px] uppercase tracking-wider text-slate-500">
        <span>Layout</span>
        <span className="font-mono normal-case tracking-normal">← → · ⏎ · Esc</span>
      </div>
      <div className="flex items-center gap-1 p-2">
        {ORDER.map((opt, i) => {
          // Visible gap between the 1-col and 2-col groups.
          const separator = i > 0 && opt.cols === 2 && ORDER[i - 1]!.cols === 1;
          const on = i === highlight;
          return (
            <Fragment key={opt.layout}>
              {separator && <div className="mx-1 h-10 w-px bg-edge/60" />}
              <button
                onClick={() => apply(i)}
                onMouseEnter={() => setHighlight(i)}
                title={`${opt.label}  (${i + 1})`}
                aria-label={opt.label}
                className={`flex h-12 w-12 items-center justify-center rounded transition-colors ${
                  on
                    ? "bg-accent/15 text-accent ring-2 ring-accent"
                    : "text-slate-400 hover:bg-panel hover:text-slate-200"
                }`}
              >
                <svg viewBox="0 0 16 16" className="h-7 w-7" aria-hidden>
                  <rect
                    x="1"
                    y="1"
                    width="14"
                    height="14"
                    rx="1.5"
                    fill="none"
                    stroke="currentColor"
                    strokeWidth="1.2"
                  />
                  {opt.draw}
                </svg>
              </button>
            </Fragment>
          );
        })}
      </div>
    </div>
  );
}
