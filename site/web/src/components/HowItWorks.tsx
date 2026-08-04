import { Cmd, Prompt, Step } from "./ui";
import { APP, DOCS } from "../urls";

// The one-liner contract, byte-for-byte what the hosted hub's dashboard
// generates (frontend/src/components/MultiTenant.tsx, crates/hub/src/install.rs
// — the hub serves /install.sh with its own URL baked in).
const INSTALL = `curl -fsSL ${APP}/install.sh | sh -s -- my-laptop --assistants`;
const INSTALL_WIN = `irm "${APP}/install.ps1?name=my-laptop&assistants=all" | iex`;

export function HowItWorks() {
  return (
    <section
      id="how"
      className="mx-auto max-w-[820px] border-t border-line-soft px-6 py-16"
    >
      <p className="mb-3 font-mono text-[0.76rem] tracking-[0.04em] text-green">
        ▸ how it works
      </p>
      <h2 className="font-mono text-[clamp(1.35rem,3vw,1.8rem)] font-bold tracking-[-0.02em]">
        From zero to agents-on-your-phone in three steps.
      </h2>
      <div className="mt-8 grid gap-4 md:grid-cols-3">
        <Step
          badge="①"
          title="Create an account"
          note="Email + password or Google. Free during the beta — no card."
        >
          <a
            href={APP}
            className="inline-block rounded-lg bg-green px-4 py-2.5 font-mono text-[0.8rem] font-bold text-[#06120a] transition-colors hover:bg-green-soft"
          >
            Sign up at app.ccscreen.dev
          </a>
        </Step>
        <Step
          badge="②"
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
        </Step>
        <Step
          badge="③"
          title="Type the code"
          note="The box shows an 8-character code — approve it on your dashboard and the machine comes online."
        >
          {/* the XXXX-XXXX grouping the product itself prints (src/enroll.rs) */}
          <div className="rounded-lg border border-line-soft bg-black/25 px-3.5 py-2.5 text-center font-mono text-[1.05rem] tracking-[0.25em] text-green-soft">
            WXYZ-MJHT
          </div>
        </Step>
      </div>
      <p className="mt-5 text-[0.9rem] text-dim">
        That's it — open <span className="text-green-soft">app.ccscreen.dev</span>{" "}
        on your phone (Add to Home Screen) and your agents are a tap away.
        Details in the{" "}
        <a className="text-green-soft underline" href={DOCS}>
          docs
        </a>
        .
      </p>
    </section>
  );
}
