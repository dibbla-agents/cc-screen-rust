import { shots } from "../assets";
import { Phone, Window } from "./ui";

function Copy({
  idx,
  title,
  children,
  wide = false,
}: {
  idx: string;
  title: string;
  children: React.ReactNode;
  wide?: boolean;
}) {
  return (
    <div>
      <span className="mb-2 block font-mono text-[0.82rem] text-green">{idx}</span>
      <h3 className="mb-1.5 font-mono text-[1.12rem] font-bold">{title}</h3>
      <p className={`text-[0.97rem] text-dim ${wide ? "max-w-[60ch]" : "max-w-[44ch]"}`}>
        {children}
      </p>
    </div>
  );
}

export function Features() {
  return (
    <section
      id="features"
      className="mx-auto max-w-[820px] border-t border-line-soft px-6 py-16"
    >
      <p className="mb-3 font-mono text-[0.76rem] tracking-[0.04em] text-green">
        ▸ what it's for
      </p>
      <h2 className="font-mono text-[clamp(1.35rem,3vw,1.8rem)] font-bold tracking-[-0.02em]">
        Built for keeping a lot of agents on the go.
      </h2>

      <div className="mt-10 flex flex-col gap-12">
        {/* 01 — copy left, phone right */}
        <article className="grid items-center gap-9 md:grid-cols-2">
          <Copy idx="01" title="A whole team, always on">
            Run as many agents as you like, side by side, around the clock —
            Anthropic, Google, OpenAI, Kimi, you name it. They keep working whether
            you're watching or not.
          </Copy>
          <div className="flex justify-center md:justify-start">
            <Phone shot={shots.mobileSessions} className="max-w-[232px]" eager />
          </div>
        </article>

        {/* 02 — phone left, copy right */}
        <article className="grid items-center gap-9 md:grid-cols-2">
          <div className="flex justify-center md:order-2 md:justify-end">
            <Phone shot={shots.mobileAgent} className="max-w-[232px]" />
          </div>
          <div className="md:order-1">
            <Copy idx="02" title="Check in from anywhere">
              Open them on your phone or your laptop and jump straight into any one
              of them. Watching TV or heading to work — your agents are a tap away.
            </Copy>
          </div>
        </article>

        {/* 03 — full width cowork window */}
        <article className="flex flex-col gap-6">
          <Copy idx="03" title="Cowork on the files" wide>
            Browse, view, edit, and move files in and out — everything your agents
            are working on. Tree, file and agent side by side. It's a real working
            session, not just a chat box.
          </Copy>
          <Window shot={shots.webCowork} label="cc-screen — file viewer" />
        </article>
      </div>
    </section>
  );
}
