// Proposal 0041 §1.3 — the one recipient mini-form behind every share entry
// point (a machine row, a session's menu, the identity bar). Styled to the
// MultiTenant terminal aesthetic. Sharing is an *invite*: committing creates a
// pending grant the recipient must accept ([0040]); we show a brief confirmation
// then close.

import { useState } from "react";
import { createShare } from "../api";

const inputCls =
  "w-full rounded-lg border border-edge bg-bar px-3 py-2.5 font-mono text-sm text-slate-100 outline-none transition placeholder:text-slate-600 focus:border-accent focus:ring-2 focus:ring-accent/25";

export interface ShareSubject {
  // Human summary shown in the header ("cc-screen-rust" or a session label).
  title: string;
  // The owning machine label and (for a session share) the session name.
  machine: string;
  session?: string;
}

export default function ShareForm({
  subject,
  onClose,
  onShared,
}: {
  subject: ShareSubject;
  onClose: () => void;
  onShared?: () => void;
}) {
  const [email, setEmail] = useState("");
  const [peek, setPeek] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState(false);
  const isSession = !!subject.session;

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    const to = email.trim();
    if (busy || !to) return;
    setBusy(true);
    setError(null);
    try {
      await createShare({
        granteeEmail: to,
        machine: subject.machine,
        session: subject.session,
        ownerPeek: isSession ? false : peek,
      });
      setDone(true);
      onShared?.();
      // Brief success flash, then close.
      setTimeout(onClose, 1100);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Could not send the invite.");
    } finally {
      setBusy(false);
    }
  }

  if (done) {
    return (
      <div className="rounded-lg border border-amber/30 bg-amber/10 px-3 py-2.5 text-xs text-amber">
        Invitation sent to <span className="font-semibold">{email.trim()}</span> — they'll see it in their inbox.
      </div>
    );
  }

  return (
    <form onSubmit={submit} className="rounded-lg border border-edge bg-bar/60 p-3">
      <div className="mb-2 flex items-baseline justify-between gap-2">
        <div className="min-w-0 truncate font-mono text-xs text-slate-400">
          Share <span className="text-slate-200">{subject.title}</span>
        </div>
        <span className="shrink-0 rounded border border-accent/25 bg-accent/5 px-1.5 py-0.5 text-[10px] uppercase tracking-wide text-accent/80">
          {isSession ? "view" : "use"}
        </span>
      </div>
      <input
        autoFocus
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
      {!isSession ? (
        <label className="mt-2 flex cursor-pointer items-center gap-2 text-[11px] text-slate-400">
          <input
            type="checkbox"
            checked={peek}
            onChange={(e) => setPeek(e.target.checked)}
            className="h-3.5 w-3.5 accent-accent"
          />
          Also let me see sessions they create on it
        </label>
      ) : (
        <p className="mt-2 text-[11px] text-slate-500">Shares only this session — not the rest of the machine.</p>
      )}
      {error && <div className="mt-2 text-[11px] text-claude">{error}</div>}
      <div className="mt-3 flex justify-end gap-2">
        <button
          type="button"
          onClick={onClose}
          className="rounded-md px-2.5 py-1.5 text-xs text-slate-400 transition hover:text-slate-200"
        >
          Cancel
        </button>
        <button
          type="submit"
          disabled={busy || !email.trim()}
          className="rounded-md bg-accent px-3 py-1.5 text-xs font-semibold text-bar transition hover:brightness-110 disabled:cursor-not-allowed disabled:opacity-40"
        >
          {busy ? "…" : "Share"}
        </button>
      </div>
    </form>
  );
}
