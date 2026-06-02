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
  "curl --proto '=https' --tlsv1.2 -LsSf https://github.com/dibbla-agents/cc-screen-rust/releases/latest/download/cc-screen-rust-installer.sh | sh";
const TUI_INSTALL =
  "curl --proto '=https' --tlsv1.2 -LsSf https://github.com/dibbla-agents/cc-screen-rust/releases/latest/download/cc-screen-tui-installer.sh | sh";

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
        Up and running in two steps.
      </h2>

      <div className="mt-8 grid items-start gap-4 md:grid-cols-2">
        <Step
          badge="①"
          title="Start it on your computer"
          note="The machine where your coding agents already live."
          after={
            <>
              Then open it in any browser and{" "}
              <span className="text-green-soft">Add to Home Screen</span> on your
              phone.
            </>
          }
        >
          <Cmd clip={RUST_INSTALL}>
            <Prompt />
            {"curl --proto '=https' --tlsv1.2 -LsSf \\\n    .../cc-screen-rust-installer.sh | sh"}
          </Cmd>
          <Cmd clip="cc-screen-rust install">
            <Prompt />
            cc-screen-rust install{"   "}
            <span className="text-faint"># keeps it running in the background</span>
          </Cmd>
        </Step>

        <Step
          badge="②"
          title="Or use the native app"
          note="A fast desktop companion for Mac and Linux."
          after="Switch between agents, watch them work, and jump in any time."
        >
          <Cmd clip={TUI_INSTALL}>
            <Prompt />
            {"curl --proto '=https' --tlsv1.2 -LsSf \\\n    .../cc-screen-tui-installer.sh | sh"}
          </Cmd>
          <Cmd clip="ccs --server http://<your-computer>:8839">
            <Prompt />
            {"ccs --server http://<your-computer>:8839"}
          </Cmd>
        </Step>
      </div>
    </section>
  );
}
