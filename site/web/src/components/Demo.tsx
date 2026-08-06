import { useEffect, useRef, useState } from "react";
import { TitleBar, Eyebrow, SectionHeading } from "./ui";
// Vite inlines the asciinema v2 cast as a string (?raw). The recording replays
// the installer's real output lines — enroll.rs / install-machine.sh strings —
// with the curl/download chatter simplified. A few KB of text, no player dep.
import castRaw from "../assets/demo/install.cast?raw";
// The Windows leg (0061 E3): the PowerShell one-liner recorded on the real
// Windows bench (harebell), behind a platform toggle below.
import castWindowsRaw from "../assets/demo/install-windows.cast?raw";

type CastEvent = { t: number; data: string };

/* Parse an asciinema v2 file: a JSON header line, then [time, "o", data]
   JSON-lines. Non-"o" events are ignored. */
function parseCast(raw: string): CastEvent[] {
  const events: CastEvent[] = [];
  for (const line of raw.split("\n")) {
    const s = line.trim();
    if (!s.startsWith("[")) continue; // header or blank
    try {
      const ev = JSON.parse(s) as [number, string, string];
      if (ev[1] === "o") events.push({ t: ev[0], data: ev[2] });
    } catch {
      /* skip malformed lines */
    }
  }
  return events;
}

/* The two platforms the toggle offers. Each poster line is the cast's own
   opening command, so the resting state shows exactly what the recording ran. */
const PLATFORMS = {
  unix: {
    label: "macOS / Linux",
    cast: castRaw,
    posterLine:
      "$ curl -fsSL https://app.ccscreen.dev/install.sh | sh -s -- my-laptop --assistants",
    title: "add a machine — 15s",
    watch: "watch the install (15s)",
  },
  windows: {
    label: "Windows",
    cast: castWindowsRaw,
    posterLine:
      'PS C:\\Users\\erik> irm "https://app.ccscreen.dev/install.ps1?name=harebell&assistants=all" | iex',
    title: "add a machine (Windows) — 10s",
    watch: "watch the install (10s)",
  },
} as const;
type PlatformId = keyof typeof PLATFORMS;

/* A ~100-line hand-rolled cast player: replay the "o" events into a <pre> on a
   requestAnimationFrame clock. No ANSI handling needed — the committed casts
   are plain text. Honors prefers-reduced-motion by jumping straight to the
   final frame instead of animating. */
function Player({ platform }: { platform: PlatformId }) {
  const { cast, posterLine, title, watch } = PLATFORMS[platform];
  const [text, setText] = useState<string | null>(null); // null = poster
  const [done, setDone] = useState(false);
  const scrollRef = useRef<HTMLDivElement>(null);
  const rafRef = useRef(0);

  // keep the newest output in view while playing
  useEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [text]);

  useEffect(() => () => cancelAnimationFrame(rafRef.current), []);

  function play() {
    const events = parseCast(cast);
    if (events.length === 0) return;
    setDone(false);
    const reduced = window.matchMedia(
      "(prefers-reduced-motion: reduce)",
    ).matches;
    if (reduced) {
      // user asked for no motion: show the finished transcript instantly
      setText(events.map((e) => e.data).join(""));
      setDone(true);
      return;
    }
    const start = performance.now();
    let next = 0;
    const tick = (now: number) => {
      const elapsed = (now - start) / 1000;
      let appended = "";
      while (next < events.length && events[next].t <= elapsed) {
        appended += events[next].data;
        next++;
      }
      if (appended) setText((t) => (t ?? "") + appended);
      if (next < events.length) {
        rafRef.current = requestAnimationFrame(tick);
      } else {
        setDone(true);
      }
    };
    setText("");
    rafRef.current = requestAnimationFrame(tick);
  }

  return (
    <div className="relative flex h-full flex-col">
      <TitleBar label={title} />
      <div
        ref={scrollRef}
        className="min-h-0 flex-1 overflow-auto px-4 py-3"
      >
        <pre className="font-mono text-[0.72rem] leading-[1.6] text-dim sm:text-[0.8rem]">
          <code className="whitespace-pre">{text ?? posterLine}</code>
        </pre>
      </div>
      {text === null && (
        <button
          type="button"
          onClick={play}
          className="absolute inset-x-0 bottom-0 top-[42px] flex cursor-pointer flex-col items-center justify-center gap-2 bg-[rgba(11,17,26,0.55)] transition-colors hover:bg-[rgba(11,17,26,0.4)]"
        >
          <span className="flex size-14 items-center justify-center rounded-full border border-accent bg-[rgba(11,17,26,0.85)] pl-1 text-[1.2rem] text-accent">
            ▶
          </span>
          <span className="font-mono text-[0.78rem] text-accent-soft">
            {watch}
          </span>
        </button>
      )}
      {done && (
        // small corner control so the finished transcript stays readable
        <button
          type="button"
          onClick={play}
          className="absolute bottom-2.5 right-2.5 cursor-pointer rounded-md border border-line bg-[rgba(11,17,26,0.85)] px-3 py-1.5 font-mono text-[0.72rem] text-dim transition-colors hover:border-accent hover:text-accent"
        >
          ↻ replay
        </button>
      )}
    </div>
  );
}

export function Demo() {
  // Lazy-mount the player on scroll-into-view so it costs nothing at first
  // paint; the fixed-aspect frame below is reserved either way (no CLS).
  const boxRef = useRef<HTMLDivElement>(null);
  const [visible, setVisible] = useState(false);
  // 0061 E3: which cast the player shows. Keying the Player on this resets it
  // to the poster state on switch — no auto-play, no mid-replay carryover.
  const [platform, setPlatform] = useState<PlatformId>("unix");

  useEffect(() => {
    const el = boxRef.current;
    if (!el || !("IntersectionObserver" in window)) {
      setVisible(true);
      return;
    }
    const obs = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) {
          setVisible(true);
          obs.disconnect();
        }
      },
      { rootMargin: "200px" },
    );
    obs.observe(el);
    return () => obs.disconnect();
  }, []);

  return (
    <section id="demo" className="mx-auto max-w-[1080px] px-6 py-24">
      <Eyebrow>demo</Eyebrow>
      <SectionHeading className="mt-3 max-w-[20ch]">
        Watch a machine come online.
      </SectionHeading>
      <p className="mt-4 max-w-[62ch] text-[1.02rem] text-dim">
        The one-liner from your dashboard, end to end: install, check the coding
        assistants, print the code, approve — connected.
      </p>
      <div className="mt-8 flex gap-2" role="tablist" aria-label="Platform">
        {(Object.keys(PLATFORMS) as PlatformId[]).map((p) => (
          <button
            key={p}
            type="button"
            role="tab"
            aria-selected={platform === p}
            onClick={() => setPlatform(p)}
            className={`cursor-pointer rounded-md border px-3 py-1.5 font-mono text-[0.78rem] transition-colors ${
              platform === p
                ? "border-accent bg-[rgba(11,17,26,0.85)] text-accent"
                : "border-line text-dim hover:border-accent hover:text-accent"
            }`}
          >
            {PLATFORMS[p].label}
          </button>
        ))}
      </div>
      <div
        ref={boxRef}
        className="mt-3 aspect-[16/9] w-full max-w-[900px] overflow-hidden rounded-xl border border-line bg-surface sm:aspect-[2/1]"
      >
        {visible && <Player key={platform} platform={platform} />}
      </div>
    </section>
  );
}
