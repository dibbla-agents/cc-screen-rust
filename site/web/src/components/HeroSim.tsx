import { useEffect, useRef, useState } from "react";
import { TitleBar, ToolBadge } from "./ui";

/* ── HeroSim (Part C1) — a living miniature of the app's session list ─────────
   A ~25s deterministic loop driven by a tick counter: activity text types out
   char-by-char, one session flips to "✓ ready for input" with the app's pulse
   halo, another resumes, and relative times tick. The panel only advances while
   on-screen (IntersectionObserver) AND while the tab is visible — no background
   CPU. Under prefers-reduced-motion it renders a single static mid-loop frame
   (one session ready, one typing frozen) with the same information, no movement.
   The script is data; copy tweaks don't touch the machine. */

type Row = {
  tool: string;
  name: string; // the session name shown in ink (its folder)
  act: string; // full activity text
  shown: string; // visible activity (grows while typing)
  typing: boolean; // caret + partial reveal
  ready: boolean; // "✓ ready for input", pulsing live dot
  secs: number; // relative time, in seconds
};

const TICK_MS = 120;
const LOOP = 210; // ticks ≈ 25.2s

function seed(): Row[] {
  return [
    { tool: "claude", name: "~/app", act: "building the login flow", shown: "building the login flow", typing: false, ready: false, secs: 131 },
    { tool: "gemini", name: "~/api", act: "refactoring the endpoints", shown: "refactoring the endpoints", typing: false, ready: false, secs: 308 },
    { tool: "codex", name: "~/web", act: "writing the tests", shown: "writing the tests", typing: false, ready: false, secs: 54 },
    { tool: "kimi", name: "~/docs", act: "tidying the README", shown: "", typing: true, ready: false, secs: 9 },
  ];
}

/* The reduced-motion still: codex ready, kimi frozen mid-word (its caret is
   present but the CSS disables its blink under reduced motion). */
function still(): Row[] {
  const r = seed();
  r[2] = { ...r[2], ready: true, act: "✓ ready for input", shown: "✓ ready for input" };
  r[3] = { ...r[3], shown: "tidying the READ" };
  return r;
}

// Scripted events, keyed by tick. Each mutates the working row array in place.
function applyEvents(rows: Row[], step: number) {
  switch (step) {
    case 26: // kimi finishes typing
      rows[3].typing = false;
      break;
    case 44: // codex flips to ready (pulse halo, live green)
      rows[2] = { ...rows[2], ready: true, typing: false, act: "✓ ready for input", shown: "✓ ready for input" };
      break;
    case 60: // times tick
      rows.forEach((r) => (r.secs += 48));
      break;
    case 96: // codex resumes work
      rows[2] = { ...rows[2], ready: false, typing: false, act: "running the test suite", shown: "running the test suite", secs: 1 };
      break;
    case 110: // claude starts typing a new activity
      rows[0] = { ...rows[0], typing: true, act: "wiring up the session store", shown: "" };
      break;
    case 134: // claude finishes typing
      rows[0].typing = false;
      break;
    case 150: // kimi flips to ready
      rows[3] = { ...rows[3], ready: true, typing: false, act: "✓ ready for input", shown: "✓ ready for input" };
      break;
    case 168: // times tick
      rows.forEach((r) => (r.secs += 63));
      break;
    default:
      break;
  }
}

function advanceTyping(rows: Row[]) {
  for (const r of rows) {
    if (r.typing && r.shown.length < r.act.length) {
      r.shown = r.act.slice(0, r.shown.length + 1);
    }
  }
}

function rel(secs: number): string {
  if (secs < 5) return "just now";
  if (secs < 60) return `${secs}s`;
  return `${Math.floor(secs / 60)}m`;
}

function SessionRow({ r }: { r: Row }) {
  return (
    <li className="grid min-h-[2.15rem] grid-cols-[7px_auto_1fr_auto] items-center gap-x-2.5">
      <span
        className={`size-[7px] rounded-full bg-live ${r.ready ? "live-dot" : "opacity-40"}`}
      />
      <ToolBadge tool={r.tool} />
      <span className="flex min-w-0 items-baseline gap-2">
        <b className="shrink-0 font-semibold text-ink">{r.name}</b>
        <span className={`truncate ${r.ready ? "text-live" : "text-dim"}`}>
          {r.shown}
          {r.typing && (
            <span className="caret ml-[2px] inline-block h-[1em] w-[7px] -translate-y-px bg-accent align-middle" />
          )}
        </span>
      </span>
      <time className="shrink-0 tabular-nums text-[0.72rem] text-faint">
        {rel(r.secs)}
      </time>
    </li>
  );
}

export function HeroSim() {
  const reduced =
    typeof window !== "undefined" &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches;

  const [rows, setRows] = useState<Row[]>(reduced ? still : seed);
  const hostRef = useRef<HTMLDivElement>(null);
  const stepRef = useRef(0);

  useEffect(() => {
    if (reduced) return; // static still; no machine
    const host = hostRef.current;
    let onScreen = true;

    function tick() {
      if (document.visibilityState !== "visible" || !onScreen) return;
      const step = stepRef.current;
      setRows((prev) => {
        if (step === 0) return seed();
        const next = prev.map((r) => ({ ...r }));
        applyEvents(next, step);
        advanceTyping(next);
        return next;
      });
      stepRef.current = step + 1 >= LOOP ? 0 : step + 1;
    }

    const id = window.setInterval(tick, TICK_MS);

    let obs: IntersectionObserver | undefined;
    if (host && "IntersectionObserver" in window) {
      obs = new IntersectionObserver(
        (entries) => {
          for (const e of entries) onScreen = e.isIntersecting;
        },
        { threshold: 0.3 },
      );
      obs.observe(host);
    }

    return () => {
      window.clearInterval(id);
      obs?.disconnect();
    };
  }, [reduced]);

  return (
    <div
      ref={hostRef}
      className="relative overflow-hidden rounded-2xl border border-line bg-card shadow-2xl shadow-black/40"
    >
      <div className="hero-scanline" />
      <TitleBar label="cc·screen — 4 agents" />
      <ul className="relative list-none px-4 pb-4 pt-3 font-mono text-[0.8rem]">
        {rows.map((r) => (
          <SessionRow key={r.tool} r={r} />
        ))}
      </ul>
    </div>
  );
}
