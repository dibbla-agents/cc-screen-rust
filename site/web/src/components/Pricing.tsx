import { APP } from "../urls";
import { Eyebrow, SectionHeading, Reveal } from "./ui";

/* Plan names and the Free caps are the seeded truth —
   crates/hub/migrations/0003_plan_limits.sql: free (10 machines / 50 concurrent
   sessions), pro, unlimited. No prices exist yet (billing is 0001 Phase 4), so
   this section prints none. */

export function Pricing() {
  return (
    <section id="pricing" className="mx-auto max-w-[1080px] px-6 py-24">
      <Eyebrow>pricing</Eyebrow>
      <SectionHeading className="mt-3 max-w-[20ch]">
        Free during the beta. Pricing to come.
      </SectionHeading>

      <div className="mt-10 max-w-[640px]">
        {/* One emphasized card (accent hairline) carries the CTA + true caps. */}
        <Reveal className="flex flex-col rounded-xl border border-accent/40 bg-card p-6 shadow-[0_24px_64px_-40px_rgba(56,189,248,0.4)]">
          <div className="flex items-baseline justify-between gap-3">
            <h3 className="text-[1.15rem] font-semibold text-ink">Free</h3>
            <span className="font-mono text-[0.78rem] text-accent">
              everything, during the beta
            </span>
          </div>
          <ul className="mt-4 flex flex-col gap-1.5 text-[0.92rem] text-dim">
            <li>Up to 10 machines</li>
            <li>Up to 50 concurrent agent sessions</li>
            <li>All clients — phone PWA, browser, ccs</li>
          </ul>
          <a
            href={APP}
            className="mt-6 inline-flex min-h-11 items-center justify-center rounded-lg bg-accent px-4 py-2.5 text-center font-semibold text-[#0b111a] transition hover:brightness-110"
          >
            Sign up free
          </a>
        </Reveal>

        {/* Pro / Unlimited — two quiet single-line rows beneath. */}
        <Reveal i={1} className="mt-4 flex flex-col divide-y divide-line-soft">
          <div className="flex items-baseline justify-between gap-3 py-3">
            <span className="font-semibold text-ink">Pro</span>
            <span className="text-right text-[0.9rem] text-dim">
              more machines, more sessions
            </span>
          </div>
          <div className="flex items-baseline justify-between gap-3 py-3">
            <span className="font-semibold text-ink">Unlimited</span>
            <span className="text-right text-[0.9rem] text-dim">
              no caps at all
            </span>
          </div>
        </Reveal>

        <p className="mt-5 text-[0.86rem] text-faint">
          Both free during the beta — ask and we'll bump you. Hitting a limit
          tells you in-app; nothing is deleted.
        </p>
      </div>
    </section>
  );
}
