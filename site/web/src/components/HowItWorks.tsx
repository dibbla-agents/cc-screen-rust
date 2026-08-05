import { Cmd, Prompt, Eyebrow, SectionHeading, Reveal } from "./ui";
import { APP, DOCS } from "../urls";

// The one-liner contract, byte-for-byte what the hosted hub's dashboard
// generates (frontend/src/components/MultiTenant.tsx, crates/hub/src/install.rs
// — the hub serves /install.sh with its own URL baked in).
const INSTALL = `curl -fsSL ${APP}/install.sh | sh -s -- my-laptop --assistants`;
const INSTALL_WIN = `irm "${APP}/install.ps1?name=my-laptop&assistants=all" | iex`;

// A single rail rung: an accent number, a title + one-line note, and whatever
// terminal/affordance the step needs. Only terminals get a box (B4).
function Rung({
  n,
  title,
  note,
  children,
}: {
  n: number;
  title: string;
  note: string;
  children: React.ReactNode;
}) {
  return (
    <Reveal
      i={n - 1}
      className="grid grid-cols-[auto_1fr] gap-x-5 gap-y-1.5 border-l border-line-soft pb-9 pl-5 last:border-transparent last:pb-0"
    >
      <span className="-ml-[calc(1.25rem+9px)] flex size-[30px] items-center justify-center rounded-full border border-line bg-card font-display text-[0.95rem] font-bold text-accent">
        {n}
      </span>
      <div className="min-w-0">
        <h3 className="text-[1.05rem] font-semibold text-ink">{title}</h3>
        <p className="mt-1 max-w-[52ch] text-[0.92rem] text-dim">{note}</p>
        <div className="mt-4">{children}</div>
      </div>
    </Reveal>
  );
}

export function HowItWorks() {
  return (
    <section id="how" className="mx-auto max-w-[1080px] px-6 py-24">
      <Eyebrow>how it works</Eyebrow>
      <SectionHeading className="mt-3 max-w-[20ch]">
        From zero to agents-on-your-phone in three steps.
      </SectionHeading>

      <ol className="mt-12 max-w-[680px] list-none">
        <Rung
          n={1}
          title="Create an account"
          note="Email + password or Google. Free plan — no card."
        >
          <a
            href={APP}
            className="inline-flex min-h-11 items-center rounded-lg bg-accent px-4 py-2.5 font-semibold text-[#0b111a] transition hover:brightness-110"
          >
            Sign up at app.ccscreen.dev
          </a>
        </Rung>

        <Rung
          n={2}
          title="Paste one line on your dev box"
          note="Installs the agent (and, if you like, the coding assistants), then prints a short code."
        >
          <Cmd clip={INSTALL}>
            <Prompt />
            {INSTALL}
          </Cmd>
          <Cmd clip={INSTALL_WIN}>
            <Prompt />
            {INSTALL_WIN}
            {"  "}
            <span className="text-faint"># Windows (PowerShell)</span>
          </Cmd>
        </Rung>

        <Rung
          n={3}
          title="Type the code"
          note="The box shows an 8-character code — approve it on your dashboard and the machine comes online."
        >
          {/* the XXXX-XXXX grouping the product itself prints (src/enroll.rs) */}
          <div className="max-w-[16rem] rounded-lg border border-line bg-surface px-3.5 py-2.5 text-center font-mono text-[1.05rem] tracking-[0.25em] text-accent">
            WXYZ-MJHT
          </div>
        </Rung>
      </ol>

      <p className="mt-8 max-w-[60ch] text-[0.95rem] text-dim">
        That's it — open{" "}
        <span className="text-accent">app.ccscreen.dev</span> on your phone (Add
        to Home Screen) and your agents are a tap away. Details in the{" "}
        <a className="text-accent underline" href={DOCS}>
          docs
        </a>
        .
      </p>
    </section>
  );
}
