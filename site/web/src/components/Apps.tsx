import type { ReactNode } from "react";
import { shots } from "../assets";
import { Phone, Window, Stage, Shot } from "./ui";

function Code({ children }: { children: ReactNode }) {
  return (
    <code className="rounded bg-[rgba(118,179,96,0.1)] px-[0.35em] py-[0.05em] font-mono text-[0.86em] text-green-soft">
      {children}
    </code>
  );
}

function ClientCopy({ title, children }: { title: ReactNode; children: ReactNode }) {
  return (
    <div>
      <h3 className="mb-1.5 font-mono text-[1.05rem] font-bold">{title}</h3>
      <p className="max-w-[64ch] text-[0.95rem] text-dim">{children}</p>
    </div>
  );
}

export function Apps() {
  return (
    <section
      id="apps"
      className="mx-auto max-w-[820px] border-t border-line-soft px-6 py-16"
    >
      <p className="mb-3 font-mono text-[0.76rem] tracking-[0.04em] text-green">
        ▸ however you work
      </p>
      <h2 className="font-mono text-[clamp(1.35rem,3vw,1.8rem)] font-bold tracking-[-0.02em]">
        Phone, browser, or a native terminal app.
      </h2>
      <p className="mt-4 max-w-[60ch] text-[1.02rem] text-dim">
        Two clients, one wire. The web app is served by the hub — add it to your
        home screen — and <Code>ccs</Code> is a native terminal client. Both connect
        to the hub; same agents, whichever you reach for.
      </p>

      {/* terminal client — two landscape windows */}
      <div className="mt-11">
        <ClientCopy title={<>In your terminal — <Code>ccs</Code></>}>
          A native client that tiles every running agent into a multi-pane grid: six
          built-in layouts, a visual palette, and one-key focus between panes, all
          under a tmux-style <Code>Ctrl-A</Code> prefix.
        </ClientCopy>
        <div className="mt-6 grid grid-cols-1 items-start gap-x-5 gap-y-8 sm:grid-cols-2">
          <Shot caption="Every agent in a multi-pane grid.">
            <Window shot={shots.tuiGrid} label="ccs — grid" />
          </Shot>
          <Shot caption="Six layouts, one keystroke.">
            <Window shot={shots.tuiLayouts} label="ccs — layouts" />
          </Shot>
        </div>
      </div>

      {/* phone & browser — two phones + a portrait window, baseline-aligned */}
      <div className="mt-11">
        <ClientCopy title="On your phone & in the browser">
          The web app installs to your home screen. Browse the whole tree, open a
          file to read or edit, pull files in and out — and spin up a brand-new
          agent in any folder, with nothing to install.
        </ClientCopy>
        <div className="mt-6 grid grid-cols-1 gap-x-5 gap-y-10 sm:grid-cols-3">
          <Shot caption="Browse the tree.">
            <Stage className="h-[24rem] sm:h-[21rem]">
              <Phone shot={shots.mobileFiles} fit="height" />
            </Stage>
          </Shot>
          <Shot caption="Read & edit any file.">
            <Stage className="h-[24rem] sm:h-[21rem]">
              <Phone shot={shots.mobileEditor} fit="height" />
            </Stage>
          </Shot>
          <Shot caption="Start an agent in any folder.">
            <Stage className="h-[24rem] sm:h-[21rem]">
              <Window shot={shots.webNewSession} label="new session" fit="height" />
            </Stage>
          </Shot>
        </div>
      </div>
    </section>
  );
}
