import { APP } from "../urls";

/* Plan names and the Free caps are the seeded truth —
   crates/hub/migrations/0003_plan_limits.sql: free (10 machines / 50 concurrent
   sessions), pro, unlimited. No prices exist yet (billing is 0001 Phase 4), so
   this section prints none. */

export function Pricing() {
  return (
    <section
      id="pricing"
      className="mx-auto max-w-[820px] border-t border-line-soft px-6 py-16"
    >
      <p className="mb-3 font-mono text-[0.76rem] tracking-[0.04em] text-green">
        ▸ pricing
      </p>
      <h2 className="font-mono text-[clamp(1.35rem,3vw,1.8rem)] font-bold tracking-[-0.02em]">
        Free during the beta. Pricing to come.
      </h2>
      <div className="mt-8 grid gap-4 md:grid-cols-3">
        {/* Free first in DOM = first on phones. Emphasis via border, not a
            scale-transform (no layout shift on small screens). */}
        <div className="flex flex-col rounded-[10px] border border-green/40 bg-card p-6">
          <h3 className="font-mono text-[1.05rem] font-bold">Free</h3>
          <p className="mt-1 font-mono text-[0.78rem] text-green">
            everything, during the beta
          </p>
          <ul className="mt-4 flex flex-col gap-1.5 text-[0.9rem] text-dim">
            <li>Up to 10 machines</li>
            <li>Up to 50 concurrent agent sessions</li>
            <li>All clients — phone PWA, browser, ccs</li>
          </ul>
          <a
            href={APP}
            className="mt-6 rounded-lg bg-green px-4 py-2.5 text-center font-mono text-[0.8rem] font-bold text-[#06120a] transition-colors hover:bg-green-soft"
          >
            Sign up free
          </a>
        </div>
        <div className="flex flex-col rounded-[10px] border border-line bg-card p-6">
          <h3 className="font-mono text-[1.05rem] font-bold">Pro</h3>
          <p className="mt-1 font-mono text-[0.78rem] text-green">
            for bigger fleets
          </p>
          <ul className="mt-4 flex flex-col gap-1.5 text-[0.9rem] text-dim">
            <li>More machines, more sessions</li>
            <li>Free while in beta — priced later</li>
          </ul>
          <p className="mt-6 font-mono text-[0.78rem] text-faint">
            during the beta, ask and we'll bump you
          </p>
        </div>
        <div className="flex flex-col rounded-[10px] border border-line bg-card p-6">
          <h3 className="font-mono text-[1.05rem] font-bold">Unlimited</h3>
          <p className="mt-1 font-mono text-[0.78rem] text-green">
            no caps at all
          </p>
          <ul className="mt-4 flex flex-col gap-1.5 text-[0.9rem] text-dim">
            <li>As many machines and sessions as you run</li>
            <li>Free while in beta — priced later</li>
          </ul>
          <p className="mt-6 font-mono text-[0.78rem] text-faint">
            during the beta, ask and we'll bump you
          </p>
        </div>
      </div>
      <p className="mt-5 text-[0.86rem] text-faint">
        Hitting a limit tells you in-app; nothing is deleted.
      </p>
    </section>
  );
}
