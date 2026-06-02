import { useState, type ReactNode } from "react";
import type { Shot } from "../assets";

/* ── window/terminal title bar — the three traffic-light dots + a label ─────── */
export function TitleBar({ label }: { label: string }) {
  return (
    <div className="flex items-center gap-[7px] border-b border-line-soft px-3.5 py-2.5">
      <i className="size-2.5 rounded-full bg-green" />
      <i className="size-2.5 rounded-full bg-[#1c2a20]" />
      <i className="size-2.5 rounded-full bg-[#1c2a20]" />
      <span className="ml-1 truncate font-mono text-[0.73rem] text-faint">
        {label}
      </span>
    </div>
  );
}

type Fit = "width" | "height";

/* A slim phone bezel. `fit="height"` makes it size to its parent's height (used
   in galleries to line every device up on a shared baseline); the default sizes
   to width (a single hero/feature shot capped by max-width). */
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
      className={`relative ${fit === "height" ? "h-full w-auto" : "w-full"} rounded-[26px] border border-line bg-surface p-[7px] shadow-[0_18px_48px_-22px_rgba(0,0,0,0.85),inset_0_0_0_1px_rgba(118,179,96,0.05)] ${className}`}
    >
      {/* speaker notch */}
      <span className="absolute left-1/2 top-3.5 z-[2] h-1 w-9 -translate-x-1/2 rounded-[3px] bg-[rgba(118,179,96,0.22)]" />
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
      className={`flex flex-col overflow-hidden rounded-[10px] border border-line bg-surface shadow-[0_18px_48px_-24px_rgba(0,0,0,0.85)] ${fit === "height" ? "h-full w-auto" : "w-full"} ${className}`}
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
          ? "border-green-light text-green-light"
          : "border-line bg-[rgba(118,179,96,0.06)] text-dim hover:border-green hover:text-green-soft"
      }`}
    >
      {label}
    </button>
  );
}
