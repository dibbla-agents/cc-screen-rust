import { APP } from "../urls";
import { Eyebrow, SectionHeading, Reveal } from "./ui";

/* Plan names + caps are the seeded truth — crates/hub/migrations/0007_plan_
   repricing.sql: Free (2 machines / 5 concurrent sessions) is the new signup
   seed; Pro (10 / 50) is the old free row, now the paid tier; Team is the
   plan_limits 'team' row (10 machines / 50 concurrent sessions per seat,
   pooled) seeded by cc-screen-rust migration 0011_team_billing.sql; the
   `unlimited` plan is admin-only and not shown here.
   Prices per proposal 0058: Pro $10/mo, $8/mo billed annually; per proposal
   0064: Team $16/seat/mo, $160/seat/yr, minimum 3 seats. */

// Pinned when billing goes live (proposal 0058 D1). The site is static, so this
// is a build-time constant with a client-side date guard — a wrong clock only
// shows a stale banner; the price actually offered is decided server-side.
const FOUNDER_DEADLINE = Date.parse("2026-10-01");

export function Pricing() {
  const founder = Date.now() < FOUNDER_DEADLINE;
  return (
    <section id="pricing" className="mx-auto max-w-[1080px] px-6 py-24">
      <Eyebrow>pricing</Eyebrow>
      <SectionHeading className="mt-3 max-w-[22ch]">
        Free forever for one dev. Pro when you scale up.
      </SectionHeading>

      {founder && (
        <Reveal className="mt-8 w-full max-w-[640px] rounded-lg border border-amber/30 bg-amber/10 px-4 py-3 text-[0.9rem] text-dim">
          <span className="font-semibold text-ink">Beta user?</span> Your limits
          are grandfathered forever — and Pro is{" "}
          <span className="text-ink">$5/mo, locked</span>, if you subscribe before
          Oct 1. Teams too: <span className="text-ink">$8/seat, locked</span>.
        </Reveal>
      )}

      {/* DOM order Pro → Team → Free so a phone (single column below 640px)
          reads the priced-and-emphasized card first, the upsell second, free
          last. Two columns at `sm` (Team wraps below the pair), three at `lg`. */}
      <div className="mt-10 grid max-w-[1080px] gap-4 sm:grid-cols-2 lg:grid-cols-3">
        {/* Pro — the emphasized card (accent border + glow). */}
        <Reveal className="flex flex-col rounded-xl border border-accent/40 bg-card p-6 shadow-[0_24px_64px_-40px_rgba(56,189,248,0.4)]">
          <div className="flex items-baseline justify-between gap-3">
            <h3 className="text-[1.15rem] font-semibold text-ink">Pro</h3>
            <span className="text-right font-mono text-[0.78rem] text-accent">
              $10/mo · $8/mo billed annually
            </span>
          </div>
          <ul className="mt-4 flex flex-1 flex-col gap-1.5 text-[0.92rem] text-dim">
            <li>10 machines</li>
            <li>50 concurrent sessions</li>
            <li>Share machines and sessions with anyone</li>
            <li>Full AI session summaries</li>
          </ul>
          <a
            href={APP}
            className="mt-6 inline-flex min-h-11 items-center justify-center rounded-lg bg-accent px-4 py-2.5 text-center font-semibold text-[#0b111a] transition hover:brightness-110"
          >
            Start with Pro
          </a>
        </Reveal>

        {/* Team — a plain card (Pro keeps the accent emphasis; it's still the
            volume product). */}
        <Reveal i={1} className="flex flex-col rounded-xl border border-line bg-card p-6">
          <div className="flex items-baseline justify-between gap-3">
            <h3 className="text-[1.15rem] font-semibold text-ink">Team</h3>
            <span className="text-right font-mono text-[0.78rem] text-dim">
              $16/seat/mo · $160/seat/yr
            </span>
          </div>
          <p className="mt-1 text-right text-[0.78rem] text-faint">
            minimum 3 seats
          </p>
          <ul className="mt-4 flex flex-1 flex-col gap-1.5 text-[0.92rem] text-dim">
            <li>Everything in Pro, per seat</li>
            <li>Pooled 10 machines &amp; 50 concurrent sessions per seat</li>
            <li>See your whole team&apos;s sessions, automatically</li>
            <li>Roles, one invoice, audit log</li>
          </ul>
          <a
            href={APP}
            className="mt-6 inline-flex min-h-11 items-center justify-center rounded-lg border border-line px-4 py-2.5 text-center font-semibold text-ink transition-colors hover:border-accent hover:text-accent"
          >
            Start a team
          </a>
        </Reveal>

        {/* Free — a plain card beside it. */}
        <Reveal i={2} className="flex flex-col rounded-xl border border-line bg-card p-6">
          <div className="flex items-baseline justify-between gap-3">
            <h3 className="text-[1.15rem] font-semibold text-ink">Free</h3>
            <span className="text-right font-mono text-[0.78rem] text-dim">
              $0
            </span>
          </div>
          <ul className="mt-4 flex flex-1 flex-col gap-1.5 text-[0.92rem] text-dim">
            <li>2 machines</li>
            <li>5 concurrent sessions</li>
            <li>Join sessions shared with you</li>
          </ul>
          <a
            href={APP}
            className="mt-6 inline-flex min-h-11 items-center justify-center rounded-lg border border-line px-4 py-2.5 text-center font-semibold text-ink transition-colors hover:border-accent hover:text-accent"
          >
            Sign up free
          </a>
        </Reveal>
      </div>

      <p className="mt-6 max-w-[760px] text-[0.86rem] text-faint">
        Hitting a limit tells you in-app; nothing is ever deleted. Self-hosting
        stays free and uncapped.
      </p>
    </section>
  );
}
