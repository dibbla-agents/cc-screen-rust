// Multi-tenant account UI (proposal 0001 Phase 3): the auth screen (login/signup
// + Google), the device-activation page (/activate), and the machines dashboard.
// Styled to match — and elevate — cc-screen's dark terminal aesthetic: mono type,
// cyan `accent` for live actions, `amber` for "settled/online", a faint grid +
// scanline backdrop, and a terminal-window chrome motif.

import { useEffect, useRef, useState } from "react";
import {
  approveDevice,
  getInviteInfo,
  leaveShare,
  listAgents,
  listOutbox,
  listReceivedShares,
  loginEmail,
  logout,
  revokeShare,
  rotateAgent,
  signup,
  unlinkAgent,
  ApiError,
  type AgentInfo,
  type InviteInfo,
  type MeInfo,
  type MePlan,
  type ReceivedShare,
  type ShareInvite,
} from "../api";
import ShareForm from "./ShareForm";
import { RefreshIcon, ShareIcon } from "../icons";

// One-time injected keyframes/texture (kept out of tailwind.config to avoid a
// build-config change). Rendered once by <Backdrop/>.
const STYLE_ID = "mt-style";
function ensureStyle() {
  if (typeof document === "undefined" || document.getElementById(STYLE_ID)) return;
  const el = document.createElement("style");
  el.id = STYLE_ID;
  el.textContent = `
    @keyframes mt-blink { 0%,49%{opacity:1} 50%,100%{opacity:0} }
    @keyframes mt-rise { from{opacity:0;transform:translateY(8px)} to{opacity:1;transform:translateY(0)} }
    @keyframes mt-pulse { 0%,100%{box-shadow:0 0 0 0 rgba(245,185,66,.55)} 50%{box-shadow:0 0 0 4px rgba(245,185,66,0)} }
    @keyframes mt-scan { from{transform:translateY(-100%)} to{transform:translateY(100%)} }
    .mt-rise{animation:mt-rise .4s cubic-bezier(.2,.8,.2,1) both}
    .mt-cursor{display:inline-block;width:.6ch;height:1.05em;vertical-align:-2px;background:#38bdf8;animation:mt-blink 1.1s steps(1) infinite}
    .mt-dot-on{animation:mt-pulse 2.2s ease-out infinite}
    .mt-grid{background-image:linear-gradient(rgba(36,48,66,.5) 1px,transparent 1px),linear-gradient(90deg,rgba(36,48,66,.5) 1px,transparent 1px);background-size:34px 34px}
    @media (prefers-reduced-motion: reduce){.mt-rise,.mt-cursor,.mt-dot-on{animation:none}}
  `;
  document.head.appendChild(el);
}

function Backdrop({ children }: { children: React.ReactNode }) {
  useEffect(ensureStyle, []);
  return (
    <div className="fixed inset-0 overflow-auto bg-bar text-slate-100">
      {/* Layered atmosphere: a cyan glow up top, a faint engineering grid, and a
          slow scanline — all very low-contrast so content stays the focus. */}
      <div className="pointer-events-none absolute inset-0 mt-grid opacity-[0.35]" />
      <div
        className="pointer-events-none absolute inset-0"
        style={{ background: "radial-gradient(120% 60% at 50% -10%, rgba(56,189,248,.13), transparent 60%)" }}
      />
      <div className="pointer-events-none absolute inset-0 overflow-hidden opacity-[0.04]">
        <div className="h-1/3 w-full bg-accent" style={{ animation: "mt-scan 7s linear infinite" }} />
      </div>
      <div className="relative flex min-h-full flex-col items-center justify-center px-5 py-10">
        {children}
      </div>
    </div>
  );
}

// A terminal-window card: a chrome bar with traffic-light dots + a path crumb and
// blinking cursor, then the body.
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

// ── Plan-limit card (proposal 0056 Part B) — one component, two mounts: the
// /activate error slot (what="machines") and the create form (what="sessions").
// Honest about manual billing: the action is a mailto to the operator, never a
// checkout.
export function LimitCard({
  plan,
  what,
  support,
}: {
  plan?: MePlan;
  what: "machines" | "sessions";
  support?: string | null;
}) {
  const cap = what === "machines" ? plan?.maxAgents : plan?.maxSessions;
  const subject = encodeURIComponent(`cc-screen plan upgrade (${what})`);
  return (
    <div className="mt-3 rounded-lg border border-amber/30 bg-amber/10 p-3 text-left">
      <div className="mb-1 text-[11px] uppercase tracking-wider text-amber">Plan limit reached</div>
      <p className="text-xs text-slate-300">
        Your <span className="text-amber">{plan?.name ?? "current"}</span> plan allows{" "}
        {cap ?? "a limited number of"} {what === "machines" ? "machines" : "concurrent sessions"}.
        {what === "machines" &&
          " Unlink a machine you no longer use, or ask for a bigger plan — the code on the box keeps polling, so approving again just works."}
      </p>
      {support && (
        <a
          href={`mailto:${support}?subject=${subject}`}
          className="mt-2 inline-block rounded-md border border-amber/60 px-2.5 py-1.5 text-xs text-amber hover:bg-amber/10"
        >
          Request an upgrade
        </a>
      )}
      <p className="mt-1.5 text-[11px] text-slate-500">
        Plans are free during the beta — upgrades are switched on by a human, usually the same day.
      </p>
    </div>
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
  onDone,
  onInstall,
  onStartSession,
}: {
  email?: string;
  /// The account's plan facts + support address (from /api/me), for the
  /// plan-limit card when approve answers 402 (proposal 0056 B2).
  plan?: MePlan;
  support?: string | null;
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
  const [result, setResult] = useState<{ ok: boolean; machine?: string; limit?: boolean; error?: string } | null>(null);
  const [missing, setMissing] = useState<string[]>([]);
  const ready = code.replace(/[^A-Z0-9]/gi, "").length === 8;

  // The agent dials in a moment AFTER approval, so its tool list isn't in the
  // registry the instant we land here. Poll briefly, then say nothing rather
  // than guess — an absent line is the correct outcome on a complete machine.
  useEffect(() => {
    if (!result?.ok || !result.machine) return;
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
          {result?.ok ? (
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
              <h2 className="mb-1 font-mono text-base font-semibold text-slate-100">Connect a machine</h2>
              <p className="mb-5 text-sm text-slate-400">
                On the headless box you ran{" "}
                <code className="rounded bg-bar px-1.5 py-0.5 font-mono text-xs text-accent">--enroll</code>. Type the
                code it printed below{email ? <> — approving as <span className="text-slate-300">{email}</span></> : null}.
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
                    <LimitCard plan={plan} what="machines" support={support} />
                  ) : (
                    <div className="mt-3 text-center text-xs text-claude">{result.error}</div>
                  ))}
                <button type="submit" disabled={busy || !ready} className={`${primaryBtn} mt-5`}>
                  {busy ? "Approving…" : "Approve machine"}
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
}: {
  a: AgentInfo;
  onChanged: () => void;
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
          <ShareForm subject={{ title: a.machine, machine: a.machine }} onClose={() => setSharing(false)} onShared={onChanged} />
        </div>
      )}

      {token && (
        <div className="mt-3 rounded-lg border border-amber/30 bg-amber/10 p-3">
          <div className="mb-1 text-[11px] uppercase tracking-wider text-amber">New uplink token — shown once</div>
          <code className="block break-all font-mono text-xs text-slate-200">{token}</code>
          <button
            onClick={() => {
              navigator.clipboard?.writeText(token);
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
  actionLabel,
  confirmText,
  onConfirm,
}: {
  dot: string;
  title: string;
  sub: React.ReactNode;
  badge?: string;
  actionLabel: string;
  confirmText: string;
  onConfirm: () => Promise<void>;
}) {
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
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
        <button
          onClick={() => setConfirming(true)}
          className="shrink-0 rounded-md border border-edge px-2.5 py-1.5 text-xs text-slate-400 transition hover:border-claude hover:text-claude"
        >
          {actionLabel}
        </button>
      </div>
      {confirming && (
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
                  await onConfirm();
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
function SharedCard() {
  const [outbox, setOutbox] = useState<ShareInvite[] | null>(null);
  const [received, setReceived] = useState<ReceivedShare[] | null>(null);

  const reload = () => {
    listOutbox().then(setOutbox).catch(() => setOutbox([]));
    listReceivedShares().then(setReceived).catch(() => setReceived([]));
  };
  useEffect(() => {
    reload();
    const t = setInterval(reload, 8000);
    return () => clearInterval(t);
  }, []);

  // Only the live offers/grants are actionable; terminal rows are dropped.
  // "invited" (proposal 0056 Part C) = an email invite awaiting signup.
  const active = (outbox ?? []).filter(
    (i) => i.status === "pending" || i.status === "accepted" || i.status === "invited"
  );
  const loading = outbox === null || received === null;

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
          {active.map((i) => (
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
          ))}
        </ul>
      )}

      {received && received.length > 0 && (
        <>
          <div className="mb-2 mt-5 flex items-center justify-between">
            <h3 className="text-xs uppercase tracking-wider text-slate-500">Shared with you</h3>
            <span className="text-[11px] text-slate-500">{received.length}</span>
          </div>
          <ul className="space-y-2">
            {received.map((r) => (
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
        </>
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
}: {
  me: MeInfo;
  onClose: () => void;
  onLoggedOut: () => void;
  /// Per-machine entry point into the assistant-update flow (proposal 0049).
  onUpdateAssistants?: (machine: string, tools?: string[]) => void;
  /// Per-machine entry into the create flow (proposal 0056 A2).
  onStartSession?: (machine: string) => void;
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
  const ASSISTANTS = ["claude", "codex", "gemini", "kimi"] as const;
  const ASSISTANT_LABELS: Record<string, string> = {
    claude: "Claude",
    codex: "Codex",
    gemini: "Gemini",
    kimi: "Kimi",
  };
  const [withAssistants, setWithAssistants] = useState(true);
  const [pickedAssistants, setPickedAssistants] = useState<string[]>([...ASSISTANTS]);
  const allPicked = pickedAssistants.length === ASSISTANTS.length;
  const assistantsArg = !withAssistants
    ? ""
    : allPicked
      ? "--assistants"
      : pickedAssistants.length
        ? `--assistants=${pickedAssistants.join(",")}`
        : "";
  const assistantsQuery = !withAssistants
    ? ""
    : allPicked
      ? "&assistants=all"
      : pickedAssistants.length
        ? `&assistants=${encodeURIComponent(pickedAssistants.join(","))}`
        : "";
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
    const t = setInterval(reload, 8000); // live online status
    return () => clearInterval(t);
  }, []);

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
                />
              ))}
            </ul>
          )}
        </Window>

        {/* Sharing — who has access to what, and take it back */}
        <SharedCard />

        {/* Add a machine */}
        <Window path="~/add-machine">
          <h3 className="mb-1 font-mono text-sm font-semibold text-slate-100">Add a machine</h3>
          {/* Cheap cap preemption (proposal 0056 B2): learn about the machine
              cap BEFORE running an installer on a box. Live agent count. */}
          {me.plan && agents !== null && agents.length >= me.plan.maxAgents && (
            <p className="mb-2 rounded-lg border border-amber/30 bg-amber/10 px-3 py-2 text-[11px] text-amber">
              Your {me.plan.name} plan is at its machine limit ({me.plan.maxAgents}) — a new box can't be
              approved until you unlink one{me.supportEmail ? " or request an upgrade" : ""}.
            </p>
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
                {ASSISTANTS.map((k) => (
                  <label key={k} className="flex items-center gap-1.5 text-[11px] text-slate-400">
                    <input
                      type="checkbox"
                      className="h-3.5 w-3.5 accent-amber"
                      checked={pickedAssistants.includes(k)}
                      onChange={(e) =>
                        setPickedAssistants((prev) =>
                          e.target.checked ? [...prev, k] : prev.filter((x) => x !== k)
                        )
                      }
                    />
                    {ASSISTANT_LABELS[k]}
                    {osTab === "win" && k === "kimi" && (
                      <span className="text-slate-600">(unverified on Windows)</span>
                    )}
                  </label>
                ))}
              </div>
            )}
            {withAssistants && osTab === "win" && (
              <p className="mt-2 pl-6 text-[11px] text-slate-500">
                Codex and Gemini need Node.js, which on Windows is a machine-wide installer —
                install it once from{" "}
                <a
                  href="https://nodejs.org/en/download"
                  target="_blank"
                  rel="noreferrer"
                  className="text-accent hover:underline"
                >
                  nodejs.org
                </a>{" "}
                if it isn’t there. Everything else installs for your user only.
              </p>
            )}
          </div>
          <div className="flex items-stretch gap-2">
            <code className="flex-1 overflow-x-auto whitespace-nowrap rounded-lg border border-edge bg-bar px-3 py-2.5 font-mono text-xs text-accent">
              {installCmd}
            </code>
            <button
              onClick={() => {
                navigator.clipboard?.writeText(installCmd);
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
  me,
  onAuthed,
  onDone,
  onLoggedOut,
}: {
  token: string;
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
    getInviteInfo(token)
      .then((i) => alive && setInfo(i))
      .catch((e) => {
        if (!alive) return;
        if (e instanceof ApiError && e.status === 404) setDead(true);
        else setDead(true);
      });
    return () => {
      alive = false;
    };
  }, [token, me.authenticated, me.email]);

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
    return (
      <AuthScreen
        google={me.googleEnabled}
        password={me.passwordLogin !== false}
        hint={`${info.inviterEmail} invited you — sign in or create an account as ${info.email} to see it.`}
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
