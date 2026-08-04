import { Cmd, Prompt, Step } from "./ui";
import { DOCS, GITHUB } from "../urls";

// Installers are served straight from the GitHub Release (latest tag); the repo
// is public, so these download anonymously. No Dibbla /dl mirror in the path.
const GH_DL =
  "https://github.com/dibbla-agents/cc-screen-rust/releases/latest/download";
const RUST_INSTALL =
  `curl --proto '=https' --tlsv1.2 -LsSf ${GH_DL}/cc-screen-rust-installer.sh | sh`;
// Windows agent installer (proposal 0045) — the PowerShell `irm | iex` twin.
const RUST_INSTALL_WIN =
  `irm ${GH_DL}/cc-screen-rust-installer.ps1 | iex`;
const TUI_INSTALL =
  `curl --proto '=https' --tlsv1.2 -LsSf ${GH_DL}/cc-screen-tui-installer.sh | sh`;
const HUB_INSTALL =
  `curl --proto '=https' --tlsv1.2 -LsSf ${GH_DL}/cc-screen-hub-installer.sh | sh`;
// One consistent worked example — machine "devbox", tailnet host
// "hub.tail1234.ts.net" — so every command is copy-paste-runnable after a
// single obvious edit. No literal <hub>/<name> placeholders on the page.
const EX_HUB = "https://hub.tail1234.ts.net:8840";
const HUB_SLAVE =
  `cc-screen-rust install --hub ${EX_HUB} --machine-id devbox --hub-only`;

export function Start() {
  return (
    // #self-host is the section's anchor; the wrapper keeps the old #start id
    // so pre-0054 deep links still land here.
    <div id="start">
      <section
        id="self-host"
        className="mx-auto max-w-[820px] border-t border-line-soft px-6 py-16"
      >
        <p className="mb-3 font-mono text-[0.76rem] tracking-[0.04em] text-green">
          ▸ self-host
        </p>
        <h2 className="font-mono text-[clamp(1.35rem,3vw,1.8rem)] font-bold tracking-[-0.02em]">
          One front door for every machine.
        </h2>
        <p className="mt-4 max-w-[62ch] text-[1.02rem] text-dim">
          Run the hub yourself instead of using app.ccscreen.dev — same product,
          your box, your network. Full guide in the{" "}
          <a className="text-green-soft underline" href={DOCS}>
            docs
          </a>
          .
        </p>
        <p className="mt-4 max-w-[62ch] text-[1.02rem] text-dim">
          The hub is the one address you open and the apps connect to. Each
          computer runs a headless host that dials out to it — so you reach
          everything in one place, and your coding machines never take a
          connection of their own.
        </p>

        <div className="mt-8 flex flex-col gap-4">
          <Step
            badge="①"
            title="Run the hub — your front door"
            note="One address for everything. It's what you open and what the apps point at."
            after={
              <>
                It serves on your private network (Tailscale) — nothing public.
                This is the address you'll open on every device.
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
                Add as many machines as you like; each shows up in the hub's
                list. One machine? Run the hub and host on the same box. Full
                guide:{" "}
                <a
                  className="text-green-soft underline"
                  href={`${GITHUB}/blob/main/HUB.md`}
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
            <Cmd clip={RUST_INSTALL_WIN}>
              <Prompt />
              {RUST_INSTALL_WIN}
              {"   "}
              <span className="text-faint"># Windows (PowerShell)</span>
            </Cmd>
            <Cmd clip={HUB_SLAVE}>
              <Prompt />
              {HUB_SLAVE}
              {"   "}
              <span className="text-faint">
                # your hub's address + this machine's name — yours will differ
              </span>
            </Cmd>
          </Step>

          <Step
            badge="③"
            title="Open it — phone, browser, or native app"
            note="Everything lives behind the hub's one address. Add the web app to your home screen, or point the native ccs app at the same hub."
            after={
              <>
                See every machine's agents in one list, each tagged with its
                machine —{" "}
                <span className="text-green-soft">Add to Home Screen</span> on
                your phone for one-tap check-ins.
              </>
            }
          >
            <Cmd clip={EX_HUB}>
              <Prompt />
              <span className="text-faint"># open in any browser:</span>{" "}
              {EX_HUB}{" "}
              <span className="text-faint"># yours will differ</span>
            </Cmd>
            <Cmd clip={TUI_INSTALL}>
              <Prompt />
              {TUI_INSTALL}
            </Cmd>
            <Cmd clip={`ccs --server ${EX_HUB}`}>
              <Prompt />
              {`ccs --server ${EX_HUB}`}
              {"   "}
              <span className="text-faint"># yours will differ</span>
            </Cmd>
          </Step>
        </div>
      </section>
    </div>
  );
}
