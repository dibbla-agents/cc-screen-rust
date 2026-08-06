// Proposal 0041 §3 — the share-invite inbox. A self-contained header bell that
// polls for pending invitations, shows an unread count, and opens a small panel
// (desktop popover / mobile bottom sheet) of Accept / Decline rows. The inbox is
// the authoritative surface; a foreground toast (App) is just a nudge.
//
// Modeled on NotificationsButton's self-managed shape: drop it in a toolbar with
// a className and it handles its own state. Renders only in multi-tenant (the
// parent gates on me.multiTenant).

import { useCallback, useEffect, useRef, useState } from "react";
import {
  acceptInvite,
  declineInvite,
  listInbox,
  orgInviteInbox,
  respondOrgInvite,
  ApiError,
  type OrgInboxInvite,
  type ShareInvite,
} from "../api";
import { InboxIcon, XIcon } from "../icons";
import { ago } from "../util";

function subject(i: ShareInvite): string {
  const m = i.machine || "a machine";
  return i.session ? `${m} / ${i.session}` : m;
}

export default function InboxButton({
  className,
  isDesktop,
  onAccepted,
  onCountChange,
}: {
  className?: string;
  isDesktop: boolean;
  // Fired after an accept so the parent can refetch the roster (the shared
  // machine/session appears) and the received-shares feed (badges).
  onAccepted?: () => void;
  // Surfaced so the parent can fire a foreground toast on a rising edge.
  onCountChange?: (count: number) => void;
}) {
  const [invites, setInvites] = useState<ShareInvite[]>([]);
  // Pending TEAM invites (proposal 0065 C): same inbox, org-flavored rows with
  // the server's normative consent line. Empty on pre-0063 hubs (404 → []).
  const [orgInvites, setOrgInvites] = useState<OrgInboxInvite[]>([]);
  const [orgErr, setOrgErr] = useState<{ id: string; msg: string } | null>(null);
  const [open, setOpen] = useState(false);
  const [busyId, setBusyId] = useState<string | null>(null);
  const onCount = useRef(onCountChange);
  onCount.current = onCountChange;

  const reload = useCallback(async () => {
    try {
      const [list, orgs] = await Promise.all([
        listInbox(),
        orgInviteInbox().catch(() => [] as OrgInboxInvite[]),
      ]);
      setInvites(list);
      setOrgInvites(orgs);
      onCount.current?.(list.length + orgs.length);
    } catch {
      /* transient — keep the last list */
    }
  }, []);

  useEffect(() => {
    reload();
    const t = setInterval(reload, 15000);
    return () => clearInterval(t);
  }, [reload]);

  const count = invites.length + orgInvites.length;

  const act = async (id: string, accept: boolean) => {
    setBusyId(id);
    // Optimistic: drop the row immediately.
    setInvites((xs) => xs.filter((i) => i.id !== id));
    try {
      if (accept) {
        await acceptInvite(id);
        onAccepted?.();
      } else {
        await declineInvite(id);
      }
    } catch {
      reload(); // reconcile on failure
    } finally {
      setBusyId(null);
      onCount.current?.(Math.max(0, count - 1));
    }
  };

  // Accept/decline a TEAM invite. NOT optimistic: accept can fail meaningfully
  // (402 team out of seats, 409 already in another team) and the server's
  // message must be shown where the tap happened.
  const actOrg = async (id: string, accept: boolean) => {
    setBusyId(id);
    setOrgErr(null);
    try {
      await respondOrgInvite(id, accept);
      setOrgInvites((xs) => xs.filter((i) => i.id !== id));
      if (accept) onAccepted?.();
      onCount.current?.(Math.max(0, count - 1));
    } catch (e) {
      setOrgErr({ id, msg: e instanceof ApiError ? e.message : "Could not respond — try again." });
      reload();
    } finally {
      setBusyId(null);
    }
  };

  const panel = (
    <div
      className={
        isDesktop
          ? "absolute right-0 top-full z-50 mt-2 w-80 max-w-[calc(100vw-1rem)] overflow-hidden rounded-xl border border-edge bg-bar/98 shadow-2xl shadow-black/50 backdrop-blur-sm"
          : "fixed inset-x-0 bottom-0 z-50 max-h-[70vh] overflow-y-auto rounded-t-2xl border-t border-edge bg-bar/98 pb-safe shadow-2xl shadow-black/60 backdrop-blur-sm"
      }
      role="dialog"
      aria-label="Share invitations"
    >
      <div className="flex items-center justify-between border-b border-edge px-4 py-2.5">
        <span className="font-mono text-xs font-semibold text-slate-200">
          Invitations {count > 0 && <span className="text-accent">{count}</span>}
        </span>
        <button onClick={() => setOpen(false)} aria-label="Close" className="text-slate-500 hover:text-slate-300">
          <XIcon className="h-4 w-4" />
        </button>
      </div>
      {count === 0 ? (
        <div className="px-4 py-8 text-center text-xs text-slate-500">No pending invitations.</div>
      ) : (
        <ul className="divide-y divide-edge">
          {orgInvites.map((i) => (
            <li key={`org-${i.id}`} className="px-4 py-3">
              <div className="text-sm text-slate-100">
                <span className="text-slate-400">{i.inviterEmail || "Someone"}</span> invited you to
                join <span className="font-medium">{i.orgName || "a team"}</span>
              </div>
              <div className="mt-0.5 text-[11px] text-slate-500">
                as {i.role || "member"} · {ago(i.createdAt)} ago
              </div>
              {/* The normative consent copy (proposal 0063 B2) — the accept
                  must be informed, so the server's string renders verbatim. */}
              {i.consent && <p className="mt-1.5 text-[11px] text-amber">{i.consent}</p>}
              <div className="mt-2 flex justify-end gap-2">
                <button
                  disabled={busyId === i.id}
                  onClick={() => actOrg(i.id, false)}
                  className="min-h-11 rounded-md border border-edge px-2.5 py-1.5 text-xs text-slate-400 transition hover:border-claude hover:text-claude disabled:opacity-40"
                >
                  Decline
                </button>
                <button
                  disabled={busyId === i.id}
                  onClick={() => actOrg(i.id, true)}
                  className="min-h-11 rounded-md bg-accent px-3 py-1.5 text-xs font-semibold text-bar transition hover:brightness-110 disabled:opacity-40"
                >
                  Join team
                </button>
              </div>
              {orgErr?.id === i.id && <div className="mt-2 text-[11px] text-claude">{orgErr.msg}</div>}
            </li>
          ))}
          {invites.map((i) => (
            <li key={i.id} className="px-4 py-3">
              <div className="text-sm text-slate-100">
                <span className="text-slate-400">{i.inviterEmail || "Someone"}</span> shared{" "}
                <span className="font-medium">{subject(i)}</span>
              </div>
              <div className="mt-0.5 text-[11px] text-slate-500">
                {i.permission === "view" ? "can view" : "can use"} · {ago(i.createdAt)} ago
              </div>
              <div className="mt-2 flex justify-end gap-2">
                <button
                  disabled={busyId === i.id}
                  onClick={() => act(i.id, false)}
                  className="rounded-md border border-edge px-2.5 py-1.5 text-xs text-slate-400 transition hover:border-claude hover:text-claude disabled:opacity-40"
                >
                  Decline
                </button>
                <button
                  disabled={busyId === i.id}
                  onClick={() => act(i.id, true)}
                  className="rounded-md bg-accent px-3 py-1.5 text-xs font-semibold text-bar transition hover:brightness-110 disabled:opacity-40"
                >
                  Accept
                </button>
              </div>
            </li>
          ))}
        </ul>
      )}
    </div>
  );

  return (
    <div className="relative">
      <button
        onClick={() => setOpen((v) => !v)}
        aria-label={`Share invitations${count > 0 ? ` (${count} pending)` : ""}`}
        title="Share invitations"
        className={className}
      >
        <span className="relative inline-flex">
          <InboxIcon className={`h-4 w-4 ${count > 0 ? "text-accent" : ""}`} />
          {count > 0 && (
            <span
              aria-live="polite"
              className="absolute -right-1.5 -top-1.5 flex h-3.5 min-w-[0.875rem] items-center justify-center rounded-full bg-accent px-1 text-[9px] font-bold leading-none text-bar"
            >
              {count > 9 ? "9+" : count}
            </span>
          )}
        </span>
      </button>
      {open && (
        <>
          {/* click-away backdrop */}
          <div className="fixed inset-0 z-40" onClick={() => setOpen(false)} />
          {panel}
        </>
      )}
    </div>
  );
}
