import {
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type ReactNode,
} from "react";
import type { Shot } from "../assets";

/* ── window/terminal title bar — the app's traffic-light dots + a label ──────
   Dots tinted claude/amber/codex like the auth card (MultiTenant.tsx:75-80). */
export function TitleBar({ label }: { label: string }) {
  return (
    <div className="flex items-center gap-2 border-b border-line bg-surface/60 px-4 py-2.5">
      <span className="size-3 rounded-full bg-claude/80" />
      <span className="size-3 rounded-full bg-amber/80" />
      <span className="size-3 rounded-full bg-codex/80" />
      <span className="ml-1 truncate font-mono text-xs text-faint">{label}</span>
    </div>
  );
}

type Fit = "width" | "height";

/* A slim phone bezel. `fit="height"` makes it size to its parent's height (used
   in galleries to line every device up on a shared baseline); the default sizes
   to width (a single hero/feature shot capped by max-width). On the new `bg`
   the screenshot's own `#0f1720` reads as a glowing native window. */
export function Phone({
  shot,
  fit = "width",
  className = "",
  eager = false,
}: {
  shot: Shot;
  fit?: Fit;
  className?: string;
  eager?: boolean;
}) {
  return (
    <div
      className={`relative ${fit === "height" ? "h-full w-auto" : "w-full"} rounded-[26px] border border-line bg-surface p-[7px] shadow-[0_24px_64px_-32px_rgba(0,0,0,0.8)] transition-colors duration-150 hover:border-accent/40 ${className}`}
    >
      {/* speaker notch */}
      <span className="absolute left-1/2 top-3.5 z-[2] h-1 w-9 -translate-x-1/2 rounded-[3px] bg-line" />
      <img
        src={shot.src}
        width={shot.w}
        height={shot.h}
        alt={shot.alt}
        loading={eager ? "eager" : "lazy"}
        className={`block rounded-[20px] ${fit === "height" ? "h-full w-auto" : "w-full"}`}
      />
    </div>
  );
}

/* A windowed (browser/terminal) frame with a title bar. */
export function Window({
  shot,
  label,
  fit = "width",
  className = "",
}: {
  shot: Shot;
  label: string;
  fit?: Fit;
  className?: string;
}) {
  return (
    <div
      className={`flex flex-col overflow-hidden rounded-xl border border-line bg-surface shadow-[0_24px_64px_-32px_rgba(0,0,0,0.8)] transition-colors duration-150 hover:border-accent/40 ${fit === "height" ? "h-full w-auto" : "w-full"} ${className}`}
    >
      <TitleBar label={label} />
      <div
        className={`flex min-h-0 flex-1 justify-center ${fit === "height" ? "" : "w-full"}`}
      >
        <img
          src={shot.src}
          width={shot.w}
          height={shot.h}
          alt={shot.alt}
          loading="lazy"
          className={`block ${fit === "height" ? "h-full w-auto" : "w-full"}`}
        />
      </div>
    </div>
  );
}

/* A fixed-height stage that bottom-aligns whatever device sits in it, so a row
   of mixed phones/windows shares one baseline and the captions below line up. */
export function Stage({
  children,
  className = "",
}: {
  children: ReactNode;
  className?: string;
}) {
  return (
    <div className={`flex items-end justify-center ${className}`}>{children}</div>
  );
}

/* A figure with a device and a caption underneath, centered. */
export function Shot({
  children,
  caption,
}: {
  children: ReactNode;
  caption: string;
}) {
  return (
    <figure className="flex w-full flex-col items-center gap-3">
      {children}
      <figcaption className="text-center font-mono text-[0.76rem] text-faint">
        {caption}
      </figcaption>
    </figure>
  );
}

/* A terminal command block with a copy button. Shared by the 3-step SaaS flow
   (HowItWorks) and the self-host hand-off (Start). Boxed because it is a
   terminal — the one place a box is honest. */
export function Cmd({ clip, children }: { clip: string; children: ReactNode }) {
  return (
    <div className="relative mb-2.5 rounded-lg border border-line bg-surface px-3.5 py-2.5">
      <pre className="overflow-x-auto">
        <code className="font-mono text-[0.83rem] leading-[1.7] whitespace-pre">
          {children}
        </code>
      </pre>
      <CopyButton text={clip} />
    </div>
  );
}

/* The shell prompt glyph in front of a Cmd. */
export const Prompt = () => (
  <span className="mr-[0.6ch] select-none text-accent">$</span>
);

/* ── Eyebrow (A2) — the app's section-label idiom, with the brand `›` glyph ── */
export function Eyebrow({
  children,
  className = "",
}: {
  children: ReactNode;
  className?: string;
}) {
  return (
    <p
      className={`font-mono text-[11px] uppercase tracking-[0.25em] text-faint ${className}`}
    >
      <span className="text-accent">›</span> {children}
    </p>
  );
}

/* ── SectionHeading — the one expressive voice: a display grotesk, not mono ── */
export function SectionHeading({
  children,
  className = "",
}: {
  children: ReactNode;
  className?: string;
}) {
  return (
    <h2
      className={`font-display text-[clamp(1.5rem,3.2vw,2.1rem)] font-bold leading-[1.12] tracking-[-0.02em] text-ink ${className}`}
    >
      {children}
    </h2>
  );
}

/* ── ToolBadge (A/B2) — the app's uppercase tool pill in the tool's own hue ──
   frontend/src/App.tsx:2067 idiom, hues from frontend/src/util.ts:22-37. */
const TOOL_BG: Record<string, string> = {
  claude: "bg-claude",
  gemini: "bg-gemini",
  codex: "bg-codex",
  kimi: "bg-kimi",
  shell: "bg-shell",
};
export function ToolBadge({
  tool,
  className = "",
}: {
  tool: string;
  className?: string;
}) {
  return (
    <span
      className={`inline-block rounded px-1.5 py-0.5 text-[10px] font-bold uppercase leading-none text-surface ${TOOL_BG[tool] ?? "bg-line"} ${className}`}
    >
      {tool}
    </span>
  );
}

/* ── useReveal / Reveal (C3) — one scroll-reveal primitive for the whole page.
   The hidden state is applied by this hook (not default CSS), so no-JS,
   pre-hydration and reduced-motion readers always see content at rest. The CSS
   itself also lives inside prefers-reduced-motion:no-preference as a second
   guarantee. Per-child stagger is set via the `--stagger-i` custom property. */
export function useReveal<T extends HTMLElement = HTMLDivElement>() {
  const ref = useRef<T>(null);
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const reduced = window.matchMedia(
      "(prefers-reduced-motion: reduce)",
    ).matches;
    if (reduced || !("IntersectionObserver" in window)) return; // render at rest
    el.classList.add("reveal");
    const obs = new IntersectionObserver(
      (entries) => {
        for (const e of entries) {
          if (e.isIntersecting) {
            el.classList.add("is-in");
            obs.disconnect();
            return;
          }
        }
      },
      { threshold: 0.15 },
    );
    obs.observe(el);
    return () => obs.disconnect();
  }, []);
  return ref;
}

export function Reveal({
  i = 0,
  className = "",
  style,
  children,
}: {
  i?: number;
  className?: string;
  style?: CSSProperties;
  children: ReactNode;
}) {
  const ref = useReveal<HTMLDivElement>();
  return (
    <div
      ref={ref}
      className={className}
      style={{ ...style, "--stagger-i": i } as CSSProperties}
    >
      {children}
    </div>
  );
}

/* Copy-to-clipboard button used on the install commands. */
export function CopyButton({ text }: { text: string }) {
  const [label, setLabel] = useState("copy");
  const [done, setDone] = useState(false);

  async function copy() {
    try {
      await navigator.clipboard.writeText(text);
      setLabel("copied ✓");
      setDone(true);
    } catch {
      setLabel("copy failed");
      setDone(false);
    }
    setTimeout(() => {
      setLabel("copy");
      setDone(false);
    }, 1400);
  }

  return (
    <button
      type="button"
      onClick={copy}
      className={`absolute right-2 top-2 cursor-pointer rounded-md border px-2.5 py-1 font-mono text-[0.68rem] transition-colors ${
        done
          ? "border-accent text-accent"
          : "border-line bg-card text-dim hover:border-accent hover:text-accent"
      }`}
    >
      {label}
    </button>
  );
}
