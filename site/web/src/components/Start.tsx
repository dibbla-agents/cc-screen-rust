import type { ReactNode } from "react";
import { CopyButton } from "./ui";

function Cmd({ clip, children }: { clip: string; children: ReactNode }) {
  return (
    <div className="relative mb-2.5 rounded-lg border border-line-soft bg-black/25 px-3.5 py-2.5">
      <pre className="overflow-x-auto">
        <code className="font-mono text-[0.83rem] leading-[1.7] whitespace-pre">
          {children}
        </code>
      </pre>
      <CopyButton text={clip} />
    </div>
  );
}

const Prompt = () => (
  <span className="mr-[0.6ch] select-none text-green">$</span>
);

const RUST_INSTALL =
  "curl --proto '=https' --tlsv1.2 -LsSf https://cc-screen-b4687da9.dibbla.app/dl/install-cc-screen.sh | sh";
const TUI_INSTALL =
  "curl --proto '=https' --tlsv1.2 -LsSf https://cc-screen-b4687da9.dibbla.app/dl/install-ccs.sh | sh";
const HUB_INSTALL =
  "curl --proto '=https' --tlsv1.2 -LsSf https://cc-screen-b4687da9.dibbla.app/dl/install-cc-screen-hub.sh | sh";
const HUB_SLAVE =
  "cc-screen-rust install --hub https://<hub>:8840 --machine-id <name> --hub-only";

function Step({
  badge,
  title,
  note,
  children,
  after,
}: {
  badge: string;
  title: string;
  note: string;
  children: ReactNode;
  after: ReactNode;
}) {
  return (
    <div className="rounded-[10px] border border-line bg-card p-6">
      <h3 className="flex items-center gap-[0.6ch] font-mono text-[0.98rem] font-bold">
        <span className="text-green">{badge}</span> {title}
      </h3>
      <p className="my-3 text-[0.88rem] text-dim">{note}</p>
      {children}
      <p className="mt-3 text-[0.86rem] text-faint">{after}</p>
    </div>
  );
}

export function Start() {
  return (
    <section
      id="start"
      className="mx-auto max-w-[820px] border-t border-line-soft px-6 py-16"
    >
      <p className="mb-3 font-mono text-[0.76rem] tracking-[0.04em] text-green">
        ▸ getting started
      </p>
      <h2 className="font-mono text-[clamp(1.35rem,3vw,1.8rem)] font-bold tracking-[-0.02em]">
        One front door for every machine.
      </h2>
      <p className="mt-4 max-w-[62ch] text-[1.02rem] text-dim">
        The hub is the one address you open and the apps connect to. Each computer
        runs a headless host that dials out to it — so you reach everything in one
        place, and your coding machines never take a connection of their own.
      </p>

      <div className="mt-8 flex flex-col gap-4">
        <Step
          badge="①"
          title="Run the hub — your front door"
          note="One address for everything. It's what you open and what the apps point at."
          after={
            <>
              It serves on your private network (Tailscale) — nothing public. This
              is the address you'll open on every device.
            </>
          }
        >
          <Cmd clip={HUB_INSTALL}>
            <Prompt />
            {HUB_INSTALL}
          </Cmd>
          <Cmd clip="cc-screen-hub install">
            <Prompt />
            cc-screen-hub install{"   "}
            <span className="text-faint"># the front door (its own address)</span>
          </Cmd>
        </Step>

        <Step
          badge="②"
          title="Add your machines"
          note="On each computer where your coding agents live. It runs the agents and dials out to the hub — no screen of its own, nothing to open directly."
          after={
            <>
              Add as many machines as you like; each shows up in the hub's list.
              One machine? Run the hub and host on the same box. Full guide:{" "}
              <a
                className="text-green-soft underline"
                href="https://github.com/dibbla-agents/cc-screen-rust/blob/main/HUB.md"
              >
                HUB.md
              </a>
              .
            </>
          }
        >
          <Cmd clip={RUST_INSTALL}>
            <Prompt />
            {RUST_INSTALL}
          </Cmd>
          <Cmd clip={HUB_SLAVE}>
            <Prompt />
            {HUB_SLAVE}
            {"   "}
            <span className="text-faint"># host only — reached through the hub</span>
          </Cmd>
        </Step>

        <Step
          badge="③"
          title="Open it — phone, browser, or native app"
          note="Everything lives behind the hub's one address. Add the web app to your home screen, or point the native ccs app at the same hub."
          after={
            <>
              See every machine's agents in one list, each tagged with its machine —{" "}
              <span className="text-green-soft">Add to Home Screen</span> on your
              phone for one-tap check-ins.
            </>
          }
        >
          <Cmd clip="https://<hub>:8840">
            <Prompt />
            <span className="text-faint"># open in any browser:</span>{" "}
            {"https://<hub>:8840"}
          </Cmd>
          <Cmd clip={TUI_INSTALL}>
            <Prompt />
            {TUI_INSTALL}
          </Cmd>
          <Cmd clip="ccs --server https://<hub>:8840">
            <Prompt />
            {"ccs --server https://<hub>:8840"}
          </Cmd>
        </Step>
      </div>
    </section>
  );
}
