// Thin client over the Go backend's four operations.

export interface Session {
  name: string;
  tool: string;
  short: string;
  attached: boolean;
  activity: number;
  last_input_at?: number;
  busy_since?: number;
  // Busy-window deadline. While working it's in the future; once ready it equals
  // the busy→ready transition instant and (unlike `activity`) is NOT bumped by a
  // cosmetic focus/resize repaint — so the ready timer/sort anchor to it for a
  // stable "ready for N" that doesn't reset on focus. 0/absent → fall back to
  // `activity` (proposal 0024).
  busy_until?: number;
  preview: string;
  // True when the session is ready / "your turn": not in an open, submit-armed
  // busy window. A user submit (Enter) arms busy; the agent's output sustains it;
  // it flips back to ready a grace window after output goes quiet — so cosmetic
  // repaints (focus/resize/spinner) never read as busy. Server-computed; see
  // engine.rs WORK_GRACE_SECS (proposal 0024).
  waiting: boolean;
  // The machine (agent) this session lives on, set by the hub when it aggregates
  // several agents. Absent/empty when talking to a single agent directly.
  machine?: string;
  // Whether the session launched in YOLO mode (approval prompts skipped).
  // Informational — drives a "YOLO" badge. `undefined` = unknown (pre-0005).
  skip_permissions?: boolean;
  // LLM-summarized status (proposal 0022). `headline` (≤6 words) replaces the
  // bare preview in dense surfaces; `detail` (2-3 sentences) is the tooltip /
  // status-view / push body. Absent until computed or when the feature is off —
  // every surface falls back to `preview`.
  headline?: string;
  detail?: string;
  // The session's live working directory (proposal 0025). The server already
  // computes and sends it (`handlers.rs` `live_cwd()` → `/proc/<pid>/cwd`,
  // falling back to the launch dir); it's omitted on the wire only when the
  // server genuinely can't read it. Drives the folder-breadcrumb label and the
  // tooltip's path row.
  cwd?: string;
  // Operator-chosen accent colour (proposal 0029): a curated palette token
  // (e.g. "rose"/"teal"), not a raw colour — util.ts owns the rendered shade via
  // sessionAccent(). Absent/empty = unmarked. Persisted on the agent so it
  // survives reload/restart and reaches every client (direct or hub-relayed).
  color?: string;
  // Operator-chosen display label (proposal 0035): shown in place of `short`
  // wherever the session is named. Display-only — identity (`name`/`short`) is
  // unchanged. Absent/empty = no label, fall back to `short`. Persisted on the
  // agent so it survives reload/restart and reaches every client (direct or
  // hub-relayed). Read it everywhere via `displayName(s)` (util.ts).
  label?: string;
}

// PaneRef is the identity the app stores for an open session: the session name
// plus the machine (agent) it lives on. We carry the machine rather than
// re-deriving it from the session name so a hub fronting several agents routes
// every request to the owning machine — and two machines with a same-named
// session never collide. `machine` is "" when talking to a single agent
// directly (no hub), which appends no query param anywhere downstream.
export interface PaneRef {
  name: string;
  machine: string;
}

// MachineInfo is one agent in the hub's roster (GET /api/machines). Used for the
// session-list grouping headers and the New-Session machine picker, so an
// offline or idle (zero-session) machine is still visible and targetable. A
// standalone agent has no such endpoint — fetchMachines() returns [] there.
export interface MachineInfo {
  machine: string;
  hostname: string;
  online: boolean;
}

// withMachine threads the owning agent onto a request URL — the single shared
// rule. The hub reads `?machine=` from the query on EVERY endpoint (even POSTs,
// which carry their data in the body), routing the relayed request to that
// agent. A non-empty machine is appended with the right separator (`?` or `&`
// depending on whether the URL already has a query); an empty machine
// (single-agent / no hub) appends nothing, so the URL is byte-identical to the
// pre-hub one and the standalone agent — which ignores the param anyway — is
// unaffected.
function withMachine(url: string, machine?: string): string {
  if (!machine) return url;
  const sep = url.includes("?") ? "&" : "?";
  return `${url}${sep}machine=${encodeURIComponent(machine)}`;
}

// ── Auth (opt-in password / API-token gate) ─────────────────────────────────
// The browser authenticates with a same-origin session cookie the server sets
// on login, so individual fetches and WebSockets need no extra headers — the
// cookie rides along automatically. We only (a) ask the server whether a gate
// is up and whether we're already in, and (b) notice when a request 401s (an
// expired cookie) so the app can drop back to the login screen.

export interface AuthStatus {
  authRequired: boolean;
  authed: boolean;
}

export async function getAuthStatus(): Promise<AuthStatus> {
  const r = await fetch("/api/auth");
  if (!r.ok) throw new Error(`auth: ${r.status}`);
  return r.json();
}

// login posts the password or API token; on success the server sets the 2-week
// session cookie. Returns true on success, false on a wrong secret.
export async function login(secret: string): Promise<boolean> {
  const r = await fetch("/api/login", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ secret }),
  });
  return r.ok;
}

export async function logout(): Promise<void> {
  await fetch("/api/logout", { method: "POST" });
}

// ── Multi-tenant (proposal 0001) ─────────────────────────────────────────────
// The web-UI boot read. `multiTenant` decides which login model to render;
// `googleEnabled` whether to show the Google button; the rest is the logged-in
// account when authenticated. Single-tenant returns multiTenant=false and the app
// falls back to the shared-secret /api/auth gate.
// The authenticated account's plan facts (proposal 0056 B1) — drives the
// LimitCard's copy and the dashboard's cheap cap preemption notice.
export interface MePlan {
  name: string;
  maxAgents: number;
  maxSessions: number;
  // How many machines the account currently has registered.
  agents: number;
  // Subscription status (proposal 0058 B1): absent = no subscription; "active"
  // paying; "past_due" payment failed but in grace (full Pro limits); "canceled"
  // downgraded. Only present on a billing-enabled hub.
  status?: "active" | "past_due" | "canceled";
  // Current billing period end, unix epoch seconds (proposal 0058). Drives the
  // "until <date>" note on a cancel-at-period-end subscription. Absent otherwise.
  periodEnd?: number;
  // ── Team plan fields (proposals 0064/0065 Part D) — present only when the
  // caller is a member of an ACTIVE org (`name === "team"`). The machine/session
  // caps above are then the POOLED org numbers; no client math needed.
  // Seats purchased on the org's subscription.
  seats?: number;
  // Current member count (the seats meter's numerator).
  members?: number;
  orgId?: string;
  orgName?: string;
  // The caller's role in the org: "owner" | "admin" | "member". Gates the
  // billing actions (portal is owner/admin).
  orgRole?: string;
  // The org owner's email — the "Billing is managed by …" caption for members.
  ownerEmail?: string;
}

// The caller's org membership (proposal 0063 B1), present on /api/me whenever
// they belong to one — even a dormant (0-seat) org. The minimal contract the
// TeamCard/AuditCard gate on; the full picture comes from GET /api/orgs/mine.
export interface MeOrg {
  id: string;
  name: string;
  role: "owner" | "admin" | "member";
  seats: number;
  memberCount: number;
}

export interface MeInfo {
  multiTenant: boolean;
  googleEnabled: boolean;
  /// Whether email+password login/signup is offered (false on a Google-only hub).
  passwordLogin?: boolean;
  authenticated: boolean;
  userId?: string;
  email?: string;
  // Present when authenticated on a multi-tenant hub (proposal 0056 B1).
  plan?: MePlan;
  // Present when the caller is an org member (proposal 0063 B1) — even while
  // the org is dormant (0 seats). Absent = no team, render nothing team-shaped.
  org?: MeOrg;
  // The operator's support address (CCHUB_SUPPORT_EMAIL) for the upgrade mailto.
  supportEmail?: string | null;
  // Whether Stripe billing is configured on this hub (proposal 0058 B4). Absent
  // or false on a self-hosted hub — the client renders the mailto fallback and
  // no checkout buttons. The JSON field name is exactly `billing`.
  billing?: boolean;
  // Whether this hub has a transactional mailer configured (proposal 0073 D1),
  // i.e. whether an invite is actually emailed. A per-hub CAPABILITY, identical
  // for every caller and saying nothing about any address — never an
  // account-existence oracle. Absent/false on a hub with no mailer, where the
  // copyable invite link stays the only channel. Present only in the
  // AUTHENTICATED /api/me body; the anonymous fallback omits it.
  mail?: boolean;
}

// An HTTP failure that keeps its status code, so callers can branch on e.g. a
// 402 plan-limit answer (proposal 0056 B2) without string-matching the message.
export class ApiError extends Error {
  status: number;
  constructor(status: number, message: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
  }
}

export async function getMe(): Promise<MeInfo> {
  const r = await fetch("/api/me");
  if (!r.ok) throw new Error(`me: ${r.status}`);
  return r.json();
}

// ── Billing (proposal 0058 Part B/C) ────────────────────────────────────────
// Only present when the hub reports `billing: true`. Both routes are cookie-
// authed and return a URL we navigate to full-page (never window.open/_blank —
// an installed PWA must return into scope; see 0058 Mobile/touch).

// The client-side price *choices* (never raw Stripe ids). The team prices
// (proposal 0064 B3) carry an org target + a seat quantity, clamped server-side
// to max(3, N, memberCount) — the client renders the floor, never enforces it.
export type CheckoutPrice = "pro-monthly" | "pro-annual" | "team-monthly" | "team-annual";

// startCheckout opens a Stripe Checkout session for the chosen price. The
// founder price is decided server-side, so the client always posts the plain
// choice string. Redirects on success; throws ApiError otherwise.
export async function startCheckout(
  price: CheckoutPrice,
  opts?: { org?: string; seats?: number }
): Promise<void> {
  const r = await fetch("/api/billing/checkout", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ price, org: opts?.org, seats: opts?.seats }),
  });
  if (!r.ok) throw new ApiError(r.status, (await r.text().catch(() => "")).trim() || `checkout: ${r.status}`);
  const { url } = (await r.json()) as { url: string };
  window.location.assign(url);
}

// openBillingPortal sends the subscriber to Stripe's hosted portal (update card,
// switch plan, cancel). 409 when the user has no billing account yet. Pass
// `{org}` to open the ORG's portal (owner/admin — where seat-quantity changes
// happen, proposal 0064 B3); omitted = today's personal path, byte-identical.
export async function openBillingPortal(opts?: { org?: string }): Promise<void> {
  const r = await fetch(
    "/api/billing/portal",
    opts?.org
      ? {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ org: opts.org }),
        }
      : { method: "POST" }
  );
  if (!r.ok) throw new ApiError(r.status, (await r.text().catch(() => "")).trim() || `portal: ${r.status}`);
  const { url } = (await r.json()) as { url: string };
  window.location.assign(url);
}

// Multi-tenant login: email + password (verified as the user's argon2 password).
export async function loginEmail(email: string, password: string): Promise<boolean> {
  const r = await fetch("/api/login", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ email, secret: password }),
  });
  return r.ok;
}

export async function signup(
  email: string,
  password: string
): Promise<{ ok: boolean; error?: string }> {
  const r = await fetch("/api/signup", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ email, password }),
  });
  if (r.ok) return { ok: true };
  const j = await r.json().catch(() => ({}) as { error?: string });
  return { ok: false, error: j.error || `Sign-up failed (${r.status})` };
}

export interface AgentInfo {
  agentId: string;
  machine: string;
  online: boolean;
  createdAt: number;
  // Tool prefixes whose CLI isn't installed on that machine (proposal 0050).
  // Absent on an older hub, and empty once everything's there — so the
  // dashboard's "N missing · Install" affordance simply doesn't render.
  missing?: string[];
}

export async function listAgents(): Promise<AgentInfo[]> {
  const r = await fetch("/api/agents");
  if (!r.ok) throw new Error(`agents: ${r.status}`);
  return r.json();
}

export async function unlinkAgent(agentId: string): Promise<boolean> {
  const r = await fetch("/api/agents/unlink", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ agent_id: agentId }),
  });
  return r.ok;
}

export async function rotateAgent(machine: string): Promise<string | null> {
  const r = await fetch("/api/agents/rotate", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ machine }),
  });
  if (!r.ok) return null;
  const j = await r.json().catch(() => ({}) as { token?: string });
  return j.token ?? null;
}

// Approve a headless box's device code (the /activate flow). The browser is the
// logged-in side; the server binds the pending enrollment to this tenant.
// A failure carries the server's human message and, for a 402, the `limit` flag
// so the page renders the plan-limit card instead of an error line (0056 B2).
export async function approveDevice(
  userCode: string
): Promise<{ ok: boolean; machine?: string; kind?: string; label?: string; limit?: boolean; error?: string }> {
  const r = await fetch("/api/device/approve", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ user_code: userCode }),
  });
  if (r.ok) {
    // `kind`/`label` (proposal 0060 B6) say WHAT was approved: a machine
    // enrollment ('agent') or a terminal sign-in ('client'). A pre-0060 hub
    // omits them — treated as the machine flow, exactly as before.
    const j = await r.json().catch(() => ({}) as { machine_id?: string; kind?: string; label?: string });
    return { ok: true, machine: j.machine_id, kind: j.kind ?? "agent", label: j.label ?? j.machine_id };
  }
  const text = (await r.text().catch(() => "")).trim();
  if (r.status === 404) return { ok: false, error: "Unknown or expired code" };
  return { ok: false, limit: r.status === 402, error: text || `Error ${r.status}` };
}

// ── Terminal clients (proposal 0060 B4): the account page's list + revoke ─────
export interface ClientTokenInfo {
  id: string;
  label: string;
  createdAt: number;
  lastUsedAt?: number | null;
}

export async function listClientTokens(): Promise<ClientTokenInfo[]> {
  const r = await fetch("/api/client-tokens");
  if (!r.ok) return [];
  return (await r.json().catch(() => [])) as ClientTokenInfo[];
}

export async function deleteClientToken(id: string): Promise<boolean> {
  const r = await fetch("/api/client-tokens/delete", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ id }),
  });
  return r.ok;
}

// ── Sharing (proposals 0039 permission model / 0040 invite lifecycle) ─────────
// An owner invites another user to a whole machine (agent) or a single session;
// the recipient accepts before any access takes effect. The client renders these
// shapes; the hub owns the lifecycle.

// One invite, as the inbox (received) and outbox (sent) endpoints return it.
export interface ShareInvite {
  id: string;
  inviterEmail?: string;
  granteeEmail?: string;
  resourceKind: "agent" | "session";
  agentId: string;
  machine?: string;
  session?: string | null;
  // [0039]'s grant level rendered as a friendly verb: "use" (agent) / "view" (session).
  permission: "use" | "view";
  ownerPeek: boolean;
  // "invited" (proposal 0056 Part C) = an email invite awaiting the address's
  // signup — outbox-only, revocable like a pending invite.
  status: "pending" | "accepted" | "declined" | "revoked" | "expired" | "invited";
  createdAt: number;
  expiresAt?: number | null;
  // The /invite/<token> link for an email-invite outbox row (0056 Part C).
  inviteUrl?: string;
}

// One active grant TO me (GET /api/shares/received) — drives the "shared with
// you" list and the shared-vs-owned badge.
export interface ReceivedShare {
  id: string;
  agentId: string;
  machine?: string;
  session?: string | null;
  // "team" (proposal 0065 A4): a machine-wide, view-level grant materialized
  // from org membership. Not individually leavable/revocable — the way out is
  // the owner's per-machine opt-out or leaving the team.
  kind: "agent" | "session" | "team";
  permission: "use" | "view";
  ownerEmail?: string;
  createdAt: number;
  // How the grant reached me (proposal 0065 Part B): "team" = materialized from
  // org membership; "direct"/absent = an explicit 0039 share. Old hubs omit it.
  origin?: "direct" | "team";
  // The granting org's name, on team rows only — the badge tooltip's text.
  orgName?: string | null;
}

// createShare offers an agent (no session) or a single session to a user by
// email. Resolves nothing until the recipient accepts. Any email works
// (proposal 0056 Part C): an address with no account gets a pending email
// invite that lands in their inbox on signup — the response shape is identical
// either way (no account-existence oracle), including a copyable invite link.
// Throws with the server's message on a bad request (not your resource,
// self-share) — an ApiError, so a 429 invite-cap answer is identifiable.
export async function createShare(args: {
  granteeEmail: string;
  machine: string;
  session?: string;
  ownerPeek?: boolean;
}): Promise<{ id: string; status: string; inviteUrl?: string }> {
  const r = await fetch("/api/shares", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      grantee_email: args.granteeEmail,
      machine: args.machine,
      session: args.session,
      owner_peek: args.ownerPeek ?? false,
    }),
  });
  if (!r.ok) throw new ApiError(r.status, (await r.text()).trim() || `share: ${r.status}`);
  const j = (await r.json()) as { id: string; status: string; invite_url?: string };
  return { id: j.id, status: j.status, inviteUrl: j.invite_url };
}

// The invite-link landing read (proposal 0056 C4): what /invite/<token> shows
// before login. Unauthenticated; a dead/unknown token throws an ApiError(404).
// When the caller IS signed in with the matching email, the server also
// attaches the invite to their inbox as a side effect.
export interface InviteInfo {
  email: string;
  inviterEmail: string;
  kind: "agent" | "session" | "team";
  // Team invites only (proposal 0065 C4, read via GET /api/org-invite/:token):
  // the org's name, the offered role, and the server's normative consent copy —
  // the landing MUST render `consent` verbatim (proposal 0063 B2).
  orgName?: string;
  role?: string;
  consent?: string;
}

export async function getInviteInfo(token: string): Promise<InviteInfo> {
  const r = await fetch(`/api/invite/${encodeURIComponent(token)}`);
  if (!r.ok) throw new ApiError(r.status, (await r.text().catch(() => "")).trim() || `invite: ${r.status}`);
  return r.json();
}

// listInbox — my pending, unexpired invitations (the things to accept/decline).
export async function listInbox(): Promise<ShareInvite[]> {
  const r = await fetch("/api/shares/inbox");
  if (!r.ok) throw new Error(`inbox: ${r.status}`);
  return r.json();
}

// listOutbox — invitations I've sent, across all statuses (the manage view).
export async function listOutbox(): Promise<ShareInvite[]> {
  const r = await fetch("/api/shares/outbox");
  if (!r.ok) throw new Error(`outbox: ${r.status}`);
  return r.json();
}

// listReceivedShares — the active shares granted to me ("shared with you").
export async function listReceivedShares(): Promise<ReceivedShare[]> {
  const r = await fetch("/api/shares/received");
  if (!r.ok) throw new Error(`received: ${r.status}`);
  return r.json();
}

// respondInvite — accept/decline a pending invite (idempotent server-side). A
// 409 means it's no longer pending (revoked/expired); the message is surfaced.
async function postShare(path: string): Promise<{ status: string }> {
  const r = await fetch(path, { method: "POST" });
  if (!r.ok) throw new Error((await r.text()).trim() || `share: ${r.status}`);
  return r.json().catch(() => ({ status: "ok" }));
}

export const acceptInvite = (id: string) => postShare(`/api/shares/${encodeURIComponent(id)}/accept`);
export const declineInvite = (id: string) => postShare(`/api/shares/${encodeURIComponent(id)}/decline`);
// revokeShare cancels an invite the caller sent (pre- or post-accept), removing
// any granted access. Forgiving server-side (double-revoke is success).
export const revokeShare = (id: string) => postShare(`/api/shares/${encodeURIComponent(id)}/revoke`);

// leaveShare gives back a share I hold (the "Leave" action); `id` is the
// received grant's id from listReceivedShares().
export async function leaveShare(id: string): Promise<void> {
  const r = await fetch(`/api/shares/received/${encodeURIComponent(id)}/leave`, { method: "POST" });
  if (!r.ok && r.status !== 204) throw new Error((await r.text()).trim() || `leave: ${r.status}`);
}

// ── Teams / orgs (proposals 0063 membership, 0065 team UX) ───────────────────
// One org per user in v1, so no org id appears in any path — the hub resolves
// the caller's org from their membership. Non-members get 404 on every org
// route (existence is never disclosed); role-forbidden actions get 403.

// The org core, as GET /api/orgs/mine returns it.
export interface OrgInfo {
  id: string;
  name: string;
  // Purchased seats. 0 = dormant (created but not activated) — members keep
  // their personal plans and nothing pools until seats exist.
  seats: number;
  memberCount: number;
  // The org subscription's status/period (proposal 0064) — absent pre-billing.
  planStatus?: string | null;
  periodEnd?: number | null;
}

export interface OrgMember {
  userId: string;
  email: string;
  role: "owner" | "admin" | "member";
  joinedAt: number;
}

// A pending org invite (the /mine outbox — owner/admin only, EMPTY for members).
export interface OrgInvite {
  id: string;
  email: string;
  role: string;
  status: string;
  createdAt: number;
  expiresAt?: number | null;
  inviteUrl?: string;
  // Mail-delivery state for this invite (proposal 0073 B2/D2). null/absent =
  // no send was attempted, which is the permanent and correct answer for every
  // row minted before the mailer existed and on every hub without one.
  // "sending" = claimed for an attempt; "sent" = the relay accepted it;
  // "failed" = transient failure (Resend is offered); "rejected" = a permanent
  // refusal (bad address — Resend is deliberately NOT offered).
  delivery?: "sending" | "sent" | "failed" | "rejected" | string | null;
}

// One of MY machines with the per-machine team-visibility opt-out flag (the
// consent surface, proposal 0063 §consent).
export interface OrgMachine {
  agentId: string;
  machine: string;
  teamVisible: boolean;
}

export interface OrgMine {
  org: OrgInfo;
  myRole: "owner" | "admin" | "member";
  members: OrgMember[];
  invites: OrgInvite[];
  machines: OrgMachine[];
}

// getOrg — the TeamCard's whole data read. Resolves null when the caller is in
// no org (the endpoint's 404); throws ApiError on real failures.
export async function getOrg(): Promise<OrgMine | null> {
  const r = await fetch("/api/orgs/mine");
  if (r.status === 404) return null;
  if (!r.ok) throw new ApiError(r.status, (await r.text().catch(() => "")).trim() || `org: ${r.status}`);
  return r.json();
}

// createOrg — start a team; the caller becomes its owner. The org is dormant
// (0 seats) until checkout activates it. 409 when already in a team.
export async function createOrg(name: string): Promise<{ id: string }> {
  const r = await fetch("/api/orgs", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ name }),
  });
  if (!r.ok) throw new ApiError(r.status, (await r.text().catch(() => "")).trim() || `org: ${r.status}`);
  return r.json();
}

// createOrgInvite — owner/admin invite by email. ONE response shape whether the
// address has an account or not (no account-existence oracle); the copyable
// /org-invite link is how the invite travels. 402 = out of seats (render the
// seats LimitCard); 403 for plain members.
export async function createOrgInvite(
  email: string,
  role?: "member" | "admin"
): Promise<{ id: string; status: string; inviteUrl?: string }> {
  const r = await fetch("/api/orgs/invites", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ email, role }),
  });
  if (!r.ok) throw new ApiError(r.status, (await r.text().catch(() => "")).trim() || `invite: ${r.status}`);
  const j = (await r.json()) as { id: string; status: string; invite_url?: string };
  return { id: j.id, status: j.status, inviteUrl: j.invite_url };
}

// revokeOrgInvite — owner/admin cancels a pending invite.
export async function revokeOrgInvite(id: string): Promise<void> {
  const r = await fetch(`/api/orgs/invites/${encodeURIComponent(id)}/revoke`, { method: "POST" });
  if (!r.ok) throw new ApiError(r.status, (await r.text().catch(() => "")).trim() || `invite: ${r.status}`);
}

// One row of MY pending org invites (GET /api/orgs/invites/inbox). `consent` is
// the server's normative copy and MUST be rendered on the accept surface.
export interface OrgInboxInvite {
  id: string;
  email: string;
  role: string;
  status: string;
  createdAt: number;
  expiresAt?: number | null;
  orgName?: string;
  inviterEmail?: string | null;
  consent?: string;
}

export async function orgInviteInbox(): Promise<OrgInboxInvite[]> {
  const r = await fetch("/api/orgs/invites/inbox");
  if (!r.ok) throw new ApiError(r.status, (await r.text().catch(() => "")).trim() || `inbox: ${r.status}`);
  return r.json();
}

// respondOrgInvite — accept/decline a pending org invite. Accept can answer
// 402 (team out of seats — surface the message / seats LimitCard) or 409
// (already in another team, or no longer pending).
export async function respondOrgInvite(id: string, accept: boolean): Promise<{ status: string }> {
  const r = await fetch(
    `/api/orgs/invites/${encodeURIComponent(id)}/${accept ? "accept" : "decline"}`,
    { method: "POST" }
  );
  if (!r.ok) throw new ApiError(r.status, (await r.text().catch(() => "")).trim() || `invite: ${r.status}`);
  return r.json().catch(() => ({ status: "ok" }));
}

// removeOrgMember — owner/admin removes a member (admins only remove members).
// Their team visibility is pruned server-side, both directions.
export async function removeOrgMember(userId: string): Promise<void> {
  const r = await fetch(`/api/orgs/members/${encodeURIComponent(userId)}/remove`, { method: "POST" });
  if (!r.ok && r.status !== 204) {
    throw new ApiError(r.status, (await r.text().catch(() => "")).trim() || `remove: ${r.status}`);
  }
}

// setOrgMemberRole — owner only. Role "owner" is an atomic ownership transfer.
export async function setOrgMemberRole(userId: string, role: string): Promise<void> {
  const r = await fetch(`/api/orgs/members/${encodeURIComponent(userId)}/role`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ role }),
  });
  if (!r.ok && r.status !== 204) {
    throw new ApiError(r.status, (await r.text().catch(() => "")).trim() || `role: ${r.status}`);
  }
}

// leaveOrg — 409 for the owner (transfer ownership first); the message is shown
// inline.
export async function leaveOrg(): Promise<void> {
  const r = await fetch("/api/orgs/leave", { method: "POST" });
  if (!r.ok && r.status !== 204) {
    throw new ApiError(r.status, (await r.text().catch(() => "")).trim() || `leave: ${r.status}`);
  }
}

// setMachineTeamVisible — the per-machine opt-out toggle (machine owner only;
// no admin override). Takes effect within the action, not a nightly pass.
export async function setMachineTeamVisible(agentId: string, visible: boolean): Promise<void> {
  const r = await fetch(`/api/orgs/machines/${encodeURIComponent(agentId)}/visibility`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ visible }),
  });
  if (!r.ok && r.status !== 204) {
    throw new ApiError(r.status, (await r.text().catch(() => "")).trim() || `visibility: ${r.status}`);
  }
}

// One audit-log line (proposal 0063 Part D). Owner/admin only; members get 404.
export interface OrgAuditEntry {
  id: number;
  at: number;
  actorEmail?: string | null;
  // Dotted vocabulary ("invite.created", "member.joined", …) — humanized
  // client-side; unknown actions fall back to the raw string.
  action: string;
  target?: string | null;
  // Small JSON blob (role, machine label…) — never terminal content.
  detail?: string | null;
}

// listOrgAudit — keyset-paged newest-first; pass the last row's id as `before`
// for the next page. Throws ApiError(404) for non-admin members (hide the card).
export async function listOrgAudit(before?: number): Promise<OrgAuditEntry[]> {
  const q = before !== undefined ? `?before=${encodeURIComponent(String(before))}` : "";
  const r = await fetch(`/api/orgs/audit${q}`);
  if (!r.ok) throw new ApiError(r.status, (await r.text().catch(() => "")).trim() || `audit: ${r.status}`);
  return r.json();
}

// getOrgInviteInfo — the /org-invite/<token> landing read (unauthenticated; the
// token IS the capability). A dead/unknown token throws ApiError(404).
export async function getOrgInviteInfo(token: string): Promise<InviteInfo> {
  const r = await fetch(`/api/org-invite/${encodeURIComponent(token)}`);
  if (!r.ok) throw new ApiError(r.status, (await r.text().catch(() => "")).trim() || `invite: ${r.status}`);
  return r.json();
}

let unauthorizedHandler: (() => void) | null = null;
// Register a callback fired when the heartbeat sees a 401 (cookie expired /
// logged out elsewhere). The app uses it to show the login screen again.
export function setUnauthorizedHandler(fn: (() => void) | null): void {
  unauthorizedHandler = fn;
}

export async function fetchSessions(): Promise<Session[]> {
  const r = await fetch("/api/sessions");
  if (r.status === 401) {
    unauthorizedHandler?.();
    throw new Error("unauthorized");
  }
  if (!r.ok) throw new Error(`sessions: ${r.status}`);
  return r.json();
}

// fetchMachines returns the hub's agent roster (id, hostname, online). Only the
// hub serves /api/machines; a standalone agent 404s, so we swallow any
// failure and return [] — the caller reads "[] ⇒ single machine, no hub", which
// keeps the UI ungrouped and machine-param-free for the common single-box case.
export async function fetchMachines(): Promise<MachineInfo[]> {
  try {
    const r = await fetch("/api/machines");
    if (!r.ok) return [];
    return await r.json();
  } catch {
    return [];
  }
}

// A session a reboot / tmux restart took down that the server recorded and can
// bring back, resuming the tool's prior conversation. (Restarting just the web
// daemon keeps sessions live, so this is empty in that case.)
export interface RestorableSession {
  session: string;
  tool: string;
  short: string;
  dir: string;
}

export async function fetchRestorable(machine?: string): Promise<RestorableSession[]> {
  const r = await fetch(withMachine("/api/sessions/restorable", machine));
  if (!r.ok) throw new Error(`restorable: ${r.status}`);
  return r.json();
}

export interface RestoreResult {
  restored: string[];
  failed?: Record<string, string>;
}

// restoreSessions recreates every restorable session, resuming each tool's
// conversation where possible (claude --continue, codex resume --last, …).
// Idempotent: already-live sessions are skipped.
export async function restoreSessions(machine?: string): Promise<RestoreResult> {
  const r = await fetch(withMachine("/api/sessions/restore", machine), { method: "POST" });
  if (!r.ok) throw new Error((await r.text()).trim() || `restore: ${r.status}`);
  return r.json();
}

// ── Coding-assistant updates (proposal 0049) ────────────────────────────────
// Updating the CLIs and restarting the sessions that use them is a *job*, not a
// request: it can take minutes, so the agent owns it and we read snapshots. The
// job lives on the agent, which is why closing the panel — or reloading the page
// — loses nothing.

export interface UpdateToolStatus {
  tool: string;
  label: string;
  // pending | updating | updated | current | failed | skipped
  state: string;
  from?: string;
  to?: string;
  message?: string;
}

export interface SessionRestartStatus {
  session: string;
  tool: string;
  // pending | stopping | starting | resumed | failed | skipped
  state: string;
  message?: string;
}

export interface UpdateJob {
  id?: string;
  // idle | updating | restarting | done
  phase: string;
  startedAt?: number;
  finishedAt?: number;
  tools?: UpdateToolStatus[];
  sessions?: SessionRestartStatus[];
  error?: string;
}

export const jobRunning = (j?: UpdateJob | null): boolean =>
  !!j && (j.phase === "updating" || j.phase === "restarting");

// fetchUpdateJob reads the current-or-last job on one machine. A hub answers
// 403 for a machine you don't own and 501 for an agent too old to self-update —
// both are surfaced as the row's reason, so the caller gets the message text.
export async function fetchUpdateJob(machine?: string): Promise<UpdateJob> {
  const r = await fetch(withMachine("/api/assistants/update", machine));
  if (!r.ok) throw new Error((await r.text()).trim() || `update status: ${r.status}`);
  return r.json();
}

// startAssistantUpdate kicks off the job. `restart` picks which sessions come
// back: "updated" (only those whose CLI actually changed — the default), "all",
// or "none". A 409 means one is already running; we return that job so the
// caller just watches it instead of racing a second one.
// `installMissing` (proposal 0050) additionally installs the CLIs that aren't on
// the machine at all, for the local user. A hub answers 501 when the agent is too
// old to install — never a silent update-only run.
export async function startAssistantUpdate(
  machine?: string,
  opts?: { tools?: string[]; restart?: "updated" | "all" | "none"; installMissing?: boolean }
): Promise<UpdateJob> {
  const r = await fetch(withMachine("/api/assistants/update", machine), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      tools: opts?.tools ?? [],
      restart: opts?.restart ?? "updated",
      installMissing: opts?.installMissing ?? false,
    }),
  });
  const text = (await r.text()).trim();
  if (r.status === 409) {
    // The body is the running job's snapshot.
    try {
      return JSON.parse(text) as UpdateJob;
    } catch {
      throw new Error("an update is already running on this machine");
    }
  }
  if (!r.ok) throw new Error(text || `update: ${r.status}`);
  return JSON.parse(text) as UpdateJob;
}

// ── Install plan (proposal 0050) ──────────────────────────────────────────────
// What installing the missing assistants on a machine WOULD do, fetched before
// the user confirms. The commands come from the AGENT's registry (including a
// machine's own cc_tool_install override), so the UI never hard-codes a vendor
// command. A pure probe — no side effects.

export interface InstallPrereqPlan {
  key: string;
  label: string;
  command: string;
  docs?: string;
  sizeHint?: string;
}

export interface InstallPlanItem {
  tool: string;
  label: string;
  command: string;
  docs?: string;
  sizeHint?: string;
  prereqs: InstallPrereqPlan[];
  // Set when this machine can't install it at all (no command for the platform,
  // or a prerequisite with no user-scope bootstrap — Node on Windows).
  unsupported?: string;
}

export interface InstallPlan {
  items: InstallPlanItem[];
}

export async function fetchInstallPlan(machine?: string): Promise<InstallPlan> {
  const r = await fetch(withMachine("/api/assistants/plan", machine));
  if (!r.ok) throw new Error((await r.text()).trim() || `install plan: ${r.status}`);
  return r.json();
}

// sendKey injects one named key (out-of-band; no focus needed). Names match
// the backend allow-list: up/down/left/right/enter/escape/tab/btab/space/
// backspace/home/end/pageup/pagedown/c-c/c-d/c-z/c-l/c-r.
export async function sendKey(session: string, key: string, machine?: string): Promise<void> {
  const r = await fetch(withMachine("/api/key", machine), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ session, key }),
  });
  if (!r.ok && r.status !== 204) throw new Error(`key: ${r.status}`);
}

// paste injects a (possibly multi-line) text block via bracketed paste, then
// optionally submits with Enter. This is the compose-sheet path.
export async function pasteText(
  session: string,
  text: string,
  enter: boolean,
  machine?: string
): Promise<void> {
  const r = await fetch(withMachine("/api/paste", machine), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ session, text, enter }),
  });
  if (!r.ok && r.status !== 204) throw new Error(`paste: ${r.status}`);
}

// clearHistory wipes the tmux scrollback for a session — the manual escape
// hatch for the re-render slideshow Claude Code leaves in scrollback whenever
// the pane is resized between clients of different widths (it writes to the
// normal buffer, so every redraw appends). The WS attach also auto-fires this
// on first connect when the client's reported cols differ from the pane's
// current width.
export async function clearHistory(session: string, machine?: string): Promise<void> {
  const r = await fetch(withMachine("/api/clear-history", machine), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ session }),
  });
  if (!r.ok && r.status !== 204) throw new Error(`clear-history: ${r.status}`);
}

// sendImage stages a PNG on the owning agent, which delivers it to the
// session's assistant using that tool's own paste contract (server-side
// dispatch — Claude reads it via the clipboard shim, Codex gets a staged file
// path pasted; proposal 0066). A 204 means staged + injected, not that the
// assistant has parsed it. Used for pasting phone screenshots.
export async function sendImage(session: string, png: Blob, machine?: string): Promise<void> {
  const r = await fetch(
    withMachine(`/api/clip?session=${encodeURIComponent(session)}`, machine),
    {
      method: "POST",
      headers: { "Content-Type": "image/png" },
      body: png,
    }
  );
  if (!r.ok && r.status !== 204) {
    const e = new Error(`clip: ${r.status}`) as Error & { status?: number };
    e.status = r.status;
    throw e;
  }
}

// Short actionable message for a failed image send; other statuses stay generic.
export function imageSendError(err: unknown): string {
  const status = (err as { status?: number } | null)?.status;
  switch (status) {
    case 413:
      return "Image too large — try a smaller screenshot.";
    case 422:
      return "That image couldn't be read — try re-copying it.";
    case 503:
      return "The session isn't accepting input — is it still running?";
    case 507:
      return "Image storage is full for this session — clean up old sessions and retry.";
    default:
      return "Image send failed — try again.";
  }
}

// A favourite is a saved prompt, stored server-side (durable + shared across
// devices) under ~/.config/cc-screen/favorites.json. The client owns CRUD and
// PUTs the whole list back; the server validates and persists it.
export interface Favorite {
  id: string;
  text: string;
}

export async function fetchFavorites(): Promise<Favorite[]> {
  const r = await fetch("/api/favorites");
  if (!r.ok) throw new Error(`favorites: ${r.status}`);
  return r.json();
}

// saveFavorites replaces the whole list and returns the server's sanitised
// version (blanks/dupes dropped, over-long trimmed) for the client to adopt.
export async function saveFavorites(list: Favorite[]): Promise<Favorite[]> {
  const r = await fetch("/api/favorites", {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(list),
  });
  if (!r.ok) throw new Error(`favorites save: ${r.status}`);
  return r.json();
}

export interface Tool {
  cmd: string;
  prefix: string;
  extraDirs?: {
    max?: number;
  };
  // This tool's CLI isn't installed on the machine (proposal 0046) — the picker
  // greys it out. Omitted (= falsy) by older agents and for available tools.
  unavailable?: boolean;
}

export interface DirEntry {
  name: string;
  path: string;
}

export interface DirsResp {
  path: string;
  home: string;
  atHome: boolean;
  parent: string;
  dirs: DirEntry[];
}

export async function fetchTools(machine?: string): Promise<Tool[]> {
  const r = await fetch(withMachine("/api/tools", machine));
  if (!r.ok) throw new Error(`tools: ${r.status}`);
  return r.json();
}

export async function fetchDirs(path?: string, machine?: string): Promise<DirsResp> {
  const q = path ? `?path=${encodeURIComponent(path)}` : "";
  const r = await fetch(withMachine(`/api/dirs${q}`, machine));
  if (!r.ok) throw new Error(`dirs: ${r.status}`);
  return r.json();
}

// One ranked hit from the recursive folder search (GET /api/dirs/search,
// proposal 0016). `rel` is the home-relative display path (~/development/foo);
// `depth` is how far below the search root it sits.
export interface DirSearchResult {
  path: string;
  name: string;
  rel: string;
  depth: number;
  score: number;
  mtime: number;
}

export interface DirsSearchResp {
  root: string;
  home: string;
  results: DirSearchResult[];
}

// searchDirs fuzzy-matches directories anywhere below `root` (default $HOME) on
// the chosen agent. Empty `q` returns no results — the caller falls back to
// fetchDirs + a recents shortcut. Per-agent like fetchDirs (the hub routes by
// ?machine=), so on a hub each agent searches its own $HOME.
export async function searchDirs(
  q: string,
  root?: string,
  machine?: string
): Promise<DirsSearchResp> {
  const params = new URLSearchParams();
  params.set("q", q);
  if (root) params.set("root", root);
  const r = await fetch(withMachine(`/api/dirs/search?${params.toString()}`, machine));
  if (!r.ok) throw new Error((await r.text()).trim() || `dirs search: ${r.status}`);
  return r.json();
}

// One ranked hit from the recursive file search (GET /api/files/search,
// proposal 0027). Like DirSearchResult but for files: `dir` is the home-relative
// parent directory (~/proj/docs) and `size` is the file's byte size.
export interface FileSearchResult {
  path: string;
  name: string;
  rel: string;
  dir: string;
  depth: number;
  score: number;
  mtime: number;
  size: number;
}

export interface FilesSearchResp {
  root: string;
  home: string;
  results: FileSearchResult[];
}

// searchFiles fuzzy-matches files (name-first, path second) anywhere below
// `root` on the chosen agent. The agent defaults `root` to the session's
// project when `session` is given, so the viewer's search is scoped to the
// session you're in. Per-agent like searchDirs (the hub routes by ?machine=);
// empty `q` returns no results — the caller only fires at ≥3 chars.
export async function searchFiles(
  q: string,
  opts?: { root?: string; session?: string; machine?: string }
): Promise<FilesSearchResp> {
  const params = new URLSearchParams();
  params.set("q", q);
  if (opts?.root) params.set("root", opts.root);
  if (opts?.session) params.set("session", opts.session);
  const r = await fetch(withMachine(`/api/files/search?${params.toString()}`, opts?.machine));
  if (!r.ok) throw new Error((await r.text()).trim() || `files search: ${r.status}`);
  return r.json();
}

export interface FileEntry {
  name: string;
  path: string;
  size: number;
  mtime: number; // unix seconds
}

export interface FilesResp {
  path: string;
  home: string;
  share: string;
  atHome: boolean;
  atShare: boolean;
  parent: string;
  dirs: DirEntry[];
  files: FileEntry[];
}

// fetchFiles lists subdirs + regular files under $HOME. Path resolution
// mirrors the backend:
//   - path given           => list that folder
//   - session given (no path) => list the session's tmux cwd (project root)
//   - neither              => list the share folder (CCWEB_SHARE_DIR or ~/cc-share/)
export async function fetchFiles(
  path?: string,
  session?: string,
  machine?: string
): Promise<FilesResp> {
  const params = new URLSearchParams();
  if (path) params.set("path", path);
  else if (session) params.set("session", session);
  const qs = params.toString();
  const r = await fetch(withMachine(`/api/files${qs ? `?${qs}` : ""}`, machine));
  if (!r.ok) throw new Error((await r.text()).trim() || `files: ${r.status}`);
  return r.json();
}

// downloadURL is the streaming download endpoint for a single file; the
// server attaches a Content-Disposition so the browser saves rather than
// renders.
export function downloadURL(path: string, machine?: string): string {
  return withMachine(`/api/download?path=${encodeURIComponent(path)}`, machine);
}

// inlineURL is the same file stream but served inline (Content-Disposition:
// inline) rather than as an attachment — the editor's PDF viewer points pdf.js
// at this so it can fetch + render the bytes (Range-supported) in place. See
// handleDownload's ?inline=1 branch.
export function inlineURL(path: string, machine?: string): string {
  return withMachine(`/api/download?inline=1&path=${encodeURIComponent(path)}`, machine);
}

// saveFileToDevice streams a file and hands it to navigator.share({files}) —
// the iOS PWA gold path: the system share sheet offers Save to Files, AirDrop,
// send to Photos. Falls back to a synthesised <a download> click for non-secure
// contexts (plain HTTP over tailnet) where canShare/share aren't available.
// Shared by the Files sheet and the PDF viewer's download button.
export async function saveFileToDevice(path: string, name: string, machine?: string): Promise<void> {
  const r = await fetch(downloadURL(path, machine));
  if (!r.ok) throw new Error(`download: ${r.status}`);
  const blob = await r.blob();
  const file = new File([blob], name, {
    type: blob.type || "application/octet-stream",
  });
  const nav = navigator as Navigator & {
    canShare?: (data: ShareData) => boolean;
  };
  if (nav.canShare?.({ files: [file] }) && nav.share) {
    try {
      await nav.share({ files: [file] });
      return;
    } catch (e) {
      if (e instanceof DOMException && e.name === "AbortError") return;
      // other share failures fall through to the download fallback
    }
  }
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

// --- File editor (markdown / text) ---
//
// The editor reads and writes text files under $HOME (same confinement as
// fetchFiles/downloadURL). Reads reject binaries and oversized files; writes
// are atomic server-side and use mtime to detect a concurrent change. See
// `web/server/editor.go`.

export interface FileReadResp {
  path: string;
  name: string;
  content: string;
  size: number;
  mtime: number; // unix seconds — echo back as baseMtime on save
}

export interface FileWriteResp {
  path: string;
  name: string;
  size: number;
  mtime: number;
}

// FileNotEditable is thrown by readFile when the server reports the file is
// binary (415). The editor catches this to fall back to download.
export class FileNotEditable extends Error {
  constructor() {
    super("file is not editable text");
    this.name = "FileNotEditable";
  }
}

// readFile loads a text file's contents for the editor. Throws FileNotEditable
// on a binary file (415), or a generic Error otherwise.
export async function readFile(path: string, machine?: string): Promise<FileReadResp> {
  const r = await fetch(withMachine(`/api/file/read?path=${encodeURIComponent(path)}`, machine));
  if (r.status === 415) throw new FileNotEditable();
  if (!r.ok) throw new Error((await r.text()).trim() || `read: ${r.status}`);
  return r.json();
}

// writeFile saves the editor's contents. Pass the baseMtime from the last read
// so the server can refuse (409) if the file changed on disk meanwhile; omit it
// (or pass 0) when creating a new file. A 409 throws FileChangedOnDisk.
export class FileChangedOnDisk extends Error {
  constructor() {
    super("file changed on disk");
    this.name = "FileChangedOnDisk";
  }
}

export async function writeFile(
  path: string,
  content: string,
  baseMtime?: number,
  machine?: string
): Promise<FileWriteResp> {
  const r = await fetch(withMachine("/api/file/write", machine), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ path, content, baseMtime: baseMtime ?? 0 }),
  });
  if (r.status === 409) throw new FileChangedOnDisk();
  if (!r.ok) throw new Error((await r.text()).trim() || `write: ${r.status}`);
  return r.json();
}

// deleteFile removes a single file under $HOME (the editor's "delete this
// file"). The server refuses directories — rmdir handles those.
export async function deleteFile(path: string, machine?: string): Promise<void> {
  const r = await fetch(withMachine("/api/file/delete", machine), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ path }),
  });
  if (!r.ok) throw new Error((await r.text()).trim() || `delete: ${r.status}`);
}

// makeDir creates a folder named `name` inside `dir` (both under $HOME).
export async function makeDir(dir: string, name: string, machine?: string): Promise<void> {
  const r = await fetch(withMachine("/api/mkdir", machine), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ dir, name }),
  });
  if (!r.ok) throw new Error((await r.text()).trim() || `mkdir: ${r.status}`);
}

// removeDir deletes a folder (under $HOME). By default only an empty folder is
// removed (non-empty -> error); pass recursive to delete the whole subtree
// (the file-tree context menu does, behind a confirm).
export async function removeDir(path: string, recursive = false, machine?: string): Promise<void> {
  const r = await fetch(withMachine("/api/rmdir", machine), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ path, recursive }),
  });
  if (!r.ok && r.status !== 204) throw new Error((await r.text()).trim() || `rmdir: ${r.status}`);
}

// renamePath renames a file or folder in place (same parent dir) to `name`.
// $HOME-confined server-side; refuses a path separator / leading dot, and a
// name that already exists (409). Returns the new {name, path}.
export async function renamePath(path: string, name: string, machine?: string): Promise<DirEntry> {
  const r = await fetch(withMachine("/api/rename", machine), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ path, name }),
  });
  if (!r.ok) throw new Error((await r.text()).trim() || `rename: ${r.status}`);
  return r.json();
}

// movePath relocates a file or folder INTO the directory `dest` (both under
// $HOME). Unlike renamePath (same-parent only), this is a cross-directory move.
// $HOME-confined + symlink-safe server-side; rejects a name collision at the
// destination (409) and moving a folder into itself/a descendant (400). Returns
// the new {name, path}.
export async function movePath(path: string, dest: string, machine?: string): Promise<DirEntry> {
  const r = await fetch(withMachine("/api/move", machine), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ path, dest }),
  });
  if (!r.ok) throw new Error((await r.text()).trim() || `move: ${r.status}`);
  return r.json();
}

// createSession launches a new cc-screen session (tool = cmd or prefix) in dir,
// named <prefix>-<name>. Returns the full session name, or throws with a
// message ("already exists" on 409) the UI can show.
export async function createSession(
  tool: string,
  name: string,
  dir: string,
  extraDirs: string[] = [],
  machine?: string,
  // Per-session launch policy (0005). Defaults to the agent's serde default so
  // omitting it reproduces today's behavior: YOLO on. (0014 retired the
  // hub-control switch — every session is editable through the hub.)
  skipPermissions = true
): Promise<PaneRef> {
  const r = await fetch(withMachine("/api/session", machine), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ tool, name, dir, extraDirs, skipPermissions }),
  });
  if (!r.ok) {
    const msg = (await r.text()).trim();
    // ApiError keeps the status so the create form can render the plan-limit
    // card for a 402 instead of the raw message (proposal 0056 B2).
    throw new ApiError(r.status, msg || `session: ${r.status}`);
  }
  const { name: session } = await r.json();
  // Return the owning machine alongside the name so the caller can mount the
  // pane with its full identity (machine is "" for a single agent / no hub).
  return { name: session, machine: machine ?? "" };
}

// deleteSession ends a session. "exit" injects the agent's /exit (graceful;
// the session dies asynchronously when the agent quits); "kill" tears it down
// immediately. Callers poll fetchSessions until the session is gone.
export async function deleteSession(
  session: string,
  mode: "exit" | "kill",
  machine?: string
): Promise<void> {
  const r = await fetch(withMachine("/api/session/delete", machine), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ session, mode }),
  });
  if (!r.ok && r.status !== 202 && r.status !== 204) {
    throw new Error((await r.text()).trim() || `delete: ${r.status}`);
  }
}

// setSessionColor marks a session with a curated palette token (proposal 0029),
// or clears the mark when `color` is null. The agent validates the token,
// persists it (survives restart), and returns the updated Session. Routed to the
// owning agent via `?machine=` exactly like key/paste/delete.
export async function setSessionColor(
  session: string,
  color: string | null,
  machine?: string
): Promise<Session> {
  const r = await fetch(withMachine("/api/session/color", machine), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ session, color }),
  });
  if (!r.ok) throw new Error((await r.text()).trim() || `color: ${r.status}`);
  return r.json();
}

// setSessionLabel sets a session's free-text display label (proposal 0035), or
// clears it when `label` is null/empty. The agent trims + length-caps it,
// persists it (survives restart), and returns the updated Session. Display-only:
// the identity `name`/`short` is untouched. Routed to the owning agent via
// `?machine=` exactly like setSessionColor.
export async function setSessionLabel(
  session: string,
  label: string | null,
  machine?: string
): Promise<Session> {
  const r = await fetch(withMachine("/api/session/label", machine), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ session, label }),
  });
  if (!r.ok) throw new Error((await r.text()).trim() || `rename: ${r.status}`);
  return r.json();
}

// wsURL builds the terminal WebSocket URL for a session, honouring the page's
// scheme (wss under tailscale serve / https). When talking to a hub, pass the
// session's `machine` so the hub routes to the owning agent; omitted/empty for a
// single agent (unchanged URL).
export function wsURL(session: string, machine?: string): string {
  const proto = location.protocol === "https:" ? "wss" : "ws";
  let url = `${proto}://${location.host}/api/ws?session=${encodeURIComponent(session)}`;
  if (machine) {
    url += `&machine=${encodeURIComponent(machine)}`;
  }
  return url;
}

// watchURL builds the filesystem-watch WebSocket URL (real-time tree + open-file
// updates), same scheme rule as wsURL.
export function watchURL(machine?: string): string {
  const proto = location.protocol === "https:" ? "wss" : "ws";
  return withMachine(`${proto}://${location.host}/api/watch`, machine);
}

// --- Drag-and-drop upload ---
//
// Drop files (and folders, via webkitGetAsEntry) onto a terminal pane; the
// UploadSheet then picks a destination inside the session's project root and
// streams everything through these endpoints. See `web/server/upload.go` for
// the backend and `AGENTS.md` for the moving-parts overview.

// sessionRoot returns the project root (tmux #{pane_current_path}) for a
// session. The destination picker in UploadSheet uses this to anchor and
// constrain its dir browser; the server enforces the same constraint
// on the upload itself, so a tampered client can't escape.
export async function sessionRoot(
  session: string,
  machine?: string
): Promise<{ root: string; home: string }> {
  const r = await fetch(
    withMachine(`/api/session/root?session=${encodeURIComponent(session)}`, machine)
  );
  if (!r.ok) throw new Error((await r.text()).trim() || `session root: ${r.status}`);
  return r.json();
}

// checkUpload asks the server which of `names` already exist in `dir` so the
// sheet can prompt for collision resolution up front. Names are relpaths
// (e.g. "src/foo.png"), matching what the upload itself will send. Used by the
// terminal-pane UploadSheet, which always has a session (destination confined
// to that session's project root). The editor file-tree drop uploads directly
// with no preflight, so it doesn't call this.
export async function checkUpload(
  session: string,
  dir: string,
  names: string[],
  machine?: string
): Promise<{ exists: string[] }> {
  const r = await fetch(withMachine("/api/upload/check", machine), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ session, dir, names }),
  });
  if (!r.ok) throw new Error((await r.text()).trim() || `upload check: ${r.status}`);
  return r.json();
}

// UploadFile is one entry from a drop, paired with its path relative to the
// drop root (so a dropped folder preserves its tree on the server).
export interface UploadFile {
  relPath: string;
  file: File;
}

// UploadMode is the per-file collision-resolution choice. "skip" is handled
// client-side (the file is omitted from the POST entirely); the server only
// ever sees "overwrite" / "rename".
export type UploadMode = "overwrite" | "rename" | "skip";

export interface UploadResult {
  written: string[];                 // relpaths actually written (renamed names if renamed)
  renamed: Record<string, string>;   // orig -> new (mode=rename + collision)
  errors?: Record<string, string>;   // per-file failure messages (rare)
}

// uploadFiles POSTs a multipart body to /api/upload. `modes` is the
// per-file mode map (defaults server-side to "rename" if missing).
// Files with mode "skip" are dropped before sending.
//
// `session` is the terminal-pane path (destination confined to that session's
// project root); null is the editor file-tree path (confined to $HOME). See
// uploadRoot in upload.go for the server-side confinement.
//
// Uses XMLHttpRequest (not fetch) for the `progress` event — fetch's
// streaming uploads (ReadableStream body) require HTTP/2 + secure context,
// which we don't have on plain HTTP. XHR gives us total-bytes progress on
// every platform. `onProgress` receives a 0..1 fraction.
export function uploadFiles(
  session: string | null,
  dir: string,
  files: UploadFile[],
  modes: Record<string, UploadMode>,
  onProgress?: (frac: number) => void,
  machine?: string
): Promise<UploadResult> {
  const fd = new FormData();
  // Manifest first so the server sees per-file modes before any file part.
  const manifest = {
    items: files
      .filter((f) => modes[f.relPath] !== "skip")
      .map((f) => ({
        name: f.relPath,
        mode: (modes[f.relPath] ?? "rename") as Exclude<UploadMode, "skip">,
      })),
  };
  fd.append("manifest", JSON.stringify(manifest));
  for (const f of files) {
    if (modes[f.relPath] === "skip") continue;
    // The 3rd arg becomes the multipart part's `filename` — crucial: we put
    // the full relpath here so a folder drop preserves its tree. (Go's
    // mime/multipart.Part.FileName() would normally strip subpaths via
    // filepath.Base; the server parses Content-Disposition manually to keep
    // them. See web/server/upload.go partFilename.)
    fd.append("file", f.file, f.relPath);
  }
  return new Promise<UploadResult>((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open(
      "POST",
      withMachine(
        `/api/upload?session=${encodeURIComponent(session ?? "")}&dir=${encodeURIComponent(dir)}`,
        machine
      )
    );
    xhr.responseType = "json";
    xhr.upload.onprogress = (ev) => {
      if (onProgress && ev.lengthComputable) onProgress(ev.loaded / ev.total);
    };
    xhr.onerror = () => reject(new Error("network error"));
    xhr.onabort = () => reject(new Error("upload aborted"));
    xhr.onload = () => {
      if (xhr.status >= 200 && xhr.status < 300) {
        resolve(xhr.response as UploadResult);
      } else {
        const msg =
          (typeof xhr.response === "string" && xhr.response) ||
          xhr.statusText ||
          `upload: ${xhr.status}`;
        reject(new Error(msg.trim()));
      }
    };
    xhr.send(fd);
  });
}

// flattenDataTransfer walks a drop's items, expanding folders into a flat
// list of {relPath, file}. Uses the venerable webkitGetAsEntry API which
// every shipping browser supports (Chrome/Edge/Safari/Firefox); the newer
// getAsFileSystemHandle is secure-context-only and would break our plain-
// HTTP tailnet deployment. Items that aren't files/dirs (URLs, plain text
// from another tab) are ignored — we want OS file drops only.
export async function flattenDataTransfer(dt: DataTransfer): Promise<UploadFile[]> {
  const out: UploadFile[] = [];
  // Snapshot items before any async hop — `dt` is invalidated as soon as
  // the drop handler returns, and items[] is a live list.
  const items = Array.from(dt.items);
  const entries: FsEntry[] = [];
  for (const it of items) {
    // webkitGetAsEntry is typed as returning `FileSystemEntry | null` in
    // the DOM lib, but we use our own narrower type below so the recursion
    // is typed without `any`.
    const entry = (it as DataTransferItem & {
      webkitGetAsEntry?: () => FsEntry | null;
    }).webkitGetAsEntry?.();
    if (entry) {
      entries.push(entry);
    } else if (it.kind === "file") {
      // No entry support — flat-file fallback (very old browsers).
      const f = it.getAsFile();
      if (f) out.push({ relPath: f.name, file: f });
    }
  }
  await Promise.all(entries.map((e) => walkEntry(e, "", out)));
  return out;
}

// Minimal FileSystemEntry interface for the recursion — keeps the file
// walk free of `any` while staying compatible with the legacy
// (webkit-prefixed) API the entries actually implement.
interface FsEntry {
  name: string;
  isFile: boolean;
  isDirectory: boolean;
  file?: (resolve: (f: File) => void, reject?: (e: unknown) => void) => void;
  createReader?: () => {
    readEntries: (
      resolve: (es: FsEntry[]) => void,
      reject?: (e: unknown) => void
    ) => void;
  };
}

function walkEntry(entry: FsEntry, prefix: string, out: UploadFile[]): Promise<void> {
  return new Promise((resolve) => {
    if (entry.isFile && entry.file) {
      entry.file(
        (f) => {
          out.push({ relPath: prefix + entry.name, file: f });
          resolve();
        },
        () => resolve()
      );
      return;
    }
    if (entry.isDirectory && entry.createReader) {
      const reader = entry.createReader();
      const collected: FsEntry[] = [];
      // readEntries returns ~100 entries per call; keep calling until empty.
      const readBatch = () => {
        reader.readEntries(
          (batch) => {
            if (batch.length === 0) {
              Promise.all(
                collected.map((e) => walkEntry(e, prefix + entry.name + "/", out))
              ).then(() => resolve());
              return;
            }
            collected.push(...batch);
            readBatch();
          },
          () => resolve()
        );
      };
      readBatch();
      return;
    }
    resolve();
  });
}
