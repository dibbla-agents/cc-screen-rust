import { APP } from "../urls";
import { Eyebrow } from "./ui";
import { HeroSim } from "./HeroSim";

export function Hero() {
  return (
    <section className="mx-auto max-w-[1080px] px-6 pb-20 pt-16 sm:pt-20">
      <div className="grid items-center gap-10 md:grid-cols-12 md:gap-12">
        {/* headline + copy + CTAs (left on ≥md, 7 cols) */}
        <div className="md:col-span-7">
          <Eyebrow>always-on AI coding agents · built in rust</Eyebrow>
          <h1 className="mt-5 max-w-[17ch] font-display text-[clamp(2.2rem,5.5vw,3.6rem)] font-bold leading-[1.05] tracking-[-0.02em] text-ink">
            Your coding agents, running{" "}
            <span className="text-accent">24/7</span> — and always a tap away.
          </h1>
          <p className="mt-5 max-w-[56ch] text-[1.08rem] text-dim">
            For developers who run Claude, Codex, Gemini or Kimi all day — and
            don't want to be at the desk they run on. See what your agents did,
            cowork on their files, keep the conversation going from wherever you
            are.
          </p>

          <div className="mt-9 flex flex-col gap-2.5 sm:flex-row sm:flex-wrap">
            <a
              href={APP}
              className="inline-flex min-h-11 w-full items-center justify-center rounded-lg bg-accent px-5 py-[0.68rem] text-center font-semibold text-[#0b111a] shadow-[0_8px_24px_-8px_rgba(56,189,248,0.5)] transition hover:-translate-y-px hover:brightness-110 sm:w-auto"
            >
              Sign up free — no card
            </a>
            <a
              href="#how"
              className="inline-flex min-h-11 w-full items-center justify-center rounded-lg border border-line px-5 py-[0.68rem] text-center font-mono text-[0.85rem] text-ink transition-colors duration-150 hover:border-accent hover:text-accent sm:w-auto"
            >
              How it works
            </a>
          </div>
          <p className="mt-3 font-mono text-[0.78rem] text-faint">
            No card. Your code stays on your machines — the hub only relays.{" "}
            <a
              className="underline transition-colors hover:text-accent"
              href="#self-host"
            >
              Prefer to self-host?
            </a>
          </p>
          <p className="mt-6 font-mono text-[0.78rem] text-faint">
            Anthropic · Google · OpenAI · Kimi — your pick, all at once.
          </p>
        </div>

        {/* the live product panel (below CTAs on mobile, right on ≥md, 5 cols) */}
        <div className="md:col-span-5">
          <HeroSim />
        </div>
      </div>
    </section>
  );
}
