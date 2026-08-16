// Proposal 0041 §1.3 — the one recipient mini-form behind every share entry
// point (a machine row, a session's menu, the identity bar). Styled to the
// MultiTenant terminal aesthetic. Sharing is an *invite*: committing creates a
// pending grant the recipient must accept ([0040]); we show a brief confirmation
// then close. Whether that confirmation says the hub is emailing the invitee is
// decided by the `mail` capability prop ([0073] D2) — per hub, never per
// address; the copyable link stays the centerpiece in both states.

import { useState } from "react";
import { createShare } from "../api";
import { writeClipboard } from "../util";

const inputCls =
  "w-full rounded-lg border border-edge bg-bar px-3 py-2.5 font-mono text-sm text-slate-100 outline-none transition placeholder:text-slate-600 focus:border-accent focus:ring-2 focus:ring-accent/25";

export interface ShareSubject {
  // Human summary shown in the header ("cc-screen-rust" or a session label).
  title: string;
  // The owning machine label and (for a session share) the session name.
  machine: string;
  session?: string;
  // Proposal 0083 Part C — a FILE subject puts the form in `link` mode: no
  // recipient (the URL is the recipient), read-only, one file. `path` is the
  // absolute path on the agent (the hub canonicalizes it at mint); `pathLabel`
  // is the home-relative form we echo back so the user can see what they are
  // about to publish ([0074] §D2's read-only path echo).
  path?: string;
  pathLabel?: string;
}

export default function ShareForm({
  subject,
  onClose,
  onShared,
  mail = false,
}: {
  subject: ShareSubject;
  onClose: () => void;
  onShared?: () => void;
  /// `me.mail` — whether THIS HUB has a mailer configured (proposal 0073 D1).
  /// There is no React context in this app, so it is prop-drilled from `App`
  /// (and, for the machine row, from `Dashboard` → `MachineRow`). Optional and
  /// defaulted to false so a hub that sends nothing — and every existing test
  /// render — keeps today's copy byte-for-byte. It is a per-hub capability,
  /// never a per-address signal: nothing below branches on the invitee.
  mail?: boolean;
}) {
  const [email, setEmail] = useState("");
  const [peek, setPeek] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState(false);
  const [inviteUrl, setInviteUrl] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const isSession = !!subject.session;
  // Proposal 0083 Part C — the link-grant mode. No recipient field, no
  // owner-peek, no email at all: the URL is the recipient, and what it grants
  // is read-only by construction (its own endpoints, hardcoded to the read op).
  const isLink = !!subject.path;

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    const to = email.trim();
    if (busy || (!isLink && !to)) return;
    setBusy(true);
    setError(null);
    try {
      const r = await createShare(
        isLink
          ? { kind: "link", machine: subject.machine, path: subject.path }
          : {
              granteeEmail: to,
              machine: subject.machine,
              session: subject.session,
              ownerPeek: isSession ? false : peek,
            }
      );
      // A relative link (no CCHUB_PUBLIC_URL) resolves against this origin.
      setInviteUrl(r.inviteUrl ? new URL(r.inviteUrl, window.location.origin).toString() : null);
      setDone(true);
      onShared?.();
    } catch (err) {
      setError(
        err instanceof Error
          ? err.message
          : isLink
            ? "Could not create the link."
            : "Could not send the invite."
      );
    } finally {
      setBusy(false);
    }
  }

  // The link grant's success state. The URL is shown ONCE — only its SHA-256 is
  // stored, so nothing can show it again; re-opening the share later offers
  // *Regenerate* instead (which kills this URL at that instant).
  if (done && isLink) {
    return (
      <div className="rounded-lg border border-amber/30 bg-amber/10 px-3 py-2.5 text-xs text-amber">
        <div>
          Read-only link created for <span className="font-semibold">{subject.title}</span>. Anyone with
          this URL can read the file until you revoke it — copy it now, it isn’t shown again:
        </div>
        {inviteUrl && (
          <div className="mt-2 flex items-stretch gap-1.5">
            <code className="min-w-0 flex-1 overflow-x-auto whitespace-nowrap rounded-md border border-amber/30 bg-bar/60 px-2 py-1.5 font-mono text-[11px] text-slate-200">
              {inviteUrl}
            </code>
            <button
              type="button"
              onClick={() => {
                void writeClipboard(inviteUrl).catch(() => {});
                setCopied(true);
                setTimeout(() => setCopied(false), 1500);
              }}
              className="min-h-11 shrink-0 rounded-md border border-amber/60 px-2.5 py-1.5 text-[11px] font-semibold text-amber transition hover:bg-amber/10"
            >
              {copied ? "Copied!" : "Copy link"}
            </button>
          </div>
        )}
        <div className="mt-2 text-[11px] text-slate-400">
          Manage or revoke it under Shares in your account.
        </div>
        <button
          type="button"
          onClick={onClose}
          className="mt-2 min-h-11 rounded-md px-1.5 py-1 text-[11px] text-slate-400 transition hover:text-slate-200"
        >
          Done
        </button>
      </div>
    );
  }

  if (done) {
    // ONE success message for both outcomes — whether the address already has
    // an account is deliberately not disclosed (proposal 0056 C2 / [0042]).
    // Two lead sentences, chosen ONLY by the per-hub `mail` capability
    // (proposal 0073 D2), never by anything about the invitee. Present
    // progressive on purpose: the send is spawned after the response is built,
    // so "we emailed them" would be untrue at this instant and flatly wrong for
    // a send that fails twenty seconds later — the outbox badge is the only
    // surface that claims an outcome. In BOTH states the copyable link keeps
    // its position and prominence: it is the fallback for a bounce, a spam
    // folder, a typo'd domain, and every hub with no mailer at all.
    return (
      <div className="rounded-lg border border-amber/30 bg-amber/10 px-3 py-2.5 text-xs text-amber">
        {mail ? (
          <div>
            Invitation created for <span className="font-semibold">{email.trim()}</span> — we're
            emailing them, and they'll see it when they sign in. If it doesn't arrive, send them this
            link:
          </div>
        ) : (
          <div>
            Invitation created for <span className="font-semibold">{email.trim()}</span> — they'll see it
            when they sign in. You can also send them this link:
          </div>
        )}
        {inviteUrl && (
          <div className="mt-2 flex items-stretch gap-1.5">
            <code className="min-w-0 flex-1 overflow-x-auto whitespace-nowrap rounded-md border border-amber/30 bg-bar/60 px-2 py-1.5 font-mono text-[11px] text-slate-200">
              {inviteUrl}
            </code>
            <button
              type="button"
              onClick={() => {
                void writeClipboard(inviteUrl).catch(() => {});
                setCopied(true);
                setTimeout(() => setCopied(false), 1500);
              }}
              /* min-h-11 + py (proposal 0073 Mobile/touch): `items-stretch`
                 alone left this ~28px tall, and it is the control the user
                 reaches for exactly when the mail didn't arrive. */
              className="min-h-11 shrink-0 rounded-md border border-amber/60 px-2.5 py-1.5 text-[11px] font-semibold text-amber transition hover:bg-amber/10"
            >
              {copied ? "Copied!" : "Copy link"}
            </button>
          </div>
        )}
        <button
          type="button"
          onClick={onClose}
          className="mt-2 min-h-11 rounded-md px-1.5 py-1 text-[11px] text-slate-400 transition hover:text-slate-200"
        >
          Done
        </button>
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
          {isLink ? "read-only" : isSession ? "view" : "use"}
        </span>
      </div>
      {isLink ? (
        <>
          {/* Read-only path echo ([0074] §D2): the user sees exactly what they
              are about to publish, and cannot edit it here — the path came from
              their own tree and the agent canonicalizes it at mint. */}
          <code className="block w-full overflow-x-auto whitespace-nowrap rounded-lg border border-edge bg-bar px-3 py-2.5 font-mono text-xs text-slate-300">
            ~/{subject.pathLabel}
          </code>
          <p className="mt-2 text-[11px] text-slate-500">
            Anyone with the URL can read this one file — no account needed, nothing else on{" "}
            {subject.machine || "this machine"} reachable, and it can’t be edited through the link.
            Revoke it any time.
          </p>
        </>
      ) : (
      <>
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
        // The whole label is the hit area, so min-h-11 on it (not the 14px box)
        // is what makes the checkbox reachable on a phone (0073 Mobile/touch).
        <label className="mt-2 flex min-h-11 cursor-pointer items-center gap-2 text-[11px] text-slate-400">
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
      </>
      )}
      {error && <div className="mt-2 text-[11px] text-claude">{error}</div>}
      <div className="mt-3 flex justify-end gap-2">
        <button
          type="button"
          onClick={onClose}
          className="min-h-11 rounded-md px-2.5 py-1.5 text-xs text-slate-400 transition hover:text-slate-200"
        >
          Cancel
        </button>
        <button
          type="submit"
          disabled={busy || (!isLink && !email.trim())}
          className="min-h-11 rounded-md bg-accent px-3 py-1.5 text-xs font-semibold text-bar transition hover:brightness-110 disabled:cursor-not-allowed disabled:opacity-40"
        >
          {busy ? "…" : isLink ? "Create link" : "Share"}
        </button>
      </div>
    </form>
  );
}
