// Multi-tenant account UI (proposal 0001 Phase 3): the auth screen (login/signup
// + Google), the device-activation page (/activate), and the machines dashboard.
// Styled to match — and elevate — cc-screen's dark terminal aesthetic: mono type,
// cyan `accent` for live actions, `amber` for "settled/online", a faint grid +
// scanline backdrop, and a terminal-window chrome motif.

import { useEffect, useRef, useState } from "react";
import {
  approveDevice,
  listAgents,
  loginEmail,
  logout,
  rotateAgent,
  signup,
  unlinkAgent,
  type AgentInfo,
  type MeInfo,
} from "../api";

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
  hint,
  onAuthed,
}: {
  google: boolean;
  hint?: string;
  onAuthed: () => void;
}) {
  const [mode, setMode] = useState<"login" | "signup">("login");
  const [email, setEmail] = useState("");
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
                placeholder={mode === "signup" ? "at least 8 characters" : "••••••••"}
                className={inputCls}
              />
            </label>

            {error && <div className="text-center text-xs text-claude">{error}</div>}

            <button type="submit" disabled={busy || !email || !pw} className={primaryBtn}>
              {busy ? "…" : mode === "login" ? "Sign in" : "Create account"}
            </button>
          </form>

          {google && (
            <>
              <div className="my-4 flex items-center gap-3 text-[11px] uppercase tracking-wider text-slate-600">
                <span className="h-px flex-1 bg-edge" />
                or
                <span className="h-px flex-1 bg-edge" />
              </div>
              <GoogleButton />
            </>
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

export function ActivatePage({ email, onDone }: { email?: string; onDone: () => void }) {
  const [code, setCode] = useState("");
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<{ ok: boolean; machine?: string; error?: string } | null>(null);
  const ready = code.replace(/[^A-Z0-9]/gi, "").length === 8;

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
              <button onClick={onDone} className={`${primaryBtn} mt-6`}>
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
                {result?.error && <div className="mt-3 text-center text-xs text-claude">{result.error}</div>}
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

function MachineRow({ a, onChanged }: { a: AgentInfo; onChanged: () => void }) {
  const [confirming, setConfirming] = useState(false);
  const [token, setToken] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

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

export function Dashboard({ me, onClose, onLoggedOut }: { me: MeInfo; onClose: () => void; onLoggedOut: () => void }) {
  const [agents, setAgents] = useState<AgentInfo[] | null>(null);
  const [copied, setCopied] = useState(false);
  const [machineName, setMachineName] = useState("");
  // The hub serves its own installer at /install.sh with the hub URL baked in;
  // the user only supplies a machine name. Same origin the browser is on.
  const origin = window.location.origin;
  const safeName = (machineName.trim() || "my-machine").replace(/[^A-Za-z0-9._-]/g, "-");
  const installCmd = `curl -fsSL ${origin}/install.sh | sh -s -- ${safeName}`;
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
                <MachineRow key={a.agentId} a={a} onChanged={reload} />
              ))}
            </ul>
          )}
        </Window>

        {/* Add a machine */}
        <Window path="~/add-machine">
          <h3 className="mb-1 font-mono text-sm font-semibold text-slate-100">Add a machine</h3>
          <p className="mb-3 text-xs text-slate-400">
            Name the machine, then paste the generated command on that box (macOS or Linux). It
            installs cc-screen-rust and connects it — a code will appear that you approve from{" "}
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
