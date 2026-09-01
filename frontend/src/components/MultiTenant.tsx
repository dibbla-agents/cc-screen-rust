// Multi-tenant account UI (proposal 0001 Phase 3): the auth screen (login/signup
// + Google), the device-activation page (/activate), and the machines dashboard.
// Styled to match — and elevate — cc-screen's dark terminal aesthetic: mono type,
// cyan `accent` for live actions, `amber` for "settled/online", a faint grid +
// scanline backdrop, and a terminal-window chrome motif.

import { useEffect, useRef, useState } from "react";
import {
  approveDevice,
  createOrg,
  createOrgInvite,
  getInviteInfo,
  getOrg,
  getOrgInviteInfo,
  leaveOrg,
  leaveShare,
  deleteClientToken,
  listAgents,
  listClientTokens,
  listOrgAudit,
  listOutbox,
  listReceivedShares,
  loginEmail,
  logout,
  openBillingPortal,
  orgInviteInbox,
  removeOrgMember,
  respondOrgInvite,
  revokeOrgInvite,
  regenerateLink,
  revokeShare,
  rotateAgent,
  setMachineTeamVisible,
  setOrgMemberRole,
  signup,
  startCheckout,
  unlinkAgent,
  ApiError,
  type AgentInfo,
  type ClientTokenInfo,
  type InviteInfo,
  type MeInfo,
  type MePlan,
  type OrgAuditEntry,
  type OrgInboxInvite,
  type OrgMember,
  type OrgMine,
  type ReceivedShare,
  type ShareInvite,
} from "../api";
import {
  assistantInstallSelection,
  assistantShortLabel,
  BUILTIN_ASSISTANTS,
  BUILTIN_ASSISTANT_PREFIXES,
  type BuiltinAssistantPrefix,
} from "../assistants";
import ShareForm from "./ShareForm";
import { RefreshIcon, ShareIcon } from "../icons";
import { usePoll } from "../poll";
import { writeClipboard } from "../util";

// One-time injected keyframes/texture (kept out of tailwind.config to avoid a
// build-config change). Rendered once by <Backdrop/>.
const STYLE_ID = "mt-style";
function ensureStyle() {
  if (typeof document === "undefined" || document.getElementById(STYLE_ID)) return;
  const el = document.createElement("style");
  el.id = STYLE_ID;
  // NOTE (proposal 0068 Part B): everything here is either static or a ONE-SHOT
  // entry animation. No `infinite` animations — the dashboard/login backdrop is
  // on screen for hours at a time, and an infinite keyframe on a paint property
  // (box-shadow, background-color) keeps the browser's style/paint pipeline
  // running every vsync for as long as the tab is open. The old perpetual
  // scanline sweep, pulsing status dots, and blinking fake cursor are gone;
  // colour carries the same information.
  el.textContent = `
    @keyframes mt-rise { from{opacity:0;transform:translateY(8px)} to{opacity:1;transform:translateY(0)} }
    .mt-rise{animation:mt-rise .4s cubic-bezier(.2,.8,.2,1) both}
    .mt-cursor{display:inline-block;width:.6ch;height:1.05em;vertical-align:-2px;background:#38bdf8}
    .mt-dot-on{box-shadow:0 0 0 3px rgba(245,185,66,.18)}
    .mt-grid{background-image:linear-gradient(rgba(36,48,66,.5) 1px,transparent 1px),linear-gradient(90deg,rgba(36,48,66,.5) 1px,transparent 1px);background-size:34px 34px}
    @media (prefers-reduced-motion: reduce){.mt-rise{animation:none}}
  `;
  document.head.appendChild(el);
}

function Backdrop({ children }: { children: React.ReactNode }) {
  useEffect(ensureStyle, []);
  return (
    <div className="fixed inset-0 overflow-auto bg-bar text-slate-100">
      {/* Layered atmosphere: a cyan glow up top and a faint engineering grid —
          both very low-contrast so content stays the focus. The scanline that
          used to sweep this layer on an infinite 7s loop is gone: a
          viewport-sized element animating forever kept the compositor busy for
          as long as the page was open (proposal 0068 Part B). */}
      <div className="pointer-events-none absolute inset-0 mt-grid opacity-[0.35]" />
      <div
        className="pointer-events-none absolute inset-0"
        style={{ background: "radial-gradient(120% 60% at 50% -10%, rgba(56,189,248,.13), transparent 60%)" }}
      />
      <div className="relative flex min-h-full flex-col items-center justify-center px-5 py-10">
        {children}
      </div>
    </div>
  );
}

// A terminal-window card: a chrome bar with traffic-light dots + a path crumb and
// a (steady — see ensureStyle) cursor block, then the body.
function Window({
  path,
  children,
  className = "",
}: {
  path: string;
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <div className={`mt-rise w-full overflow-hidden rounded-2xl border border-edge bg-panel shadow-2xl shadow-black/40 ${className}`}>
      <div className="flex items-center gap-2 border-b border-edge bg-bar/60 px-4 py-2.5">
        <span className="h-3 w-3 rounded-full bg-claude/80" />
        <span className="h-3 w-3 rounded-full bg-amber/80" />
        <span className="h-3 w-3 rounded-full bg-codex/80" />
        <span className="ml-2 font-mono text-xs text-slate-500">
          {path}
          <span className="mt-cursor ml-0.5" />
        </span>
      </div>
      <div className="p-6 sm:p-7">{children}</div>
    </div>
  );
}

function Wordmark() {
  return (
    <div className="mb-6 text-center">
      <div className="font-mono text-lg font-semibold tracking-tight text-slate-100">
        cc<span className="text-accent">·</span>screen
      </div>
      <div className="mt-1 text-[11px] uppercase tracking-[0.25em] text-slate-500">
        agents, anywhere
      </div>
    </div>
  );
}

const inputCls =
  "w-full rounded-lg border border-edge bg-bar px-3.5 py-3 font-mono text-sm text-slate-100 outline-none transition placeholder:text-slate-600 focus:border-accent focus:ring-2 focus:ring-accent/25";
const primaryBtn =
  "w-full rounded-lg bg-accent px-3.5 py-3 text-sm font-semibold text-bar transition hover:brightness-110 active:brightness-95 disabled:cursor-not-allowed disabled:opacity-40";
const secondaryBtn =
  "w-full rounded-lg border border-edge px-3.5 py-3 text-sm font-medium text-slate-200 transition hover:border-accent hover:text-accent";

// Founder-offer window (proposal 0058 D1): a `beta`-plan user gets the $5/mo
// locked founder price until this date. Client-side gating is cosmetic (the copy
// only) — the price actually offered is decided server-side at checkout.
const FOUNDER_DEADLINE = Date.parse("2026-10-01");

// ── Plan-limit card (proposal 0056 Part B, 0058 C2) — one component, two mounts:
// the /activate error slot (what="machines") and the create form (what="sessions").
// On a billing-enabled hub the primary action is Stripe checkout; a self-hosted
// hub (billing off) renders exactly today's mailto-primary card.
export function LimitCard({
  plan,
  what,
  support,
  billing = false,
}: {
  plan?: MePlan;
  what: "machines" | "sessions" | "seats";
  support?: string | null;
  billing?: boolean;
}) {
  const cap = what === "machines" ? plan?.maxAgents : plan?.maxSessions;
  const subject = encodeURIComponent(`cc-screen plan upgrade (${what})`);
  const founder = plan?.name === "beta" && Date.now() < FOUNDER_DEADLINE;
  const [checkoutErr, setCheckoutErr] = useState<string | null>(null);

  // ── Team seat cap (proposal 0065 Part D) — its own branch so the
  // machines/sessions cards stay pixel-identical. Owner/admin get the portal
  // (seat changes are portal-driven, 0064); members get the mailto only.
  if (what === "seats") {
    const seats = plan?.seats ?? 0;
    const isOrgAdmin = plan?.orgRole === "owner" || plan?.orgRole === "admin";
    return (
      <div className="mt-3 rounded-lg border border-amber/30 bg-amber/10 p-3 text-left">
        <div className="mb-1 text-[11px] uppercase tracking-wider text-amber">Plan limit reached</div>
        <p className="text-xs text-slate-300">
          {seats > 0 ? (
            <>
              Your team's <span className="text-amber">{seats}</span> seats are all in use.
            </>
          ) : (
            <>Your team is out of seats.</>
          )}{" "}
          {isOrgAdmin
            ? "Add seats and the invite goes through the moment one is free."
            : `Ask ${plan?.ownerEmail || "your team owner"} to add seats.`}
        </p>
        {billing && isOrgAdmin && (
          <button
            type="button"
            onClick={() => {
              setCheckoutErr(null);
              openBillingPortal(plan?.orgId ? { org: plan.orgId } : undefined).catch((e) =>
                setCheckoutErr(e instanceof Error ? e.message : "Couldn't open the billing portal")
              );
            }}
            className={`${primaryBtn} mt-3 min-h-11`}
          >
            Add seats
          </button>
        )}
        {checkoutErr && <div className="mt-2 text-[11px] text-claude">{checkoutErr}</div>}
        {support && (
          <a
            href={`mailto:${support}?subject=${subject}`}
            className="mt-2 inline-block text-[11px] text-slate-400 underline hover:text-slate-200"
          >
            Questions? Email us
          </a>
        )}
        <p className="mt-1.5 text-[11px] text-slate-500">
          Nothing is lost at a limit — pending invites can simply be sent again.
        </p>
      </div>
    );
  }

  const body = (
    <p className="text-xs text-slate-300">
      Your <span className="text-amber">{plan?.name ?? "current"}</span> plan allows{" "}
      {cap ?? "a limited number of"} {what === "machines" ? "machines" : "concurrent sessions"}.
      {what === "machines" &&
        " Unlink a machine you no longer use, or upgrade — the code on the box keeps polling, so approving again just works."}
    </p>
  );

  // Self-hosted hubs (no Stripe): the action stays a mailto to the operator.
  if (!billing) {
    return (
      <div className="mt-3 rounded-lg border border-amber/30 bg-amber/10 p-3 text-left">
        <div className="mb-1 text-[11px] uppercase tracking-wider text-amber">Plan limit reached</div>
        {body}
        {support && (
          <a
            href={`mailto:${support}?subject=${subject}`}
            className="mt-2 inline-block rounded-md border border-amber/60 px-2.5 py-1.5 text-xs text-amber hover:bg-amber/10"
          >
            Request an upgrade
          </a>
        )}
        <p className="mt-1.5 text-[11px] text-slate-500">
          Nothing is deleted at a limit — upgrades are switched on by a human, usually the same day.
        </p>
      </div>
    );
  }

  // Billing-enabled: checkout is the primary action; the mailto survives as a
  // quiet secondary link.
  return (
    <div className="mt-3 rounded-lg border border-amber/30 bg-amber/10 p-3 text-left">
      <div className="mb-1 text-[11px] uppercase tracking-wider text-amber">Plan limit reached</div>
      {body}
      <button
        type="button"
        onClick={() => {
          setCheckoutErr(null);
          startCheckout("pro-monthly").catch((e) =>
            setCheckoutErr(e instanceof Error ? e.message : "Couldn't start checkout")
          );
        }}
        className={`${primaryBtn} mt-3 min-h-11`}
      >
        {founder ? "Upgrade — $5/mo founder price" : "Upgrade to Pro"}
      </button>
      {checkoutErr && <div className="mt-2 text-[11px] text-claude">{checkoutErr}</div>}
      {support && (
        <a
          href={`mailto:${support}?subject=${subject}`}
          className="mt-2 inline-block text-[11px] text-slate-400 underline hover:text-slate-200"
        >
          Questions? Email us
        </a>
      )}
      <p className="mt-1.5 text-[11px] text-slate-500">
        Nothing is deleted at a limit — new machines and sessions unblock the moment you're under the cap.
      </p>
    </div>
  );
}

// ── Dashboard plan card (proposal 0058 C3) — plan name + status badges, the
// machine meter, the sessions cap, and one action (upgrade → checkout, or manage
// → portal). Renders nothing when billing isn't configured on the hub.
function PlanCard({
  me,
  billingPending,
}: {
  me: MeInfo;
  billingPending: "pending" | "slow" | null;
}) {
  const [err, setErr] = useState<string | null>(null);
  if (!me.billing) return null;
  const plan = me.plan;
  const status = plan?.status;
  const subscribed = status === "active" || status === "past_due";
  const periodEnd = plan?.periodEnd;
  const used = plan?.agents ?? 0;
  const cap = plan?.maxAgents ?? 0;
  const pct = cap > 0 ? Math.min(100, Math.round((used / cap) * 100)) : 0;
  const fmtDate = (t: number) =>
    new Date(t * 1000).toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric" });
  const planLabel = plan?.name ? plan.name.charAt(0).toUpperCase() + plan.name.slice(1) : "Free";
  // Team state (proposal 0065 Part D): pooled caps arrive pre-computed on
  // /api/me; the seats meter sits above the machines meter; billing actions are
  // owner/admin-only (portal-driven seat changes, 0064).
  const isTeam = plan?.name === "team";
  const orgAdmin = plan?.orgRole === "owner" || plan?.orgRole === "admin";
  const seatUsed = plan?.members ?? 0;
  const seatCap = plan?.seats ?? 0;
  const seatPct = seatCap > 0 ? Math.min(100, Math.round((seatUsed / seatCap) * 100)) : 0;
  const go = (fn: () => Promise<void>) => {
    setErr(null);
    fn().catch((e) => setErr(e instanceof Error ? e.message : "Something went wrong"));
  };

  return (
    <Window path="~/plan" className="mb-4">
      <div className="mb-4 flex items-center gap-2">
        <h2 className="font-mono text-base font-semibold text-slate-100">
          {isTeam ? "Team plan" : `${planLabel} plan`}
        </h2>
        {isTeam && plan?.orgName && (
          <span className="min-w-0 truncate rounded border border-accent/25 bg-accent/5 px-1.5 py-0.5 text-[10px] uppercase tracking-wider text-accent/80">
            {plan.orgName}
          </span>
        )}
        {status === "past_due" && (
          <span className="rounded border border-amber/50 bg-amber/10 px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-wider text-amber">
            payment failed
          </span>
        )}
        {status === "canceled" && periodEnd && (
          <span className="rounded border border-edge px-1.5 py-0.5 text-[10px] uppercase tracking-wider text-slate-400">
            until {fmtDate(periodEnd)}
          </span>
        )}
      </div>

      {status === "past_due" && (
        <p className="mb-3 rounded-lg border border-amber/30 bg-amber/10 px-3 py-2 text-[11px] text-amber">
          Your last payment didn't go through. You keep full Pro access for about 7 days while we retry —
          update your card to stay on Pro.
        </p>
      )}

      {billingPending === "pending" && (
        <p className="mb-3 rounded-lg border border-accent/30 bg-accent/10 px-3 py-2 text-[11px] text-accent">
          Payment received — activating your Pro plan…
        </p>
      )}
      {billingPending === "slow" && (
        <p className="mb-3 rounded-lg border border-edge px-3 py-2 text-[11px] text-slate-400">
          Taking longer than usual — your payment is safe; this page updates itself
          {me.supportEmail ? (
            <>
              , or email <span className="text-slate-300">{me.supportEmail}</span>
            </>
          ) : null}
          .
        </p>
      )}

      {/* Seats meter (team only, proposal 0065 D) — above machines, same bar. */}
      {isTeam && (
        <>
          <div className="mb-1 flex items-center justify-between text-xs text-slate-400">
            <span>Seats</span>
            <span className="font-mono text-slate-300">
              {seatUsed} / {seatCap}
            </span>
          </div>
          <div className="mb-3 h-1.5 w-full overflow-hidden rounded-full bg-bar">
            <div className="h-full rounded-full bg-accent" style={{ width: `${seatPct}%` }} />
          </div>
        </>
      )}

      <div className="mb-1 flex items-center justify-between text-xs text-slate-400">
        <span>Machines</span>
        <span className="font-mono text-slate-300">
          {used} / {cap}
        </span>
      </div>
      <div className="mb-3 h-1.5 w-full overflow-hidden rounded-full bg-bar">
        <div className="h-full rounded-full bg-accent" style={{ width: `${pct}%` }} />
      </div>
      <p className="mb-4 text-xs text-slate-400">
        Up to <span className="text-slate-300">{plan?.maxSessions ?? 0}</span> concurrent sessions
        {isTeam ? " — pooled across your team" : ""}.
      </p>

      {err && <div className="mb-2 text-[11px] text-claude">{err}</div>}

      {isTeam ? (
        orgAdmin ? (
          <button
            type="button"
            onClick={() => go(() => openBillingPortal(plan?.orgId ? { org: plan.orgId } : undefined))}
            className={`${primaryBtn} min-h-11`}
          >
            Manage billing
          </button>
        ) : (
          <p className="text-center text-[11px] text-slate-500">
            Billing is managed by {plan?.ownerEmail || "your team owner"}.
          </p>
        )
      ) : subscribed ? (
        <button type="button" onClick={() => go(openBillingPortal)} className={`${primaryBtn} min-h-11`}>
          Manage billing
        </button>
      ) : (
        <>
          <button
            type="button"
            onClick={() => go(() => startCheckout("pro-monthly"))}
            className={`${primaryBtn} min-h-11`}
          >
            Upgrade to Pro
          </button>
          <button
            type="button"
            onClick={() => go(() => startCheckout("pro-annual"))}
            className="mt-2 w-full text-center text-[11px] text-slate-400 underline hover:text-slate-200"
          >
            or $8/mo billed annually
          </button>
        </>
      )}
    </Window>
  );
}

function GoogleButton() {
  return (
    <a
      href="/api/auth/google/start"
      className="flex w-full items-center justify-center gap-2.5 rounded-lg border border-edge bg-bar px-3.5 py-3 text-sm font-medium text-slate-200 transition hover:border-slate-500 hover:bg-edge/40"
    >
      <svg width="17" height="17" viewBox="0 0 18 18" aria-hidden>
        <path fill="#4285F4" d="M17.6 9.2c0-.6-.05-1.18-.16-1.74H9v3.29h4.84a4.14 4.14 0 0 1-1.8 2.72v2.26h2.91c1.7-1.57 2.65-3.88 2.65-6.53z" />
        <path fill="#34A853" d="M9 18c2.43 0 4.47-.8 5.96-2.18l-2.91-2.26c-.81.54-1.84.86-3.05.86-2.35 0-4.33-1.58-5.04-3.71H.94v2.33A9 9 0 0 0 9 18z" />
        <path fill="#FBBC05" d="M3.96 10.71A5.4 5.4 0 0 1 3.68 9c0-.59.1-1.17.28-1.71V4.96H.94A9 9 0 0 0 0 9c0 1.45.35 2.83.94 4.04l3.02-2.33z" />
        <path fill="#EA4335" d="M9 3.58c1.32 0 2.5.46 3.44 1.35l2.58-2.58C13.46.89 11.43 0 9 0A9 9 0 0 0 .94 4.96l3.02 2.33C4.67 5.16 6.65 3.58 9 3.58z" />
      </svg>
      Continue with Google
    </a>
  );
}

// ── Auth: login / sign up ─────────────────────────────────────────────────────
export function AuthScreen({
  google,
  password = true,
  hint,
  initialEmail,
  onAuthed,
}: {
  google: boolean;
  /// Whether to offer email/password login + signup. False on a Google-only hub.
  password?: boolean;
  hint?: string;
  /// Prefill for the email field (the /invite landing, proposal 0056 C4).
  initialEmail?: string;
  onAuthed: () => void;
}) {
  const [mode, setMode] = useState<"login" | "signup">("login");
  const [email, setEmail] = useState(initialEmail ?? "");
  const [pw, setPw] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    if (busy || !email || !pw) return;
    setBusy(true);
    setError(null);
    try {
      if (mode === "login") {
        if (await loginEmail(email, pw)) onAuthed();
        else setError("Wrong email or password.");
      } else {
        const r = await signup(email, pw);
        if (r.ok) onAuthed();
        else setError(r.error || "Could not create the account.");
      }
    } catch {
      setError("Network error — try again.");
    } finally {
      setBusy(false);
    }
  }

  return (
    <Backdrop>
      <div className="w-full max-w-sm">
        <Wordmark />
        <Window path={mode === "login" ? "~/login" : "~/signup"}>
          {hint && (
            <div className="mb-4 rounded-lg border border-accent/30 bg-accent/10 px-3 py-2 text-xs text-accent">
              {hint}
            </div>
          )}
          {password && (
            <>
          {/* segmented login/signup toggle */}
          <div className="mb-5 grid grid-cols-2 gap-1 rounded-lg border border-edge bg-bar p-1 text-center text-xs font-medium">
            {(["login", "signup"] as const).map((m) => (
              <button
                key={m}
                type="button"
                onClick={() => {
                  setMode(m);
                  setError(null);
                }}
                className={`rounded-md py-1.5 transition ${
                  mode === m ? "bg-accent text-bar" : "text-slate-400 hover:text-slate-200"
                }`}
              >
                {m === "login" ? "Sign in" : "Create account"}
              </button>
            ))}
          </div>

          <form onSubmit={submit} className="space-y-3">
            <label className="block">
              <span className="mb-1.5 block text-[11px] uppercase tracking-wider text-slate-500">Email</span>
              <input
                autoFocus
                type="email"
                autoComplete="email"
                value={email}
                onChange={(e) => {
                  setEmail(e.target.value);
                  setError(null);
                }}
                placeholder="you@example.com"
                className={inputCls}
              />
            </label>
            <label className="block">
              <span className="mb-1.5 block text-[11px] uppercase tracking-wider text-slate-500">Password</span>
              <input
                type="password"
                autoComplete={mode === "login" ? "current-password" : "new-password"}
                value={pw}
                onChange={(e) => {
                  setPw(e.target.value);
                  setError(null);
                }}
                placeholder={mode === "signup" ? "at least 12 characters" : "••••••••"}
                className={inputCls}
              />
            </label>

            {error && <div className="text-center text-xs text-claude">{error}</div>}

            <button type="submit" disabled={busy || !email || !pw} className={primaryBtn}>
              {busy ? "…" : mode === "login" ? "Sign in" : "Create account"}
            </button>
          </form>
            </>
          )}

          {google && (
            <>
              {password && (
                <div className="my-4 flex items-center gap-3 text-[11px] uppercase tracking-wider text-slate-600">
                  <span className="h-px flex-1 bg-edge" />
                  or
                  <span className="h-px flex-1 bg-edge" />
                </div>
              )}
              <GoogleButton />
            </>
          )}
          {!password && !google && (
            <div className="text-center text-xs text-claude">No login method is configured.</div>
          )}
        </Window>
        <p className="mt-5 text-center font-mono text-[11px] text-slate-600">
          tailnet-grade access to your coding agents
        </p>
      </div>
    </Backdrop>
  );
}

// ── /activate: approve a headless box's device code ───────────────────────────
function formatCode(raw: string): string {
  const clean = raw.toUpperCase().replace(/[^A-Z0-9]/g, "").slice(0, 8);
  return clean.length > 4 ? `${clean.slice(0, 4)}-${clean.slice(4)}` : clean;
}

export function ActivatePage({
  email,
  plan,
  support,
  billing = false,
  onDone,
  onInstall,
  onStartSession,
}: {
  email?: string;
  /// The account's plan facts + support address (from /api/me), for the
  /// plan-limit card when approve answers 402 (proposal 0056 B2).
  plan?: MePlan;
  support?: string | null;
  /// Whether Stripe billing is configured (proposal 0058) — flips the 402
  /// card's action from a mailto to a checkout button.
  billing?: boolean;
  onDone: () => void;
  /// Open the assistant install/update dialog for a machine that just enrolled
  /// short of CLIs (proposal 0050 F3) — the catch-all for "I forgot the flag".
  onInstall?: (machine: string, tools?: string[]) => void;
  /// Open the create flow pre-scoped to the just-connected machine (proposal
  /// 0056 A1) — activation should end in a live terminal, not a dashboard.
  onStartSession?: (machine: string) => void;
}) {
  const [code, setCode] = useState("");
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<{
    ok: boolean;
    machine?: string;
    kind?: string;
    label?: string;
    limit?: boolean;
    error?: string;
  } | null>(null);
  const [missing, setMissing] = useState<string[]>([]);
  const ready = code.replace(/[^A-Z0-9]/gi, "").length === 8;

  // The agent dials in a moment AFTER approval, so its tool list isn't in the
  // registry the instant we land here. Poll briefly, then say nothing rather
  // than guess — an absent line is the correct outcome on a complete machine.
  // A terminal sign-in (kind 'client', proposal 0060) registers no machine, so
  // there is nothing to poll for.
  useEffect(() => {
    if (!result?.ok || !result.machine || result.kind === "client") return;
    const machine = result.machine;
    let tries = 0;
    let stop = false;
    const tick = async () => {
      if (stop || tries++ > 6) return;
      const found = (await listAgents().catch(() => [] as AgentInfo[])).find((a) => a.machine === machine);
      if (stop) return;
      if (found?.online) {
        setMissing(found.missing ?? []);
        return;
      }
      window.setTimeout(tick, 1500);
    };
    tick();
    return () => {
      stop = true;
    };
  }, [result?.ok, result?.machine]);

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    if (busy || !ready) return;
    setBusy(true);
    setResult(null);
    setResult(await approveDevice(code));
    setBusy(false);
  }

  return (
    <Backdrop>
      <div className="w-full max-w-md">
        <Wordmark />
        <Window path="~/activate">
          {result?.ok && result.kind === "client" ? (
            // A terminal sign-in (proposal 0060 B6): no machine enrolled, no
            // plan slot consumed — the terminal picks its token up by itself.
            <div className="py-4 text-center">
              <div className="mx-auto mb-4 flex h-12 w-12 items-center justify-center rounded-full border border-amber/40 bg-amber/10 text-2xl text-amber">
                ✓
              </div>
              <h2 className="font-mono text-base font-semibold text-slate-100">
                Terminal signed in
              </h2>
              <p className="mx-auto mt-2 max-w-xs text-sm text-slate-400">
                <span className="text-amber">{result.label || "Your terminal"}</span> is now connected to
                your account. You can close this page — <code className="font-mono text-xs">ccs</code> picks
                it up by itself. Revoke it anytime under “Terminal clients” on this dashboard.
              </p>
              <button onClick={onDone} className={`${primaryBtn} mt-6`}>
                Done
              </button>
            </div>
          ) : result?.ok ? (
            <div className="py-4 text-center">
              <div className="mx-auto mb-4 flex h-12 w-12 items-center justify-center rounded-full border border-amber/40 bg-amber/10 text-2xl text-amber">
                ✓
              </div>
              <h2 className="font-mono text-base font-semibold text-slate-100">
                {result.machine ? <span className="text-amber">{result.machine}</span> : "Machine"} connected
              </h2>
              <p className="mx-auto mt-2 max-w-xs text-sm text-slate-400">
                It's linked to your account and will appear in your machines. You can close this on the box —
                it's already dialing in.
              </p>
              {missing.length > 0 && (
                <p className="mx-auto mt-3 max-w-xs text-sm text-amber">
                  {missing.length} of the coding assistants aren’t installed there
                  {onInstall ? " — install them now?" : "."}
                </p>
              )}
              {missing.length > 0 && onInstall && result.machine && (
                <button
                  onClick={() => onInstall(result.machine as string, missing)}
                  className={`${primaryBtn} mt-4`}
                >
                  Install {missing.length} assistant{missing.length === 1 ? "" : "s"}
                </button>
              )}
              {/* The hand-off (proposal 0056 A1): activation ends in a live
                  terminal. Primary unless the install prompt already claimed
                  the slot — installing the missing CLIs first is the better
                  first step (0050). */}
              {result.machine && onStartSession && (
                <button
                  onClick={() => onStartSession(result.machine as string)}
                  className={missing.length > 0 && onInstall ? `${secondaryBtn} mt-3` : `${primaryBtn} mt-6`}
                >
                  Start your first session
                </button>
              )}
              <button
                onClick={onDone}
                className={
                  (missing.length > 0 && onInstall) || (result.machine && onStartSession)
                    ? "mt-3 w-full rounded-lg px-3 py-2 text-sm text-slate-400 transition hover:text-slate-200"
                    : `${primaryBtn} mt-6`
                }
              >
                Go to my machines
              </button>
            </div>
          ) : (
            <>
              <h2 className="mb-1 font-mono text-base font-semibold text-slate-100">Connect a device</h2>
              <p className="mb-5 text-sm text-slate-400">
                On a headless box you ran{" "}
                <code className="rounded bg-bar px-1.5 py-0.5 font-mono text-xs text-accent">--enroll</code>, or in a
                terminal you ran{" "}
                <code className="rounded bg-bar px-1.5 py-0.5 font-mono text-xs text-accent">ccs activate</code>. Type
                the code it printed below{email ? <> — approving as <span className="text-slate-300">{email}</span></> : null}.
              </p>
              <form onSubmit={submit}>
                <input
                  autoFocus
                  inputMode="text"
                  autoCapitalize="characters"
                  value={code}
                  onChange={(e) => {
                    setCode(formatCode(e.target.value));
                    setResult(null);
                  }}
                  placeholder="WDJB-MJHT"
                  className="w-full rounded-lg border border-edge bg-bar px-4 py-4 text-center font-mono text-2xl tracking-[0.4em] text-slate-100 outline-none transition placeholder:text-slate-700 focus:border-accent focus:ring-2 focus:ring-accent/25"
                />
                {result?.error &&
                  (result.limit ? (
                    // Machine cap (402) → the plan-limit card, not an error
                    // line (proposal 0056 B2). The enrollment code keeps
                    // polling, so approving again after an unlink just works.
                    <LimitCard plan={plan} what="machines" support={support} billing={billing} />
                  ) : (
                    <div className="mt-3 text-center text-xs text-claude">{result.error}</div>
                  ))}
                <button type="submit" disabled={busy || !ready} className={`${primaryBtn} mt-5`}>
                  {busy ? "Approving…" : "Approve"}
                </button>
              </form>
            </>
          )}
        </Window>
      </div>
    </Backdrop>
  );
}

// ── Dashboard: the user's machines ────────────────────────────────────────────
function timeAgo(epochSecs: number): string {
  const d = Math.max(0, Math.floor(Date.now() / 1000) - epochSecs);
  if (d < 60) return "just now";
  if (d < 3600) return `${Math.floor(d / 60)}m ago`;
  if (d < 86400) return `${Math.floor(d / 3600)}h ago`;
  return `${Math.floor(d / 86400)}d ago`;
}

function MachineRow({
  a,
  onChanged,
  onUpdate,
  onStartSession,
  mail,
}: {
  a: AgentInfo;
  onChanged: () => void;
  /// `me.mail` (proposal 0073 D1), drilled one hop from `Dashboard` purely so
  /// the row's ShareForm can pick its success copy. There is no context in this
  /// app; a narrow optional boolean is the whole dependency.
  mail?: boolean;
  /// Open the "Update coding assistants" flow scoped to this machine (0049).
  /// The per-machine case belongs here, where per-machine administration
  /// already lives; the top-bar button remains the whole-fleet action.
  onUpdate?: (machine: string, tools?: string[]) => void;
  /// Open the create flow scoped to this machine (proposal 0056 A2) — the
  /// standing answer to "I have a machine, now what?".
  onStartSession?: (machine: string) => void;
}) {
  const [confirming, setConfirming] = useState(false);
  const [token, setToken] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [sharing, setSharing] = useState(false);

  return (
    <li className="mt-rise rounded-xl border border-edge bg-bar/50 p-4">
      <div className="flex items-center gap-3">
        <span
          className={`h-2.5 w-2.5 shrink-0 rounded-full ${
            a.online ? "bg-amber mt-dot-on" : "bg-slate-700"
          }`}
          title={a.online ? "online" : "offline"}
        />
        <div className="min-w-0 flex-1">
          <div className="truncate font-mono text-sm font-semibold text-slate-100">{a.machine}</div>
          <div className="text-[11px] text-slate-500">
            {a.online ? <span className="text-amber">online</span> : "offline"} · added {timeAgo(a.createdAt)}
          </div>
        </div>
        <div className="flex shrink-0 items-center gap-1.5">
          {/* The one action a new user actually wants from a machine row
              (proposal 0056 A2): start a session on it. Online machines only. */}
          {onStartSession && a.online && (
            <button
              onClick={() => onStartSession(a.machine)}
              className="flex items-center gap-1 rounded-md border border-accent/60 px-2.5 py-1.5 text-xs font-semibold text-accent transition hover:bg-accent/10"
              title="Start a session on this machine"
            >
              New session
            </button>
          )}
          {/* A linked machine that's short of assistants says so BEFORE it bites
              (proposal 0050 F2) — otherwise the gap only surfaces as a failed
              create. Opens the same dialog scoped to this machine, with the
              install box ticked. Disappears on the next dashboard reload after a
              successful install, because the agent re-advertises its tools. */}
          {onUpdate && a.online && !!a.missing?.length && (
            <button
              onClick={() => onUpdate(a.machine, a.missing)}
              className="flex items-center gap-1 rounded-md border border-amber/60 px-2.5 py-1.5 text-xs text-amber transition hover:border-amber hover:bg-amber/10"
              title={`Not installed here: ${a.missing.join(", ")}. Install them for this machine's user.`}
            >
              ⚠ {a.missing.length} missing · Install
            </button>
          )}
          {onUpdate && a.online && (
            <button
              onClick={() => onUpdate(a.machine)}
              className="flex items-center gap-1 rounded-md border border-edge px-2.5 py-1.5 text-xs text-slate-300 transition hover:border-accent hover:text-accent"
              title="Update this machine's coding assistants, then restart its sessions"
            >
              <RefreshIcon className="h-3.5 w-3.5" />
              Update
            </button>
          )}
          <button
            onClick={() => setSharing((v) => !v)}
            className={`flex items-center gap-1 rounded-md border px-2.5 py-1.5 text-xs transition ${
              sharing ? "border-accent text-accent" : "border-edge text-slate-300 hover:border-accent hover:text-accent"
            }`}
            title="Invite another user to this machine"
          >
            <ShareIcon className="h-3.5 w-3.5" />
            Share
          </button>
          <button
            onClick={async () => {
              setBusy(true);
              setToken(await rotateAgent(a.machine));
              setBusy(false);
            }}
            disabled={busy}
            className="rounded-md border border-edge px-2.5 py-1.5 text-xs text-slate-300 transition hover:border-accent hover:text-accent disabled:opacity-40"
            title="Issue a new uplink token (the old one stops working)"
          >
            Rotate
          </button>
          <button
            onClick={() => setConfirming(true)}
            className="rounded-md border border-edge px-2.5 py-1.5 text-xs text-slate-400 transition hover:border-claude hover:text-claude"
          >
            Unlink
          </button>
        </div>
      </div>

      {sharing && (
        <div className="mt-3">
          <ShareForm
            subject={{ title: a.machine, machine: a.machine }}
            onClose={() => setSharing(false)}
            onShared={onChanged}
            mail={mail}
          />
        </div>
      )}

      {token && (
        <div className="mt-3 rounded-lg border border-amber/30 bg-amber/10 p-3">
          <div className="mb-1 text-[11px] uppercase tracking-wider text-amber">New uplink token — shown once</div>
          <code className="block break-all font-mono text-xs text-slate-200">{token}</code>
          <button
            onClick={() => {
              void writeClipboard(token).catch(() => {});
            }}
            className="mt-2 text-[11px] text-accent hover:underline"
          >
            Copy
          </button>
        </div>
      )}

      {confirming && (
        <div className="mt-3 flex items-center justify-between rounded-lg border border-claude/30 bg-claude/10 px-3 py-2.5">
          <span className="text-xs text-slate-300">Unlink {a.machine}? It'll need to re-enroll.</span>
          <div className="flex gap-2">
            <button
              onClick={() => setConfirming(false)}
              className="rounded-md px-2 py-1 text-xs text-slate-400 hover:text-slate-200"
            >
              Cancel
            </button>
            <button
              onClick={async () => {
                await unlinkAgent(a.agentId);
                setConfirming(false);
                onChanged();
              }}
              className="rounded-md bg-claude px-2.5 py-1 text-xs font-semibold text-bar hover:brightness-110"
            >
              Unlink
            </button>
          </div>
        </div>
      )}
    </li>
  );
}

// One row in the access list: a status dot, the subject + counterpart, and an
// always-visible destructive action that expands the same inline-confirm strip
// MachineRow's Unlink uses (no modal).
function GrantRow({
  dot,
  title,
  sub,
  badge,
  extra,
  actionLabel,
  confirmText,
  onConfirm,
}: {
  dot: string;
  title: string;
  sub: React.ReactNode;
  badge?: string;
  // Optional inline control before the action (proposal 0065 C2's role select).
  extra?: React.ReactNode;
  // Action is optional (proposal 0065): a read-only row (an audit line, a
  // member you can't remove) renders without the destructive button.
  actionLabel?: string;
  confirmText?: string;
  onConfirm?: () => Promise<void>;
}) {
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const actionable = !!actionLabel && !!onConfirm;
  return (
    <li className="mt-rise rounded-xl border border-edge bg-bar/50 p-3.5">
      <div className="flex items-center gap-3">
        <span className={`h-2.5 w-2.5 shrink-0 rounded-full ${dot}`} />
        <div className="min-w-0 flex-1">
          <div className="truncate font-mono text-sm text-slate-100">{title}</div>
          <div className="truncate text-[11px] text-slate-500">{sub}</div>
        </div>
        {badge && (
          <span className="shrink-0 rounded border border-edge px-1.5 py-0.5 text-[10px] uppercase tracking-wide text-slate-400">
            {badge}
          </span>
        )}
        {extra}
        {actionable && (
          <button
            onClick={() => setConfirming(true)}
            className="shrink-0 rounded-md border border-edge px-2.5 py-1.5 text-xs text-slate-400 transition hover:border-claude hover:text-claude"
          >
            {actionLabel}
          </button>
        )}
      </div>
      {confirming && actionable && (
        <div className="mt-3 flex items-center justify-between gap-2 rounded-lg border border-claude/30 bg-claude/10 px-3 py-2.5">
          <span className="text-xs text-slate-300">{confirmText}</span>
          <div className="flex shrink-0 gap-2">
            <button onClick={() => setConfirming(false)} className="rounded-md px-2 py-1 text-xs text-slate-400 hover:text-slate-200">
              Cancel
            </button>
            <button
              disabled={busy}
              onClick={async () => {
                setBusy(true);
                try {
                  await onConfirm?.();
                } finally {
                  setBusy(false);
                  setConfirming(false);
                }
              }}
              className="rounded-md bg-claude px-2.5 py-1 text-xs font-semibold text-bar hover:brightness-110 disabled:opacity-40"
            >
              {actionLabel}
            </button>
          </div>
        </div>
      )}
    </li>
  );
}

function subjectTitle(machine: string | undefined | null, session?: string | null): string {
  const m = machine || "machine";
  return session ? `${m} / ${session}` : m;
}

// The ~/shared dashboard card: "Shared by you" (revoke/cancel) and "Shared with
// you" (leave). Polls on the dashboard cadence so accepted/declined invites flip
// without a manual reload.
// ── Terminal clients (proposal 0060 B4) — every `ccs activate` sign-in, with
// per-token revoke. Metadata only; the token itself existed in plaintext
// exactly once, at handover. Renders nothing while the list is empty.
function TerminalClientsCard() {
  const [tokens, setTokens] = useState<ClientTokenInfo[] | null>(null);

  const reload = () => listClientTokens().then(setTokens).catch(() => setTokens([]));
  // The first load always runs; the recurring poll only once we know there IS a
  // card to keep fresh. Before proposal 0068 the interval was registered above
  // the `if (!tokens?.length) return null` guard, so a user with no terminal
  // clients polled every 8s for a card that rendered nothing. Paused while the
  // tab is hidden, refetched on return.
  usePoll(reload, 8000, { immediate: true, enabled: !!tokens?.length });

  if (!tokens?.length) return null;

  const ago = (secs?: number | null) => {
    if (!secs) return "never used";
    const d = Math.max(0, Math.floor(Date.now() / 1000 - secs));
    if (d < 90) return "just now";
    if (d < 3600) return `${Math.floor(d / 60)} min ago`;
    if (d < 172800) return `${Math.floor(d / 3600)} h ago`;
    return `${Math.floor(d / 86400)} d ago`;
  };

  return (
    <Window path="~/terminal-clients" className="mb-4">
      <h2 className="mb-1 font-mono text-base font-semibold text-slate-100">Terminal clients</h2>
      <p className="mb-3 text-xs text-slate-500">
        Terminals signed in with <code className="font-mono text-slate-400">ccs activate</code>. Revoking one
        logs that terminal out immediately.
      </p>
      <ul className="space-y-2">
        {tokens.map((t) => (
          <li
            key={t.id}
            className="flex items-center justify-between gap-3 rounded-xl border border-edge bg-bar/40 px-3.5 py-2.5"
          >
            <div className="min-w-0">
              <div className="truncate font-mono text-sm text-slate-200">{t.label}</div>
              <div className="text-[11px] text-slate-500">
                added {new Date(t.createdAt * 1000).toLocaleDateString()} · {ago(t.lastUsedAt)}
              </div>
            </div>
            <button
              onClick={async () => {
                if (!window.confirm(`Sign out ${t.label}? It loses access immediately.`)) return;
                await deleteClientToken(t.id);
                reload();
              }}
              className="shrink-0 rounded-lg border border-edge px-3 py-1.5 text-xs text-slate-400 transition hover:border-claude hover:text-claude"
            >
              Revoke
            </button>
          </li>
        ))}
      </ul>
    </Window>
  );
}

// One read-only link grant in the outbox (proposal 0083 Part C). Revoke rides
// the shared GrantRow; Regenerate sits in its `extra` slot and reveals the
// fresh URL inline — which is the whole point of it existing: the token is
// hashed at rest, so a URL the owner has lost can only be REPLACED, never
// re-shown.
function LinkGrantRow({ grant, onChanged }: { grant: ShareInvite; onChanged: () => void }) {
  const [fresh, setFresh] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [copied, setCopied] = useState(false);
  const label = grant.file || "file";
  return (
    <>
      <GrantRow
        dot="bg-amber mt-dot-on"
        title={label}
        sub={
          <>
            → anyone with the link · read-only{grant.machine ? ` · ${grant.machine}` : ""}
          </>
        }
        badge="link"
        extra={
          <button
            disabled={busy}
            onClick={async () => {
              setBusy(true);
              try {
                const r = await regenerateLink(grant.id);
                setFresh(r.inviteUrl);
                onChanged();
              } catch {
                /* the row stays as it was; the old link is untouched */
              } finally {
                setBusy(false);
              }
            }}
            className="shrink-0 rounded-md border border-edge px-2.5 py-1.5 text-xs text-slate-400 transition hover:border-accent hover:text-accent disabled:opacity-50"
            title="Replace this link with a new URL — the old one stops working immediately"
          >
            {busy ? "…" : "New URL"}
          </button>
        }
        actionLabel="Revoke"
        confirmText={`Stop sharing ${label}? The link stops working immediately.`}
        onConfirm={async () => {
          await revokeShare(grant.id);
          onChanged();
        }}
      />
      {fresh && (
        <li className="rounded-xl border border-amber/30 bg-amber/10 p-3 text-xs text-amber">
          <div>New link — the previous URL no longer works. Copy it now; it isn’t shown again:</div>
          <div className="mt-2 flex items-stretch gap-1.5">
            <code className="min-w-0 flex-1 overflow-x-auto whitespace-nowrap rounded-md border border-amber/30 bg-bar/60 px-2 py-1.5 font-mono text-[11px] text-slate-200">
              {fresh}
            </code>
            <button
              onClick={() => {
                void writeClipboard(fresh).catch(() => {});
                setCopied(true);
                setTimeout(() => setCopied(false), 1500);
              }}
              className="min-h-11 shrink-0 rounded-md border border-amber/60 px-2.5 py-1.5 text-[11px] font-semibold text-amber transition hover:bg-amber/10"
            >
              {copied ? "Copied!" : "Copy"}
            </button>
          </div>
        </li>
      )}
    </>
  );
}

function SharedCard() {
  const [outbox, setOutbox] = useState<ShareInvite[] | null>(null);
  const [received, setReceived] = useState<ReceivedShare[] | null>(null);

  const reload = () => {
    listOutbox().then(setOutbox).catch(() => setOutbox([]));
    listReceivedShares().then(setReceived).catch(() => setReceived([]));
  };
  // 8s while visible (the cadence proposals 0041/0065 specify), paused while
  // hidden with a refetch on return — proposal 0068 Part C.
  usePoll(reload, 8000, { immediate: true });

  // Only the live offers/grants are actionable; terminal rows are dropped.
  // "invited" (proposal 0056 Part C) = an email invite awaiting signup.
  // "active" (proposal 0083 Part C) is a live read-only link grant — no
  // acceptance step, so it is live from the moment it is minted.
  const active = (outbox ?? []).filter(
    (i) =>
      i.status === "pending" || i.status === "accepted" || i.status === "invited" || i.status === "active"
  );
  const loading = outbox === null || received === null;
  // Team-materialized rows (proposal 0065 Part B) are EXCLUDED from the grant
  // list — their Leave action is refused server-side, and dozens of
  // unactionable rows are noise. One summary line stands in; the TeamCard
  // (~/team) is the management surface.
  const receivedDirect = (received ?? []).filter((r) => r.origin !== "team");
  const teamRows = (received ?? []).filter((r) => r.origin === "team");
  const teamOwners = new Set(teamRows.map((r) => r.ownerEmail || r.agentId)).size;

  return (
    <Window path="~/shared" className="mb-4">
      <h2 className="mb-4 font-mono text-base font-semibold text-slate-100">Sharing</h2>

      <div className="mb-2 flex items-center justify-between">
        <h3 className="text-xs uppercase tracking-wider text-slate-500">Shared by you</h3>
        {active.length > 0 && <span className="text-[11px] text-slate-500">{active.length}</span>}
      </div>
      {loading ? (
        <div className="py-6 text-center font-mono text-sm text-slate-500">loading…</div>
      ) : active.length === 0 ? (
        <div className="rounded-xl border border-dashed border-edge px-4 py-6 text-center text-xs text-slate-500">
          Nothing shared yet — share a machine from its row, or a session from its menu.
        </div>
      ) : (
        <ul className="space-y-2">
          {active.map((i) =>
            // A read-only link grant (proposal 0083 Part C) reads differently
            // from an invite: the recipient is "anyone with the URL", the
            // subject is a file, and the second action is Regenerate — the
            // only way back to a URL we deliberately cannot show again.
            i.resourceKind === "link" ? (
              <LinkGrantRow key={i.id} grant={i} onChanged={reload} />
            ) : (
              <GrantRow
                key={i.id}
                dot={i.status === "accepted" ? "bg-amber mt-dot-on" : "bg-slate-600"}
                title={subjectTitle(i.machine, i.session)}
                sub={
                  <>
                    → {i.granteeEmail || "—"} · {i.permission === "view" ? "can view" : "can use"}
                  </>
                }
                badge={i.status}
                actionLabel={i.status === "accepted" ? "Revoke" : "Cancel"}
                confirmText={`Stop sharing ${subjectTitle(i.machine, i.session)} with ${i.granteeEmail || "them"}? They lose access immediately.`}
                onConfirm={async () => {
                  await revokeShare(i.id);
                  reload();
                }}
              />
            )
          )}
        </ul>
      )}

      {received && (receivedDirect.length > 0 || teamRows.length > 0) && (
        <>
          <div className="mb-2 mt-5 flex items-center justify-between">
            <h3 className="text-xs uppercase tracking-wider text-slate-500">Shared with you</h3>
            {receivedDirect.length > 0 && (
              <span className="text-[11px] text-slate-500">{receivedDirect.length}</span>
            )}
          </div>
          {receivedDirect.length > 0 && (
            <ul className="space-y-2">
              {receivedDirect.map((r) => (
                <GrantRow
                  key={r.id}
                  dot="bg-accent"
                  title={subjectTitle(r.machine, r.session)}
                  sub={
                    <>
                      from {r.ownerEmail || "—"} · {r.permission === "view" ? "can view" : "can use"}
                    </>
                  }
                  actionLabel="Leave"
                  confirmText={`Leave ${subjectTitle(r.machine, r.session)}? You'll lose access until re-invited.`}
                  onConfirm={async () => {
                    await leaveShare(r.id);
                    reload();
                  }}
                />
              ))}
            </ul>
          )}
          {teamRows.length > 0 && (
            <p className="mt-2 text-[11px] text-slate-500">
              via your team: {teamRows.length} machine{teamRows.length === 1 ? "" : "s"} from{" "}
              {teamOwners} teammate{teamOwners === 1 ? "" : "s"} — managed in{" "}
              <span className="font-mono text-slate-400">~/team</span>
            </p>
          )}
        </>
      )}
    </Window>
  );
}

// ── Teams (proposals 0063/0065) ──────────────────────────────────────────────

// The founder-approved team prices (2026-08-06): $16/seat/mo · $160/seat/yr,
// minimum 3 seats. Client-side numbers are display math only — the server
// resolves the actual price and clamps seats to max(3, N, memberCount).
const TEAM_SEAT_MIN = 3;
const TEAM_MONTHLY_PER_SEAT = 16;
const TEAM_ANNUAL_PER_SEAT = 160;

// The seat picker (proposal 0065 Part D): a stepper (min 3, default 3), a
// monthly/annual toggle, and live total math on the submit button. Checkout
// navigation is full-page via startCheckout (never window.open — the
// installed-PWA return rule).
function SeatPicker({
  busy = false,
  submitPrefix,
  onSubmit,
}: {
  busy?: boolean;
  /// "Start a team" / "Activate" — the live total is appended.
  submitPrefix: string;
  onSubmit: (price: "team-monthly" | "team-annual", seats: number) => void;
}) {
  const [seats, setSeats] = useState(TEAM_SEAT_MIN);
  const [cycle, setCycle] = useState<"monthly" | "annual">("monthly");
  const total =
    cycle === "monthly" ? `$${TEAM_MONTHLY_PER_SEAT * seats}/mo` : `$${TEAM_ANNUAL_PER_SEAT * seats}/yr`;
  const stepBtn =
    "flex min-h-11 min-w-11 items-center justify-center rounded-lg border border-edge bg-bar font-mono text-base text-slate-200 transition hover:border-accent hover:text-accent disabled:cursor-not-allowed disabled:opacity-40";
  return (
    <div>
      <div className="mb-2 flex items-center gap-2">
        <button
          type="button"
          onClick={() => setSeats((n) => Math.max(TEAM_SEAT_MIN, n - 1))}
          disabled={seats <= TEAM_SEAT_MIN}
          aria-label="Fewer seats"
          className={stepBtn}
        >
          −
        </button>
        <div className="min-w-0 flex-1 text-center">
          <div className="font-mono text-lg font-semibold text-slate-100">{seats}</div>
          <div className="text-[10px] uppercase tracking-wider text-slate-500">
            seats (min {TEAM_SEAT_MIN})
          </div>
        </div>
        <button
          type="button"
          onClick={() => setSeats((n) => n + 1)}
          aria-label="More seats"
          className={stepBtn}
        >
          +
        </button>
      </div>
      <div className="mb-3 grid grid-cols-2 gap-1 rounded-lg border border-edge bg-bar p-1 text-center text-xs font-medium">
        {(
          [
            ["monthly", `$${TEAM_MONTHLY_PER_SEAT}/seat/mo`],
            ["annual", `$${TEAM_ANNUAL_PER_SEAT}/seat/yr`],
          ] as const
        ).map(([key, label]) => (
          <button
            key={key}
            type="button"
            onClick={() => setCycle(key)}
            className={`min-h-11 rounded-md py-1.5 transition ${
              cycle === key ? "bg-accent text-bar" : "text-slate-400 hover:text-slate-200"
            }`}
          >
            {label}
          </button>
        ))}
      </div>
      <button
        type="button"
        disabled={busy}
        onClick={() => onSubmit(cycle === "monthly" ? "team-monthly" : "team-annual", seats)}
        className={`${primaryBtn} min-h-11`}
      >
        {busy ? "…" : `${submitPrefix} — ${total}`}
      </button>
    </div>
  );
}

// The org-invite form (proposal 0065 C3) — the ShareForm skeleton against
// POST /api/orgs/invites: email in, ONE success state for both arms (account or
// not — no account-existence oracle) with the copyable /org-invite link as the
// centerpiece. A 402 seat-cap answer renders the seats LimitCard beneath.
// Proposal 0073 D2: the success card's lead sentence is chosen by `me.mail` —
// the per-hub mailer capability — and by nothing else. Exported for the tests
// that cover both states.
export function TeamInviteForm({ me, onInvited }: { me: MeInfo; onInvited: () => void }) {
  const [email, setEmail] = useState("");
  const [role, setRole] = useState<"member" | "admin">("member");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [seatsFull, setSeatsFull] = useState(false);
  const [done, setDone] = useState<{ email: string; inviteUrl: string | null } | null>(null);
  const [copied, setCopied] = useState(false);

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    const to = email.trim();
    if (busy || !to) return;
    setBusy(true);
    setError(null);
    setSeatsFull(false);
    try {
      const r = await createOrgInvite(to, role);
      // A relative link (no CCHUB_PUBLIC_URL) resolves against this origin.
      setDone({
        email: to,
        inviteUrl: r.inviteUrl ? new URL(r.inviteUrl, window.location.origin).toString() : null,
      });
      onInvited();
    } catch (err) {
      if (err instanceof ApiError && err.status === 402) setSeatsFull(true);
      else setError(err instanceof Error ? err.message : "Could not send the invite.");
    } finally {
      setBusy(false);
    }
  }

  if (done) {
    return (
      <div className="rounded-lg border border-amber/30 bg-amber/10 px-3 py-2.5 text-xs text-amber">
        {/* Two lead sentences, chosen ONLY by the per-hub `mail` capability
            (0073 D2) — never by whether the invitee already has an account.
            Present progressive because the send is spawned after the response
            is built: the outbox badge is the only surface that claims an
            outcome. The link keeps its prominence in both states. */}
        {me.mail ? (
          <div>
            Invitation created for <span className="font-semibold">{done.email}</span> — we're
            emailing them, and they'll see it when they sign in. If it doesn't arrive, send them this
            link:
          </div>
        ) : (
          <div>
            Invitation created for <span className="font-semibold">{done.email}</span> — they'll see it
            when they sign in. You can also send them this link:
          </div>
        )}
        {done.inviteUrl && (
          <div className="mt-2 flex items-stretch gap-1.5">
            <code className="min-w-0 flex-1 overflow-x-auto whitespace-nowrap rounded-md border border-amber/30 bg-bar/60 px-2 py-1.5 font-mono text-[11px] text-slate-200">
              {done.inviteUrl}
            </code>
            <button
              type="button"
              onClick={() => {
                if (done.inviteUrl) void writeClipboard(done.inviteUrl).catch(() => {});
                setCopied(true);
                setTimeout(() => setCopied(false), 1500);
              }}
              /* min-h-11 + py (0073 Mobile/touch): `items-stretch` alone left
                 this ~28px tall — the hardest thing on the card to hit, and the
                 one the user reaches for when the mail didn't arrive. */
              className="min-h-11 shrink-0 rounded-md border border-amber/60 px-2.5 py-1.5 text-[11px] font-semibold text-amber transition hover:bg-amber/10"
            >
              {copied ? "Copied!" : "Copy link"}
            </button>
          </div>
        )}
        <button
          type="button"
          onClick={() => {
            setDone(null);
            setEmail("");
            setRole("member");
          }}
          className="mt-2 min-h-11 rounded-md px-1.5 py-1 text-[11px] text-slate-400 transition hover:text-slate-200"
        >
          Invite another
        </button>
      </div>
    );
  }

  return (
    <form onSubmit={submit} className="rounded-lg border border-edge bg-bar/60 p-3">
      <div className="mb-2 font-mono text-xs text-slate-400">Invite a teammate</div>
      <input
        type="email"
        inputMode="email"
        autoCapitalize="none"
        autoComplete="off"
        spellCheck={false}
        value={email}
        onChange={(e) => {
          setEmail(e.target.value);
          setError(null);
        }}
        placeholder="name@example.com"
        className={inputCls}
      />
      <div className="mt-2 flex items-center gap-2">
        <label className="text-[11px] text-slate-500" htmlFor="team-invite-role">
          Role
        </label>
        <select
          id="team-invite-role"
          value={role}
          onChange={(e) => setRole(e.target.value === "admin" ? "admin" : "member")}
          className="min-h-11 rounded-md border border-edge bg-bar px-2 text-xs text-slate-200 outline-none focus:border-accent"
        >
          <option value="member">member</option>
          <option value="admin">admin</option>
        </select>
        <div className="flex-1" />
        <button
          type="submit"
          disabled={busy || !email.trim()}
          className="min-h-11 rounded-md bg-accent px-3 py-1.5 text-xs font-semibold text-bar transition hover:brightness-110 disabled:cursor-not-allowed disabled:opacity-40"
        >
          {busy ? "…" : "Invite"}
        </button>
      </div>
      {error && <div className="mt-2 text-[11px] text-claude">{error}</div>}
      {seatsFull && (
        <LimitCard plan={me.plan} what="seats" support={me.supportEmail} billing={me.billing ?? false} />
      )}
    </form>
  );
}

// The pending-invite badge (proposal 0073 D2) — the outbox's `delivery` column
// rendered in GrantRow's existing pill, no new colour and (deliberately) no
// spinner or animated "sending…": the dashboard is on screen for hours and its
// rule is that nothing on it animates infinitely, because colour carries the
// same information. An absent/NULL delivery is today's `invited` — the correct
// and permanent answer for every row minted before the mailer existed and for
// every hub that never configures one, so an older hub renders unchanged.
export function deliveryBadge(delivery?: string | null): string {
  switch (delivery) {
    case "sending":
      return "sending";
    case "sent":
      return "sent";
    case "failed":
      return "failed";
    case "rejected":
      // A permanent refusal from the relay — the address or its domain does not
      // exist. Named for the human, not the SMTP verb.
      return "bad address";
    default:
      return "invited";
  }
}

// Resend is offered for `failed` ONLY. `rejected` is permanent: retrying cannot
// succeed and each attempt spends the account's send quota, so the affordance
// must not be there to tempt anyone.
export function canResendInvite(delivery?: string | null): boolean {
  return delivery === "failed";
}

// The Resend control (proposal 0073 D2), living in GrantRow's `extra` slot.
// It re-invites through the ordinary create endpoint: a re-invite already mints
// a fresh token and kills the old link, so this is an operation the UI has
// always had, spelled differently. min-h-11 like every other touch target here.
function ResendInvite({
  email,
  role,
  onDone,
}: {
  email: string;
  role: string;
  onDone: () => void;
}) {
  const [busy, setBusy] = useState(false);
  return (
    <button
      type="button"
      disabled={busy}
      onClick={async () => {
        setBusy(true);
        try {
          await createOrgInvite(email, role === "admin" ? "admin" : "member");
        } catch {
          /* the row's own delivery state is the report — nothing to invent here */
        }
        setBusy(false);
        onDone();
      }}
      className="min-h-11 shrink-0 rounded-md border border-edge px-2.5 py-1.5 text-xs text-slate-400 transition hover:border-accent hover:text-accent disabled:opacity-40"
      title="Send the invitation again (this mints a fresh link and retires the old one)"
    >
      {busy ? "…" : "Resend"}
    </button>
  );
}

// The ~/team dashboard window (proposal 0065 Part C): org name + role, the
// member list (GrantRow), pending invites, the invite form, per-machine
// team-visibility toggles, and the Leave footer. With no org: pending org
// invites (accept/decline, with the consent copy) and — on a billing hub — the
// "Start a team" entry with the Part D seat-picker checkout. Renders nothing
// for an org-less user on a billing-less hub.
function TeamCard({ me, onChanged }: { me: MeInfo; onChanged?: () => void }) {
  const [data, setData] = useState<OrgMine | null | undefined>(undefined);
  const [inbox, setInbox] = useState<OrgInboxInvite[]>([]);
  const [err, setErr] = useState<string | null>(null);
  const [inboxErr, setInboxErr] = useState<string | null>(null);
  const [starting, setStarting] = useState(false);
  const [teamName, setTeamName] = useState("");
  const [busy, setBusy] = useState(false);
  const [leaving, setLeaving] = useState(false);

  const reload = () => {
    getOrg()
      .then((d) => setData(d))
      .catch(() => {});
    orgInviteInbox()
      .then(setInbox)
      .catch(() => {});
  };
  usePoll(reload, 8000, { immediate: true });

  // Accept/decline a pending org invite. A 402 (team out of seats) or 409
  // (already in a team) surfaces the server's message inline.
  const respond = async (id: string, accept: boolean) => {
    setInboxErr(null);
    try {
      await respondOrgInvite(id, accept);
      reload();
      if (accept) onChanged?.();
    } catch (e) {
      setInboxErr(e instanceof Error ? e.message : "Could not respond to the invitation.");
      reload();
    }
  };

  if (data === undefined) return null; // first load — appear quietly when known

  // ── No org ──────────────────────────────────────────────────────────────────
  if (data === null) {
    if (inbox.length === 0 && !me.billing) return null;
    return (
      <Window path="~/team" className="mb-4">
        <h2 className="mb-3 font-mono text-base font-semibold text-slate-100">Team</h2>
        {inbox.length > 0 && (
          <ul className="mb-3 space-y-2">
            {inbox.map((i) => (
              <li key={i.id} className="rounded-xl border border-accent/30 bg-accent/5 p-3.5">
                <div className="text-sm text-slate-100">
                  <span className="text-slate-400">{i.inviterEmail || "Someone"}</span> invited you to
                  join <span className="font-semibold">{i.orgName || "a team"}</span>
                  {i.role === "admin" ? " as an admin" : ""}
                </div>
                {i.consent && <p className="mt-1.5 text-[11px] text-amber">{i.consent}</p>}
                <div className="mt-2.5 flex justify-end gap-2">
                  <button
                    onClick={() => respond(i.id, false)}
                    className="min-h-11 rounded-md border border-edge px-2.5 py-1.5 text-xs text-slate-400 transition hover:border-claude hover:text-claude"
                  >
                    Decline
                  </button>
                  <button
                    onClick={() => respond(i.id, true)}
                    className="min-h-11 rounded-md bg-accent px-3 py-1.5 text-xs font-semibold text-bar transition hover:brightness-110"
                  >
                    Join team
                  </button>
                </div>
              </li>
            ))}
          </ul>
        )}
        {inboxErr && <div className="mb-3 text-[11px] text-claude">{inboxErr}</div>}
        {me.billing &&
          (!starting ? (
            <div className="flex flex-wrap items-center justify-between gap-2">
              <p className="min-w-0 text-xs text-slate-400">
                Working with a team? ${TEAM_MONTHLY_PER_SEAT}/seat, min {TEAM_SEAT_MIN} seats — everyone
                sees everyone's sessions.
              </p>
              <button
                onClick={() => setStarting(true)}
                className="min-h-11 shrink-0 rounded-lg border border-accent/60 px-3 text-xs font-semibold text-accent transition hover:bg-accent/10"
              >
                Start a team
              </button>
            </div>
          ) : (
            <div>
              <label className="mb-1.5 block text-[11px] uppercase tracking-wider text-slate-500">
                Team name
              </label>
              <input
                autoFocus
                value={teamName}
                onChange={(e) => {
                  setTeamName(e.target.value);
                  setErr(null);
                }}
                placeholder="acme-eng"
                spellCheck={false}
                autoCapitalize="none"
                className={`${inputCls} mb-3`}
              />
              <SeatPicker
                busy={busy}
                submitPrefix="Start a team"
                onSubmit={async (price, seats) => {
                  const name = teamName.trim();
                  if (!name) {
                    setErr("Give your team a name first.");
                    return;
                  }
                  setBusy(true);
                  setErr(null);
                  try {
                    // Create the org first (0063 B1), then checkout targets it
                    // (0064 B3). If the checkout hop fails, the dormant org
                    // remains and this card shows the Activate state next poll.
                    const { id } = await createOrg(name);
                    onChanged?.();
                    await startCheckout(price, { org: id, seats });
                  } catch (e) {
                    setErr(e instanceof Error ? e.message : "Could not start the team.");
                    reload();
                  } finally {
                    setBusy(false);
                  }
                }}
              />
              {err && <div className="mt-2 text-[11px] text-claude">{err}</div>}
              <button
                onClick={() => setStarting(false)}
                className="mt-2 min-h-11 w-full rounded-lg px-3 py-2 text-xs text-slate-400 transition hover:text-slate-200"
              >
                Cancel
              </button>
            </div>
          ))}
      </Window>
    );
  }

  // ── Member ──────────────────────────────────────────────────────────────────
  const { org, myRole, members, invites, machines } = data;
  const admin = myRole === "owner" || myRole === "admin";
  const dormant = org.seats === 0;
  const roleDot = (r: string) => (r === "owner" ? "bg-accent" : r === "admin" ? "bg-amber" : "bg-slate-600");

  const changeRole = async (m: OrgMember, role: string) => {
    if (role === m.role) return;
    if (
      role === "owner" &&
      !window.confirm(`Transfer ownership of ${org.name} to ${m.email}? You become an admin.`)
    ) {
      reload();
      return;
    }
    setErr(null);
    try {
      await setOrgMemberRole(m.userId, role);
    } catch (e) {
      setErr(e instanceof Error ? e.message : "Could not change the role.");
    }
    reload();
  };

  const toggleVisible = async (agentId: string, visible: boolean) => {
    // Optimistic — the checkbox flips at once; reload reconciles.
    setData((d) =>
      d
        ? { ...d, machines: d.machines.map((x) => (x.agentId === agentId ? { ...x, teamVisible: visible } : x)) }
        : d
    );
    setErr(null);
    try {
      await setMachineTeamVisible(agentId, visible);
    } catch (e) {
      setErr(e instanceof Error ? e.message : "Could not change the machine's visibility.");
    }
    reload();
  };

  const doLeave = async () => {
    setErr(null);
    try {
      await leaveOrg();
      setLeaving(false);
      reload();
      onChanged?.();
    } catch (e) {
      // The last owner gets a 409 with a human message — shown inline.
      setErr(e instanceof Error ? e.message : "Could not leave the team.");
      setLeaving(false);
    }
  };

  return (
    <Window path="~/team" className="mb-4">
      <div className="mb-1 flex items-center gap-2">
        <h2 className="min-w-0 truncate font-mono text-base font-semibold text-slate-100">{org.name}</h2>
        <span className="shrink-0 rounded border border-accent/25 bg-accent/5 px-1.5 py-0.5 text-[10px] uppercase tracking-wide text-accent/80">
          {myRole}
        </span>
      </div>
      <p className="mb-4 text-xs text-slate-500">
        {dormant
          ? "Team created — activate seats to pool your limits and turn on team visibility."
          : `${org.memberCount} member${org.memberCount === 1 ? "" : "s"} · ${org.seats} seat${
              org.seats === 1 ? "" : "s"
            }`}
      </p>

      {dormant && myRole === "owner" && me.billing && (
        <div className="mb-4 rounded-lg border border-accent/30 bg-accent/5 p-3">
          <div className="mb-2 text-[11px] uppercase tracking-wider text-accent">Activate your team</div>
          <SeatPicker
            busy={busy}
            submitPrefix="Activate"
            onSubmit={async (price, seats) => {
              setBusy(true);
              setErr(null);
              try {
                await startCheckout(price, { org: org.id, seats });
              } catch (e) {
                setErr(e instanceof Error ? e.message : "Couldn't start checkout.");
              } finally {
                setBusy(false);
              }
            }}
          />
        </div>
      )}

      <div className="mb-2 flex items-center justify-between">
        <h3 className="text-xs uppercase tracking-wider text-slate-500">Members</h3>
        <span className="text-[11px] text-slate-500">
          {members.length}
          {org.seats > 0 ? ` / ${org.seats}` : ""}
        </span>
      </div>
      <ul className="space-y-2">
        {members.map((m) => {
          const self = m.userId === me.userId;
          const canRemove =
            !self && (myRole === "owner" ? m.role !== "owner" : myRole === "admin" && m.role === "member");
          const canSetRole = myRole === "owner" && !self;
          return (
            <GrantRow
              key={m.userId}
              dot={roleDot(m.role)}
              title={m.email}
              sub={
                <>
                  joined {timeAgo(m.joinedAt)}
                  {self ? " · you" : ""}
                </>
              }
              badge={canSetRole ? undefined : m.role}
              extra={
                canSetRole ? (
                  <select
                    value={m.role}
                    onChange={(e) => changeRole(m, e.target.value)}
                    aria-label={`Role for ${m.email}`}
                    className="min-h-11 shrink-0 rounded-md border border-edge bg-bar px-2 text-xs text-slate-300 outline-none transition focus:border-accent"
                  >
                    <option value="member">member</option>
                    <option value="admin">admin</option>
                    <option value="owner">owner (transfer)</option>
                  </select>
                ) : undefined
              }
              actionLabel={canRemove ? "Remove" : undefined}
              confirmText={`Remove ${m.email}? They lose team visibility immediately.`}
              onConfirm={
                canRemove
                  ? async () => {
                      setErr(null);
                      try {
                        await removeOrgMember(m.userId);
                      } catch (e) {
                        setErr(e instanceof Error ? e.message : "Could not remove the member.");
                      }
                      reload();
                    }
                  : undefined
              }
            />
          );
        })}
        {admin &&
          invites.map((i) => (
            <GrantRow
              key={i.id}
              dot="bg-slate-600"
              title={i.email}
              sub={
                <>
                  invited {timeAgo(i.createdAt)}
                  {i.role === "admin" ? " · as admin" : ""}
                </>
              }
              badge={deliveryBadge(i.delivery)}
              extra={
                canResendInvite(i.delivery) ? (
                  <ResendInvite email={i.email} role={i.role} onDone={reload} />
                ) : undefined
              }
              actionLabel="Cancel"
              confirmText={`Cancel the invitation for ${i.email}?`}
              onConfirm={async () => {
                setErr(null);
                try {
                  await revokeOrgInvite(i.id);
                } catch (e) {
                  setErr(e instanceof Error ? e.message : "Could not cancel the invitation.");
                }
                reload();
              }}
            />
          ))}
      </ul>

      {admin && (
        <div className="mt-3">
          <TeamInviteForm me={me} onInvited={reload} />
        </div>
      )}

      {machines.length > 0 && (
        <>
          <h3 className="mb-1 mt-5 text-xs uppercase tracking-wider text-slate-500">Your machines</h3>
          <p className="mb-2 text-[11px] text-slate-500">
            Checked machines are visible to your team (view-only). Uncheck one to hide it.
          </p>
          <ul className="space-y-1.5">
            {machines.map((mc) => (
              <li key={mc.agentId}>
                <label className="flex min-h-11 cursor-pointer items-center gap-2.5 rounded-lg border border-edge bg-bar/40 px-3 py-2">
                  <input
                    type="checkbox"
                    className="h-4 w-4 shrink-0 accent-accent"
                    checked={mc.teamVisible}
                    onChange={(e) => toggleVisible(mc.agentId, e.target.checked)}
                  />
                  <span className="min-w-0 flex-1 truncate font-mono text-sm text-slate-200">
                    {mc.machine}
                  </span>
                  <span className="shrink-0 text-[11px] text-slate-500">
                    {mc.teamVisible ? "visible to team" : "hidden"}
                  </span>
                </label>
              </li>
            ))}
          </ul>
        </>
      )}

      <div className="mt-5 border-t border-edge pt-3">
        {err && <div className="mb-2 text-[11px] text-claude">{err}</div>}
        {myRole === "owner" ? (
          <p className="text-[11px] text-slate-500">
            As the owner you can't leave — transfer ownership to another member first (the role menu on
            their row).
          </p>
        ) : leaving ? (
          <div className="flex items-center justify-between gap-2 rounded-lg border border-claude/30 bg-claude/10 px-3 py-2.5">
            <span className="text-xs text-slate-300">
              Leave {org.name}? You lose team visibility both ways.
            </span>
            <div className="flex shrink-0 gap-2">
              <button
                onClick={() => setLeaving(false)}
                className="min-h-11 rounded-md px-2 py-1 text-xs text-slate-400 hover:text-slate-200"
              >
                Cancel
              </button>
              <button
                onClick={doLeave}
                className="min-h-11 rounded-md bg-claude px-2.5 py-1 text-xs font-semibold text-bar hover:brightness-110"
              >
                Leave
              </button>
            </div>
          </div>
        ) : (
          <button
            onClick={() => setLeaving(true)}
            className="min-h-11 rounded-lg border border-edge px-3 py-2 text-xs text-slate-400 transition hover:border-claude hover:text-claude"
          >
            Leave team
          </button>
        )}
      </div>
    </Window>
  );
}

// Humanize the audit log's dotted action vocabulary (proposal 0063 Part D) into
// one readable line. Unknown actions fall back to the raw string with the dots
// spaced out, so a newer hub's events still render legibly.
function humanizeAudit(e: OrgAuditEntry): string {
  const actor = e.actorEmail || "system";
  const t = e.target || "";
  let detail: Record<string, unknown> = {};
  if (e.detail) {
    try {
      detail = JSON.parse(e.detail) as Record<string, unknown>;
    } catch {
      /* not JSON — ignore */
    }
  }
  switch (e.action) {
    case "org.created":
      return `${actor} created the team`;
    case "member.joined":
      return `${actor} joined the team`;
    case "member.left":
      return `${actor} left the team`;
    case "member.removed":
      return `${actor} removed ${t || "a member"}`;
    case "member.role_changed":
      return typeof detail.role === "string"
        ? `${actor} made ${t || "a member"} ${detail.role}`
        : `${actor} changed the role of ${t || "a member"}`;
    case "invite.created":
      return `${actor} invited ${t || "someone"}`;
    // Proposal 0073 B1 — the mailer's durable trail. The actor is the hub
    // itself, so the line names the recipient the way invite.created does
    // rather than pretending a person pressed send.
    case "invite.emailed":
      return `${actor} emailed the invitation to ${t || "someone"}`;
    case "invite.declined":
      return `${actor} declined an invitation`;
    case "invite.revoked":
      return `${actor} revoked an invitation`;
    case "invite.expired":
      return "an invitation expired";
    case "machine.visibility_changed": {
      const m = typeof detail.machine === "string" && detail.machine ? detail.machine : t || "a machine";
      return detail.visible === false
        ? `${actor} hid ${m} from the team`
        : `${actor} made ${m} visible to the team`;
    }
    case "org.seats_changed":
      return `${actor} changed the team's seats`;
    case "share.created":
      return `${actor} shared ${t || "a resource"}`;
    case "share.revoked":
      return `${actor} revoked a share`;
    default:
      return `${actor} ${e.action.replace(/\./g, " ")}${t ? ` ${t}` : ""}`;
  }
}

// The minimal audit-log admin view (proposal 0063 D3): read-only lines in
// GrantRow's visual grammar (dot · subject · quiet timestamp), one "Show more"
// button driving the keyset cursor. Mounted only for owners/admins (the parent
// gates on me.org.role); a 404 (demoted mid-session) hides it.
function AuditCard() {
  const [rows, setRows] = useState<OrgAuditEntry[] | null>(null);
  const [hasMore, setHasMore] = useState(false);
  const [busy, setBusy] = useState(false);
  const [hidden, setHidden] = useState(false);

  useEffect(() => {
    listOrgAudit()
      .then((r) => {
        setRows(r);
        setHasMore(r.length >= 50);
      })
      .catch(() => setHidden(true));
  }, []);

  const more = async () => {
    if (!rows?.length || busy) return;
    setBusy(true);
    try {
      const next = await listOrgAudit(rows[rows.length - 1]!.id);
      setRows([...rows, ...next]);
      setHasMore(next.length >= 50);
    } catch {
      setHasMore(false);
    } finally {
      setBusy(false);
    }
  };

  if (hidden || rows === null) return null;

  return (
    <Window path="~/team/audit" className="mb-4">
      <h2 className="mb-1 font-mono text-base font-semibold text-slate-100">Team activity</h2>
      <p className="mb-3 text-xs text-slate-500">
        Membership, invites and visibility changes — visible to owners and admins.
      </p>
      {rows.length === 0 ? (
        <div className="rounded-xl border border-dashed border-edge px-4 py-6 text-center text-xs text-slate-500">
          No team activity yet.
        </div>
      ) : (
        <ul className="space-y-1.5">
          {rows.map((e) => (
            <li key={e.id} className="flex items-start gap-2.5 rounded-lg border border-edge bg-bar/40 px-3 py-2">
              <span className="mt-1.5 h-2 w-2 shrink-0 rounded-full bg-slate-600" />
              <div className="min-w-0 flex-1">
                <div className="break-words font-mono text-xs text-slate-200">{humanizeAudit(e)}</div>
                <div className="text-[11px] text-slate-500">{timeAgo(e.at)}</div>
              </div>
            </li>
          ))}
        </ul>
      )}
      {hasMore && (
        <button
          onClick={more}
          disabled={busy}
          className="mt-3 min-h-11 w-full rounded-lg border border-edge px-3 py-2 text-xs text-slate-300 transition hover:border-accent hover:text-accent disabled:opacity-40"
        >
          {busy ? "…" : "Show more"}
        </button>
      )}
    </Window>
  );
}

// Best-effort browser-OS sniff to pick the default install tab. `userAgentData`
// is the modern signal (falls back to the legacy `platform`).
function detectOs(): "unix" | "win" {
  const nav = navigator as Navigator & { userAgentData?: { platform?: string } };
  const p = nav.userAgentData?.platform ?? navigator.platform ?? "";
  return /win/i.test(p) ? "win" : "unix";
}

export function Dashboard({
  me,
  onClose,
  onLoggedOut,
  onUpdateAssistants,
  onStartSession,
  billingPending = null,
  onMeChanged,
}: {
  me: MeInfo;
  onClose: () => void;
  onLoggedOut: () => void;
  /// Per-machine entry point into the assistant-update flow (proposal 0049).
  onUpdateAssistants?: (machine: string, tools?: string[]) => void;
  /// Per-machine entry into the create flow (proposal 0056 A2).
  onStartSession?: (machine: string) => void;
  /// Checkout-return state (proposal 0058 C4): drives the plan card's
  /// "activating…" notice. null when not returning from Stripe checkout.
  billingPending?: "pending" | "slow" | null;
  /// Re-read /api/me after a membership-changing action (join/leave/create a
  /// team, proposal 0065 C) so the plan/org blocks catch up without a reload.
  onMeChanged?: () => void;
}) {
  const [agents, setAgents] = useState<AgentInfo[] | null>(null);
  const [copied, setCopied] = useState(false);
  const [machineName, setMachineName] = useState("");
  // Default the platform tab to the browser's OS, but let the user switch (they
  // may be reading this on a phone while setting up a Windows box).
  const [osTab, setOsTab] = useState<"unix" | "win">(detectOs());
  // The hub serves its own installer at /install.sh (+ /install.ps1 for Windows)
  // with the hub URL baked in; the user only supplies a machine name. Same origin
  // the browser is on.
  const origin = window.location.origin;
  const safeName = (machineName.trim() || "my-machine").replace(/[^A-Za-z0-9._-]/g, "-");
  // macOS/Linux pass the name as an arg; PowerShell's `iex` can't take one as
  // cleanly, so the hub bakes it in via ?name= (proposal 0045).
  // Coding assistants (proposal 0050): the moment the user is deciding what this
  // box should BE is the right moment to ask, and the answer is then VISIBLE in
  // the command they copy — that is the consent, auditable before it runs.
  // Unticking gives today's command byte-for-byte.
  const [withAssistants, setWithAssistants] = useState(true);
  const [pickedAssistants, setPickedAssistants] = useState<BuiltinAssistantPrefix[]>([
    ...BUILTIN_ASSISTANT_PREFIXES,
  ]);
  const assistantSelection = assistantInstallSelection(withAssistants, pickedAssistants);
  const assistantsArg = assistantSelection.shellArg;
  const assistantsQuery = assistantSelection.query;
  const installShell = `curl -fsSL ${origin}/install.sh | sh -s -- ${safeName}${
    assistantsArg ? ` ${assistantsArg}` : ""
  }`;
  // PowerShell's `irm … | iex` can't take positional args, so the choice rides
  // the served script's query string — the same reason `?name=` exists (0045).
  const installPwsh = `irm ${origin}/install.ps1?name=${encodeURIComponent(safeName)}${assistantsQuery} | iex`;
  const installCmd = osTab === "win" ? installPwsh : installShell;
  const reload = () => listAgents().then(setAgents).catch(() => setAgents([]));
  const firstLoad = useRef(true);
  useEffect(() => {
    if (firstLoad.current) {
      firstLoad.current = false;
      reload();
    }
  }, []);
  usePoll(reload, 8000); // live online status; paused while hidden (0068 C)

  return (
    <Backdrop>
      <div className="w-full max-w-lg">
        <div className="mb-4 flex items-center justify-between">
          <Wordmark />
        </div>
        <Window path="~/machines" className="mb-4">
          <div className="mb-5 flex items-center justify-between gap-3">
            <div className="min-w-0">
              <h2 className="font-mono text-base font-semibold text-slate-100">Your machines</h2>
              <p className="truncate text-xs text-slate-500">{me.email}</p>
            </div>
            <div className="flex shrink-0 gap-2">
              <button
                onClick={onClose}
                className="rounded-lg border border-edge px-3 py-2 text-xs text-slate-300 transition hover:border-accent hover:text-accent"
              >
                ← Back to terminal
              </button>
              <button
                onClick={async () => {
                  await logout();
                  onLoggedOut();
                }}
                className="rounded-lg border border-edge px-3 py-2 text-xs text-slate-400 transition hover:border-claude hover:text-claude"
              >
                Log out
              </button>
            </div>
          </div>

          {agents === null ? (
            <div className="py-10 text-center font-mono text-sm text-slate-500">loading…</div>
          ) : agents.length === 0 ? (
            <div className="rounded-xl border border-dashed border-edge px-4 py-8 text-center">
              <div className="text-sm text-slate-300">No machines yet</div>
              <div className="mt-1 text-xs text-slate-500">Connect your first box below.</div>
            </div>
          ) : (
            <ul className="space-y-2.5">
              {agents.map((a) => (
                <MachineRow
                  key={a.agentId}
                  a={a}
                  onChanged={reload}
                  onUpdate={onUpdateAssistants}
                  onStartSession={onStartSession}
                  mail={me.mail}
                />
              ))}
            </ul>
          )}
        </Window>

        {/* Plan & billing (proposal 0058 C3) — renders nothing on a hub without
            Stripe configured. Above the fold, under the machines it caps. */}
        <PlanCard me={me} billingPending={billingPending} />

        {/* Team (proposals 0063/0065): the plan, then the team it pays for,
            then personal shares. Renders nothing for an org-less user on a
            billing-less hub. */}
        <TeamCard me={me} onChanged={onMeChanged} />

        {/* Sharing — who has access to what, and take it back */}
        <SharedCard />

        {/* The team audit log (proposal 0063 D3) — owners/admins only. */}
        {(me.org?.role === "owner" || me.org?.role === "admin") && <AuditCard />}

        {/* Terminal clients (proposal 0060 B4): every `ccs activate` sign-in,
            with per-token revoke — the browser-side kill switch. Hidden when
            empty (most users meet ccs later, if at all). */}
        <TerminalClientsCard />

        {/* Add a machine */}
        <Window path="~/add-machine">
          <h3 className="mb-1 font-mono text-sm font-semibold text-slate-100">Add a machine</h3>
          {/* Cheap cap preemption (proposal 0056 B2): learn about the machine
              cap BEFORE running an installer on a box. Live agent count. */}
          {me.plan && agents !== null && agents.length >= me.plan.maxAgents && (
            <div className="mb-2 rounded-lg border border-amber/30 bg-amber/10 px-3 py-2">
              <p className="text-[11px] text-amber">
                Your {me.plan.name} plan is at its machine limit ({me.plan.maxAgents}) — a new box can't be
                approved until you unlink one
                {me.billing ? " or upgrade to Pro" : me.supportEmail ? " or request an upgrade" : ""}.
              </p>
              {me.billing && (
                <button
                  type="button"
                  onClick={() => startCheckout("pro-monthly").catch(() => {})}
                  className="mt-2 inline-flex min-h-11 items-center rounded-md bg-amber px-3 py-1.5 text-xs font-semibold text-bar transition hover:brightness-110"
                >
                  Upgrade to Pro
                </button>
              )}
            </div>
          )}
          <p className="mb-3 text-xs text-slate-400">
            Name the machine, then paste the generated command on that box (macOS, Linux, or
            Windows). It installs cc-screen-rust and connects it — a code will appear that you
            approve from{" "}
            <a href="/activate" className="text-accent hover:underline">/activate</a>.
          </p>
          <label className="mb-1.5 block text-[11px] uppercase tracking-wider text-slate-500">Machine name</label>
          <input
            value={machineName}
            onChange={(e) => setMachineName(e.target.value)}
            placeholder="my-laptop"
            spellCheck={false}
            autoCapitalize="none"
            className="mb-3 w-full rounded-lg border border-edge bg-bar px-3.5 py-2.5 font-mono text-sm text-slate-100 outline-none transition placeholder:text-slate-600 focus:border-accent focus:ring-2 focus:ring-accent/25"
          />
          <div className="mb-2 flex gap-1.5">
            {([
              ["unix", "macOS / Linux"],
              ["win", "Windows (PowerShell)"],
            ] as const).map(([key, label]) => (
              <button
                key={key}
                onClick={() => setOsTab(key)}
                className={
                  "rounded-md border px-2.5 py-1 text-[11px] font-semibold transition " +
                  (osTab === key
                    ? "border-accent bg-accent/10 text-accent"
                    : "border-edge text-slate-400 hover:border-accent/60 hover:text-slate-200")
                }
              >
                {label}
              </button>
            ))}
          </div>
          <div className="mb-3 rounded-lg border border-edge bg-bar/50 px-3 py-2.5">
            <label className="flex items-start gap-2 text-[12px] text-slate-300">
              <input
                type="checkbox"
                className="mt-0.5 h-4 w-4 shrink-0 accent-amber"
                checked={withAssistants}
                onChange={(e) => setWithAssistants(e.target.checked)}
              />
              <span className="min-w-0 flex-1">
                Also install the coding assistants
                <span className="block text-[11px] text-slate-500">
                  Installed under that user’s home directory — no sudo. Several hundred MB.
                </span>
              </span>
            </label>
            {withAssistants && (
              <div className="mt-2 flex flex-wrap gap-x-4 gap-y-1.5 pl-6">
                {BUILTIN_ASSISTANTS.map(({ prefix }) => (
                  <label key={prefix} className="flex min-h-11 items-center gap-1.5 text-[11px] text-slate-400">
                    <input
                      type="checkbox"
                      className="h-4 w-4 accent-amber"
                      checked={pickedAssistants.includes(prefix)}
                      onChange={(e) =>
                        setPickedAssistants((prev) =>
                          e.target.checked ? [...prev, prefix] : prev.filter((x) => x !== prefix)
                        )
                      }
                    />
                    {assistantShortLabel(prefix)}
                    {osTab === "win" && prefix === "kimi" && (
                      <span className="text-slate-600">(unverified on Windows)</span>
                    )}
                  </label>
                ))}
              </div>
            )}
            {withAssistants && osTab === "win" && (
              <p className="mt-2 pl-6 text-[11px] text-slate-500">
                Codex, Gemini, and OpenCode need Node.js, which on Windows is a machine-wide
                installer — install it once from{" "}
                <a
                  href="https://nodejs.org/en/download"
                  target="_blank"
                  rel="noreferrer"
                  className="text-accent hover:underline"
                >
                  nodejs.org
                </a>{" "}
                if it isn’t there. Everything else installs for your user only. OpenCode recommends
                WSL for the best Windows experience.
              </p>
            )}
          </div>
          <div className="flex items-stretch gap-2">
            <code className="flex-1 overflow-x-auto whitespace-nowrap rounded-lg border border-edge bg-bar px-3 py-2.5 font-mono text-xs text-accent">
              {installCmd}
            </code>
            <button
              onClick={() => {
                void writeClipboard(installCmd).catch(() => {});
                setCopied(true);
                setTimeout(() => setCopied(false), 1500);
              }}
              className="shrink-0 rounded-lg border border-edge px-3 text-xs font-semibold text-slate-200 transition hover:border-accent hover:text-accent"
            >
              {copied ? "Copied!" : "Copy"}
            </button>
          </div>
          <p className="mt-2 text-[11px] text-slate-600">
            Runs the device-flow enrollment, then installs a background service that reconnects on boot.
          </p>
          {/* Proposal 0060 D4: the terminal path, one dim line — ccs is the
              other client of this same account. */}
          <p className="mt-3 border-t border-edge pt-2 text-[11px] text-slate-500">
            Prefer a terminal? <code className="font-mono text-slate-400">curl -fsSL {origin}/ccs.sh | sh</code>{" "}
            then <code className="font-mono text-slate-400">ccs activate</code> — the same code-approve flow.
          </p>
        </Window>
      </div>
    </Backdrop>
  );
}

// ── /invite/<token> landing (proposal 0056 C4) — the /activate mold. The token
// only *identifies* the invitation (the share still lands via the inbox accept);
// attaching happens server-side when an authenticated, email-matched caller
// reads the info endpoint. Never reveals whether the invited email has an
// account.
export function InviteLanding({
  token,
  org = false,
  me,
  onAuthed,
  onDone,
  onLoggedOut,
}: {
  token: string;
  /// True for a team invite (proposal 0065 C4): the /org-invite/<token> path,
  /// read via GET /api/org-invite/:token. Copy branches on `info.kind`; the
  /// email-mismatch and dead-token screens are shared verbatim.
  org?: boolean;
  me: MeInfo;
  /// Re-read identity after a login/signup from the embedded AuthScreen.
  onAuthed: () => void;
  /// Leave /invite for the app root (the inbox bell does the rest).
  onDone: () => void;
  onLoggedOut: () => void;
}) {
  const [info, setInfo] = useState<InviteInfo | null>(null);
  const [dead, setDead] = useState(false);

  // (Re)read the invite — also after login, because an authenticated matching
  // read is what defensively attaches the invite server-side (C3.3).
  useEffect(() => {
    let alive = true;
    (org ? getOrgInviteInfo(token) : getInviteInfo(token))
      .then((i) => alive && setInfo(i))
      .catch((e) => {
        if (!alive) return;
        if (e instanceof ApiError && e.status === 404) setDead(true);
        else setDead(true);
      });
    return () => {
      alive = false;
    };
  }, [token, org, me.authenticated, me.email]);

  const matched = !!info && me.authenticated && (me.email ?? "").toLowerCase() === info.email.toLowerCase();

  // Authenticated + matching → the invite is attached; hand over to the app
  // (the inbox badge takes it from here).
  useEffect(() => {
    if (matched) onDone();
  }, [matched, onDone]);

  if (dead) {
    return (
      <Backdrop>
        <div className="w-full max-w-sm">
          <Wordmark />
          <Window path="~/invite">
            <p className="text-sm text-slate-300">This invitation link is no longer valid.</p>
            <p className="mt-2 text-xs text-slate-500">
              It may have expired or been cancelled — ask the person who sent it for a fresh one.
            </p>
            <button onClick={onDone} className={`${primaryBtn} mt-5`}>
              Open cc-screen
            </button>
          </Window>
        </div>
      </Backdrop>
    );
  }

  if (!info || matched) {
    return (
      <Backdrop>
        <div className="py-10 text-center font-mono text-sm text-slate-500">loading…</div>
      </Backdrop>
    );
  }

  if (!me.authenticated) {
    // Team invites (proposal 0065 C4): org-specific copy, and the server's
    // normative consent line rendered ON the landing (proposal 0063 B2).
    const hint =
      info.kind === "team"
        ? `${info.inviterEmail} invited you to join ${info.orgName || "a team"} — sign in or create an account as ${info.email} to join.${info.consent ? ` ${info.consent}` : ""}`
        : `${info.inviterEmail} invited you — sign in or create an account as ${info.email} to see it.`;
    return (
      <AuthScreen
        google={me.googleEnabled}
        password={me.passwordLogin !== false}
        hint={hint}
        initialEmail={info.email}
        onAuthed={onAuthed}
      />
    );
  }

  // Authenticated with a DIFFERENT email: say so, offer logout. (Don't reveal
  // anything about the invited address's account status.)
  return (
    <Backdrop>
      <div className="w-full max-w-sm">
        <Wordmark />
        <Window path="~/invite">
          <p className="text-sm text-slate-300">
            This invitation was sent to <span className="text-amber">{info.email}</span>, but you're
            signed in as <span className="text-slate-100">{me.email}</span>.
          </p>
          <p className="mt-2 text-xs text-slate-500">
            Sign out, then sign in (or create an account) as {info.email} to accept it.
          </p>
          <button
            onClick={async () => {
              await logout();
              onLoggedOut();
            }}
            className={`${primaryBtn} mt-5`}
          >
            Log out
          </button>
          <button onClick={onDone} className="mt-3 w-full rounded-lg px-3 py-2 text-sm text-slate-400 transition hover:text-slate-200">
            Keep my session
          </button>
        </Window>
      </div>
    </Backdrop>
  );
}
