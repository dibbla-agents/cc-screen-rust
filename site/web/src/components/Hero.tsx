import { GITHUB } from "./Nav";
import { TitleBar } from "./ui";

type Session = {
  name: string;
  repo: string;
  act: string;
  typing?: boolean;
};

const SESSIONS: Session[] = [
  { name: "claude", repo: "~/app", act: "building the login flow" },
  { name: "gemini", repo: "~/api", act: "refactoring the endpoints" },
  { name: "codex", repo: "~/web", act: "writing the tests" },
  { name: "kimi", repo: "~/docs", act: "tidying the README", typing: true },
];

function SessionRow({ name, repo, act, typing }: Session) {
  return (
    <li className="grid grid-cols-[1.2ch_7ch_auto_1fr] items-center gap-[0.8ch] py-[0.34rem] text-dim">
      <span className="live-dot size-[7px] rounded-full bg-green" />
      <b className="font-bold text-ink">{name}</b>
      <span className="text-green-soft">{repo}</span>
      <span className="truncate">
        {act}
        {typing && (
          <span className="caret ml-[3px] inline-block h-[1em] w-[7px] -translate-y-px bg-green align-middle" />
        )}
      </span>
    </li>
  );
}

export function Hero() {
  return (
    <section className="mx-auto max-w-[820px] px-6 pb-16 pt-20">
      <p className="mb-5 font-mono text-[0.8rem] tracking-[0.03em] text-green">
        // always-on AI coding agents · built in rust
      </p>
      <h1 className="max-w-[17ch] font-mono text-[clamp(1.9rem,5vw,3rem)] font-bold leading-[1.1] tracking-[-0.03em]">
        Your coding agents, running <span className="text-green-soft">24/7</span> —
        and always a tap away.
      </h1>
      <p className="mt-5 max-w-[56ch] text-[1.08rem] text-dim">
        Keep a whole team of AI agents working at once, and drop in on any of them
        from your phone or laptop — on the couch, on your commute, wherever you
        are. See what they've done, work on their files, and keep the conversation
        going.
      </p>

      <div className="mt-9 overflow-hidden rounded-[10px] border border-line bg-surface">
        <TitleBar label="cc-screen — 4 agents running" />
        <ul className="list-none px-4 pb-3.5 pt-4 font-mono text-[0.82rem]">
          {SESSIONS.map((s) => (
            <SessionRow key={s.name} {...s} />
          ))}
        </ul>
      </div>

      <p className="mt-7 flex flex-wrap gap-2.5">
        <a
          href="#start"
          className="rounded-lg bg-green px-5 py-[0.68rem] font-mono text-[0.85rem] font-bold text-[#06120a] transition-colors hover:bg-green-soft"
        >
          Get started
        </a>
        <a
          href={GITHUB}
          className="rounded-lg border border-line px-5 py-[0.68rem] font-mono text-[0.85rem] text-ink transition-colors hover:border-green hover:text-green-soft"
        >
          See it on GitHub ↗
        </a>
      </p>
      <p className="mt-6 font-mono text-[0.78rem] text-faint">
        Anthropic · Google · OpenAI · Kimi — your pick, all at once.
      </p>
    </section>
  );
}
