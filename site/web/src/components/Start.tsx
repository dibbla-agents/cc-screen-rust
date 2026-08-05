import { Cmd, Prompt, Eyebrow, SectionHeading } from "./ui";
import { DOCS, GITHUB } from "../urls";

// Installer served straight from the GitHub Release (latest tag); the repo is
// public, so this downloads anonymously. No Dibbla /dl mirror in the path.
const GH_DL =
  "https://github.com/dibbla-agents/cc-screen-rust/releases/latest/download";
const HUB_INSTALL = `curl --proto '=https' --tlsv1.2 -LsSf ${GH_DL}/cc-screen-hub-installer.sh | sh`;

export function Start() {
  return (
    // #self-host is the section's anchor; the wrapper keeps the old #start id
    // so pre-0054 deep links still land here.
    <div id="start">
      <section id="self-host" className="mx-auto max-w-[1080px] px-6 py-24">
        <Eyebrow>self-host</Eyebrow>
        <SectionHeading className="mt-3 max-w-[20ch]">
          Prefer your own box? Run the hub yourself.
        </SectionHeading>

        <div className="mt-6 max-w-[62ch]">
          <p className="text-[1.02rem] text-dim">
            Skip app.ccscreen.dev and run the hub on your own machine — same
            product, your box, your network. The hub is the one address you open
            and the apps connect to; each computer runs a headless host that
            dials out to it, so your coding machines never take a connection of
            their own.
          </p>

          <div className="mt-6">
            <Cmd clip={HUB_INSTALL}>
              <Prompt />
              {HUB_INSTALL}
            </Cmd>
          </div>

          <div className="mt-6 flex flex-wrap items-center gap-x-5 gap-y-2">
            <a
              href={DOCS}
              className="inline-flex min-h-11 items-center rounded-lg border border-line px-4 py-2.5 font-mono text-[0.85rem] text-ink transition-colors duration-150 hover:border-accent hover:text-accent"
            >
              Full self-host guide →
            </a>
            <a
              className="font-mono text-[0.85rem] text-dim underline transition-colors hover:text-accent"
              href={`${GITHUB}/blob/main/HUB.md`}
            >
              HUB.md
            </a>
          </div>
        </div>
      </section>
    </div>
  );
}
