import { useEffect, useRef, type ReactNode } from "react";
import { shots } from "../assets";
import { Phone, Window, TitleBar, Eyebrow, SectionHeading, Reveal } from "./ui";
// The one real-UI clip in the Tour (0061 E2): the web-create-session capture,
// shared byte-for-byte with the docs embed (one manifest entry, N writes).
import clipWebm from "../assets/demo/web-create-session.webm?url";
import clipMp4 from "../assets/demo/web-create-session.mp4?url";
import clipPoster from "../assets/demo/web-create-session-poster.png";

function Beat({
  n,
  title,
  children,
  className = "",
}: {
  n: string;
  title: string;
  children: ReactNode;
  className?: string;
}) {
  return (
    <div className={className}>
      <span className="mb-2 block font-mono text-[0.8rem] text-accent">{n}</span>
      <h3 className="mb-1.5 text-[1.15rem] font-semibold text-ink">{title}</h3>
      <p className="max-w-[46ch] text-[0.97rem] text-dim">{children}</p>
    </div>
  );
}

/* The Tour's one motion slot (0061 E2): the real create-session capture in the
   same window chrome as the stills. `preload="none"` + poster means zero video
   bytes until playback; an IntersectionObserver starts/pauses it with the
   viewport. Under prefers-reduced-motion the observer is never wired at all —
   the poster is the whole experience (same contract as useReveal above). */
function ClipWindow({ label }: { label: string }) {
  const videoRef = useRef<HTMLVideoElement>(null);
  useEffect(() => {
    const el = videoRef.current;
    if (!el) return;
    const reduced = window.matchMedia(
      "(prefers-reduced-motion: reduce)",
    ).matches;
    if (reduced || !("IntersectionObserver" in window)) return; // poster only
    const obs = new IntersectionObserver(
      (entries) => {
        for (const e of entries) {
          if (e.isIntersecting) void el.play().catch(() => {});
          else el.pause();
        }
      },
      { threshold: 0.35 },
    );
    obs.observe(el);
    return () => obs.disconnect();
  }, []);
  return (
    <div className="flex w-full flex-col overflow-hidden rounded-xl border border-line bg-surface shadow-[0_24px_64px_-32px_rgba(0,0,0,0.8)] transition-colors duration-150 hover:border-accent/40">
      <TitleBar label={label} />
      <video
        ref={videoRef}
        muted
        loop
        playsInline
        preload="none"
        poster={clipPoster}
        width={1280}
        height={720}
        className="block h-auto w-full"
        aria-label="Creating a session: new session, directory search, Claude starting"
      >
        <source src={clipWebm} type="video/webm" />
        <source src={clipMp4} type="video/mp4" />
      </video>
    </div>
  );
}

function Code({ children }: { children: ReactNode }) {
  return (
    <code className="rounded bg-card px-[0.35em] py-[0.05em] font-mono text-[0.86em] text-accent">
      {children}
    </code>
  );
}

export function Tour() {
  return (
    <div className="border-y border-line-soft bg-[rgba(15,23,32,0.4)]">
      <section id="tour" className="mx-auto max-w-[1080px] px-6 py-24">
        <Eyebrow>the product</Eyebrow>
        <SectionHeading className="mt-3 max-w-[22ch]">
          A whole team of agents, wherever you are.
        </SectionHeading>

        <div className="mt-14 flex flex-col gap-20">
          {/* 1 — copy left, phone right */}
          <Reveal className="grid items-center gap-9 md:grid-cols-2">
            <Beat n="01" title="A whole team, always on">
              Run as many agents as you like, side by side, around the clock —
              Anthropic, Google, OpenAI, Kimi, you name it. They keep working
              whether you're watching or not.
            </Beat>
            <div className="flex justify-center md:justify-end">
              <Phone shot={shots.mobileSessions} className="max-w-[236px]" eager />
            </div>
          </Reveal>

          {/* 2 — phone left, copy right */}
          <Reveal className="grid items-center gap-9 md:grid-cols-2">
            <div className="flex justify-center md:order-2 md:justify-start">
              <Phone shot={shots.mobileAgent} className="max-w-[236px]" />
            </div>
            <Beat n="02" title="Check in from anywhere" className="md:order-1">
              Open them on your phone or your laptop and jump straight into any
              one of them. Watching TV or heading to work — your agents are a tap
              away.
            </Beat>
          </Reveal>

          {/* 3 — full-width cowork window (the best screenshot) */}
          <Reveal className="flex flex-col gap-6">
            <Beat n="03" title="Cowork on the files">
              Browse, view, edit, and move files in and out — everything your
              agents are working on. Tree, file and agent side by side. It's a
              real working session, not just a chat box.
            </Beat>
            <Window shot={shots.webCowork} label="cc-screen — file viewer" />
          </Reveal>

          {/* 4 — clients: a native terminal window + a phone, one row */}
          <Reveal className="flex flex-col gap-6">
            <Beat n="04" title="Phone, browser, or a native terminal">
              Two clients, one wire. The web app is served by the hub — add it to
              your home screen — and <Code>ccs</Code> is a native terminal
              client. Same agents, whichever you reach for.
            </Beat>
            <div className="grid items-end gap-6 sm:grid-cols-[1.6fr_1fr]">
              <figure className="flex flex-col gap-3">
                <Window shot={shots.tuiGrid} label="ccs — grid" />
                <figcaption className="max-w-[52ch] text-[0.9rem] text-dim">
                  <Code>ccs</Code> tiles every running agent into a multi-pane
                  grid — six layouts and one-key focus, all under a tmux-style{" "}
                  <Code>Ctrl-A</Code> prefix.
                </figcaption>
              </figure>
              <figure className="flex flex-col gap-3">
                <Phone shot={shots.mobileFiles} className="mx-auto max-w-[220px]" />
                <figcaption className="text-[0.9rem] text-dim">
                  The web app installs to your home screen: browse the tree, read
                  or edit any file, start a fresh agent in any folder.
                </figcaption>
              </figure>
            </div>
          </Reveal>

          {/* 5 — the real UI moving: one clip, shared with the docs (0061 E2) */}
          <Reveal className="flex flex-col gap-6">
            <Beat n="05" title="Zero to a working agent">
              The whole flow, uncut: new session, find the project folder with
              the directory search, and Claude is running — under ten
              seconds, real UI, no speed-ups.
            </Beat>
            <ClipWindow label="cc-screen — new session" />
          </Reveal>
        </div>
      </section>
    </div>
  );
}
