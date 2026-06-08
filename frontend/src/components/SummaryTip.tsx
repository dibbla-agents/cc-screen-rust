// A summary tooltip/popover (proposal 0022). The session summaries are clamped in
// dense surfaces (switcher row, status table, toast); this surfaces the FULL text:
//   • desktop — on hover (a floating tooltip),
//   • touch   — on long-press (and a tap anywhere dismisses it).
//
// Rendered in a portal at <body> so it escapes the `truncate`/overflow-hidden of
// the row it sits in, and positioned (clamped) to the viewport. Wraps an inline
// trigger; when there's no text it renders the children untouched.

import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";

type Mode = "hover" | "press" | null;

const MAX_W = 360;
const LONG_PRESS_MS = 450;

export default function SummaryTip({
  text,
  children,
  className,
}: {
  text?: string;
  children: React.ReactNode;
  className?: string;
}) {
  const ref = useRef<HTMLSpanElement>(null);
  const lp = useRef<number | null>(null);
  // True between a fired long-press and the click it would otherwise produce, so
  // we can swallow that click (don't also mount/open the row under us).
  const pressed = useRef(false);
  const [mode, setMode] = useState<Mode>(null);
  const [rect, setRect] = useState<DOMRect | null>(null);

  const open = (m: Mode) => {
    const el = ref.current;
    if (!el) return;
    setRect(el.getBoundingClientRect());
    setMode(m);
  };
  const clearLp = () => {
    if (lp.current) {
      clearTimeout(lp.current);
      lp.current = null;
    }
  };
  const close = () => {
    setMode(null);
    clearLp();
  };
  useEffect(() => clearLp, []);

  if (!text) return <>{children}</>;

  const vw = typeof window !== "undefined" ? window.innerWidth : MAX_W;
  const w = Math.min(MAX_W, vw - 24);
  let style: React.CSSProperties = {};
  if (rect) {
    const cx = rect.left + rect.width / 2;
    const left = Math.max(12, Math.min(cx - w / 2, vw - w - 12));
    // Prefer above the trigger; flip below when there isn't room.
    const above = rect.top > 180;
    style = above
      ? { left, top: rect.top - 8, width: w, transform: "translateY(-100%)" }
      : { left, top: rect.bottom + 8, width: w };
  }

  return (
    <span
      ref={ref}
      className={className}
      onPointerEnter={(e) => {
        if (e.pointerType !== "touch") open("hover");
      }}
      onPointerLeave={(e) => {
        if (e.pointerType !== "touch") close();
      }}
      onPointerDown={(e) => {
        if (e.pointerType === "touch") {
          pressed.current = false;
          clearLp();
          lp.current = window.setTimeout(() => {
            pressed.current = true;
            open("press");
          }, LONG_PRESS_MS);
        }
      }}
      onPointerUp={(e) => {
        if (e.pointerType === "touch") clearLp();
      }}
      onPointerCancel={clearLp}
      onClickCapture={(e) => {
        if (pressed.current) {
          // A long-press just fired — eat the click so the row under us doesn't
          // also activate (e.g. mount the session).
          e.preventDefault();
          e.stopPropagation();
          pressed.current = false;
        }
      }}
    >
      {children}
      {mode &&
        rect &&
        createPortal(
          <>
            {/* On touch, a tap-away layer dismisses; on hover we leave clicks
                through (pointer-events-none) so the tooltip never steals focus. */}
            {mode === "press" && (
              <div className="fixed inset-0 z-[190]" onClick={close} onPointerDown={close} />
            )}
            <div
              role="tooltip"
              style={style}
              className={`fixed z-[200] whitespace-pre-wrap break-words rounded-lg border border-edge bg-panel px-3 py-2 text-xs leading-snug text-slate-100 shadow-xl ${
                mode === "hover" ? "pointer-events-none" : ""
              }`}
            >
              {text}
            </div>
          </>,
          document.body
        )}
    </span>
  );
}
