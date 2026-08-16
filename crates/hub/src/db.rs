//! Multi-tenant store (proposal 0001), compiled only under the `multi-tenant`
//! feature. The persistence layer sits behind the [`Store`] trait so the backend
//! is **pluggable** (deviating from the proposal's Postgres-only assumption):
//! [`SqliteStore`] is the first backend; a Postgres backend can be added later as
//! a second impl of the same trait without touching any caller. Queries are
//! runtime `sqlx` (not the compile-checked `query!` macro), so the build needs no
//! `DATABASE_URL` and the SQL stays portable across backends.
//!
//! Phase 1a: `users` (argon2id password verify) + `agents` (the tenancy boundary)
//! + the `(machine_id, token) → (user_id, agent_id)` uplink resolution the relay
//! match (§4.1, Phase 1b) gates on. Phase 2 adds `device_enrollments`.

use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::Row;

/// The first backend's connection pool. SQLite (file-backed, zero-ops for dev and
/// small single-node installs); a Postgres pool becomes an alternate backend
/// behind the same [`Store`] trait in a later phase.
pub type Db = sqlx::SqlitePool;

/// The hub's persistence seam. A multi-tenant `HubState` holds an `Arc<dyn Store>`
/// (see [`crate::state::Tenancy::Multi`]); single-tenant holds none and behaves
/// exactly as today. Object-safe via `async_trait` (boxed futures) so it can be a
/// trait object.
#[async_trait::async_trait]
pub trait Store: Send + Sync {
    /// Resolve an inbound uplink `(machine_id, token)` to its owning
    /// `(user_id, agent_id)`, or `None` to reject. Multi-tenant is always gated —
    /// a token is required (there is no open mode). This is the multi-tenant
    /// counterpart of [`crate::state::StaticMap`]'s sync resolver.
    async fn resolve_agent(&self, machine_id: &str, token: Option<&str>) -> Option<(String, String)>;

    /// Verify an `email` + `password` login; `Some(user_id)` on success. OAuth-only
    /// accounts (`password_hash IS NULL`) never match the password path.
    async fn verify_login(&self, email: &str, password: &str) -> Option<String>;

    /// The account email for `GET /api/me`; `None` if the id is unknown.
    async fn user_email(&self, user_id: &str) -> Option<String>;

    /// Look up a `user_id` by email (for the hand-provisioning CLI); `None` if no
    /// such account.
    async fn user_id_by_email(&self, email: &str) -> Option<String>;

    /// Delete a user by email (and, via FK cascade, their agents). Returns true if
    /// a row was removed. Admin CLI only.
    async fn delete_user(&self, email: &str) -> bool;

    /// Create a user — backs both public signup and the `user add` admin CLI.
    /// Returns the new `user_id`. Errors on a duplicate email or a too-short
    /// password.
    async fn create_user(&self, email: &str, password: &str) -> anyhow::Result<String>;

    /// Resolve a Google sign-in to a local `user_id` (proposal 0001 §3.3), creating
    /// or linking as needed. Matches first on the stable `google_sub`; failing that
    /// links the (verified) `email` to an existing password account; otherwise
    /// creates a new OAuth-only user (`password_hash` NULL).
    async fn upsert_google_user(&self, google_sub: &str, email: &str) -> anyhow::Result<String>;

    /// Bind a new agent to a user, or rotate an existing `(user_id, machine_id)`'s
    /// token in place. Returns `(plaintext_token, agent_id)` — the plaintext is
    /// shown once; only its hash is stored. Used by tests now and the Phase 2
    /// device-approve handler later.
    async fn upsert_agent(&self, user_id: &str, machine_id: &str) -> anyhow::Result<(String, String)>;

    // ── RFC-8628 device flow (proposal 0001 §6–8; client kind: proposal 0060) ──
    /// Mint + store a pending enrollment for a headless host (`/api/device/code`).
    async fn device_create(&self, device_id: &str, machine_id: &str) -> anyhow::Result<DeviceCode>;

    /// Mint + store a pending **terminal-client** enrollment (proposal 0060 B2,
    /// `/api/device/client/code`). `label` is the client-suggested display name
    /// ("erik@orchid"); no device identity is needed — the flow is one-shot.
    async fn device_create_client(&self, label: &str) -> anyhow::Result<DeviceCode>;

    /// A host's poll (`/api/device/token` / `/api/device/client/token`). Handles
    /// lazy expiry, `slow_down` throttling, and single-use delivery of the
    /// approved token. `kind` scopes the lookup ('agent' | 'client') so a code
    /// minted for one flow can never be redeemed through the other's endpoint —
    /// a cross-kind poll answers `Expired` (uniform, non-leaking).
    async fn device_poll(&self, device_code: &str, kind: &str) -> DevicePoll;

    /// A logged-in browser approves a pending code (`/api/device/approve`), binding
    /// it to `user_id`, minting the credential (an agent uplink token or a 0060
    /// client token, per the row's `kind`), and parking the plaintext for the
    /// poller's next poll. Errors if the code is unknown/expired/already used.
    async fn device_approve(&self, user_id: &str, user_code: &str) -> anyhow::Result<DeviceApproval>;

    // ── terminal-client tokens (proposal 0060 B3/B4) ───────────────────────────
    /// Resolve a presented client token (by its sha256 hash) to its owning
    /// `user_id` — the Bearer half of the client auth gate. Touches
    /// `last_used_at`, throttled to ~1/min so the hot path stays read-mostly.
    async fn user_by_client_token_hash(&self, hash: &str) -> Option<String>;

    /// A user's minted client tokens (never the hashes' preimages), newest first —
    /// the account page's "Terminal clients" list.
    async fn list_client_tokens(&self, user_id: &str) -> Vec<ClientTokenRow>;

    /// Revoke one client token by row id, owner-scoped. True if a row went away;
    /// the next request with that token 401s.
    async fn delete_client_token(&self, user_id: &str, id: &str) -> bool;

    /// Revoke the client token whose hash this is (`ccs logout`'s server half —
    /// the token authenticates its own revocation, no cookie needed).
    async fn delete_client_token_by_hash(&self, hash: &str) -> bool;

    /// Reap expired (and approved-but-never-claimed) enrollments. Cheap; run on a
    /// timer.
    async fn device_sweep(&self);

    // ── dashboard (proposal 0001 Phase 3) ──────────────────────────────────────
    /// A user's registered agents, newest first.
    async fn list_agents(&self, user_id: &str) -> Vec<AgentRow>;

    /// Unlink (delete) one of a user's agents — scoped to the owner so a user can
    /// only ever remove their own. Returns true if a row was deleted. The agent's
    /// token instantly stops resolving; a live uplink keeps working until it drops,
    /// then can't reconnect (and self-re-enrolls if configured).
    async fn delete_agent(&self, user_id: &str, agent_id: &str) -> bool;

    // ── plan limits (proposal 0001 Phase 4) ────────────────────────────────────
    /// The caps for a user's plan (defaulting conservatively if unknown).
    async fn limits_for(&self, user_id: &str) -> PlanLimits;
    /// How many agents a user currently has registered.
    async fn agent_count(&self, user_id: &str) -> i64;
    /// Whether the user already has an agent labelled `machine_id` (so a re-enroll
    /// / rotate doesn't count against the cap).
    async fn has_machine(&self, user_id: &str, machine_id: &str) -> bool;
    /// Assign a plan to a user by email (admin CLI). Errors if no such user, or the
    /// plan isn't one of the `plan_limits` rows.
    async fn set_plan(&self, email: &str, plan: &str) -> anyhow::Result<()>;

    // ── billing (proposal 0058 Part B) ─────────────────────────────────────────
    /// Record a Stripe event id for exactly-once processing (`INSERT … ON
    /// CONFLICT(id) DO NOTHING`). Returns `rows_affected`: 1 for the first
    /// delivery, 0 for a replay. Standalone (reconcile/tests); the webhook uses
    /// [`Store::billing_process_event`] so the insert shares the state change's
    /// transaction.
    async fn billing_event_insert(&self, id: &str, payload_hash: &str, received_at: i64) -> anyhow::Result<u64>;

    /// `(billing_customer_id, billing_subscription_id)` for a user — the checkout
    /// (reuse a known customer) and portal (require one) handlers read this.
    async fn billing_ids(&self, user_id: &str) -> (Option<String>, Option<String>);

    /// `(plan_status, current_period_end)` for a user — surfaced by `/api/me`
    /// (proposal 0058 B4). `(None, None)` when never subscribed.
    async fn billing_status(&self, user_id: &str) -> (Option<String>, Option<i64>);

    /// Resolve a Stripe customer id (`cus_…`) to its local `user_id` via the
    /// `idx_users_billing_customer` index. `None` if unmapped (a checkout event or
    /// reconcile heals that).
    async fn user_by_billing_customer(&self, customer_id: &str) -> Option<String>;

    /// The single billing writer (proposal 0058 B3). Applies current subscription
    /// truth to `users`, guarded like [`Store::set_plan`] so a stale plan can never
    /// strand a user:
    /// - `plan = Some("pro")`: activate/keep the paid plan; stamps
    ///   `prior_plan = COALESCE(prior_plan, <current plan>)` on first activation.
    /// - `plan = None`: restore `COALESCE(prior_plan,'free')` (the terminal /
    ///   cancel path); `prior_plan` is never touched.
    ///
    /// `subscription_id = None` clears the stored id (cancel); `customer_id = None`
    /// leaves the stored customer id untouched (kept for resubscribe). Used by the
    /// nightly reconcile; the webhook goes through [`Store::billing_process_event`].
    async fn apply_subscription_state(
        &self,
        user_id: &str,
        plan: Option<&str>,
        status: &str,
        subscription_id: Option<&str>,
        customer_id: Option<&str>,
        period_end: Option<i64>,
    ) -> anyhow::Result<()>;

    /// Atomic webhook processing (proposal 0058 B3): ONE transaction = the
    /// idempotency insert (`INSERT … ON CONFLICT DO NOTHING`) + the optional state
    /// change. Returns `Duplicate` (ack, no change) when the event id was already
    /// recorded; `Applied` otherwise. On any failure the whole transaction rolls
    /// back — including the idempotency insert — so Stripe's retry reprocesses.
    async fn billing_process_event(
        &self,
        id: &str,
        payload_hash: &str,
        received_at: i64,
        apply: Option<SubApply>,
    ) -> anyhow::Result<EventOutcome>;

    /// Every user carrying billing state, for the nightly reconcile (both
    /// directions). Small; runs once a day.
    async fn billing_rows_for_reconcile(&self) -> Vec<BillingRow>;

    // ── sharing grants (proposal 0039) ─────────────────────────────────────────
    /// Every `shares` row relevant to building `user_id`'s [`Visibility`]: those
    /// granting INTO them (`grantee_user_id = user_id`) and those they issued OUT of
    /// agents they own (`agents.user_id = user_id`), each joined to its owner. The
    /// hot path — one load per gated request.
    async fn visibility_rows(&self, user_id: &str) -> Vec<ShareRow>;

    /// Grant the whole agent to a grantee (idempotent on the agent share row;
    /// re-issuing flips `owner_peek`). Owner-scoped: errors unless `owner_user_id`
    /// owns `agent_id`; rejects a self-share. Returns the share id.
    async fn share_agent(&self, owner_user_id: &str, agent_id: &str, grantee_user_id: &str, owner_peek: bool) -> anyhow::Result<String>;

    /// Grant view of a single session to a grantee (idempotent). Owner-scoped;
    /// rejects a self-share. Returns the share id.
    async fn share_session(&self, owner_user_id: &str, agent_id: &str, grantee_user_id: &str, session: &str) -> anyhow::Result<String>;

    /// Every grant OUT of an owner's agents (for their "shared by me" view).
    async fn shares_by_owner(&self, owner_user_id: &str) -> Vec<ShareRow>;

    /// Revoke one grant. Owner-scoped: the DELETE joins agents so a user can only
    /// revoke a share on an agent they own. Returns true if a row went away.
    async fn revoke_share(&self, owner_user_id: &str, share_id: &str) -> bool;

    // ── share invites (proposal 0040) ──────────────────────────────────────────
    /// Create or re-invite (upsert on `(grantee, resource)`): refresh a
    /// pending/terminal row back to `pending` with a fresh TTL, or no-op an already
    /// `accepted` one. Owner-scoped (the caller must own `agent_id`); rejects a
    /// self-invite. Returns `(invite_id, status)`.
    async fn share_create(
        &self,
        inviter: &str,
        grantee: &str,
        kind: &str,
        agent_id: &str,
        session: Option<&str>,
        owner_peek: bool,
    ) -> anyhow::Result<(String, String)>;

    /// A grantee's pending, unexpired invites (the inbox feed), newest first.
    async fn share_inbox(&self, grantee: &str) -> Vec<ShareInviteRow>;

    /// An inviter's sent invites across all statuses (the manage/cancel view).
    async fn share_outbox(&self, inviter: &str) -> Vec<ShareInviteRow>;

    /// The grantee accepts (`accept=true`) or declines a pending invite. Accepting
    /// also **materialises** the 0039 grant; the transition is idempotent (§4).
    async fn share_respond(&self, grantee: &str, id: &str, accept: bool) -> ShareOutcome;

    /// The inviter revokes an invite (cancel, pre- or post-accept). Removes any
    /// materialised grant. Idempotent and forgiving (§4).
    async fn share_revoke(&self, inviter: &str, id: &str) -> ShareOutcome;

    /// Background reap (§7): flip overdue `pending` rows to `expired` and
    /// hard-delete long-dead terminal rows. Cheap; runs on the device-sweep timer.
    async fn share_sweep(&self);

    /// Look up one invite by id (for the handler to render/notify after a mutation).
    async fn share_get(&self, id: &str) -> Option<ShareInviteRow>;

    /// The active grants *to* this user — accepted shares others have made to them
    /// (proposal 0041's "shared with you" + the shared-vs-owned badge feed). Reads
    /// the 0039 `shares` table joined to its agents' owners.
    async fn shares_to_me(&self, grantee: &str) -> Vec<ShareRow>;

    /// The grantee gives back a share they hold (the "Leave" action). Grantee-
    /// scoped by the `shares` row id; removes the grant and settles the matching
    /// invite to `declined`. Returns true if a grant went away.
    async fn leave_grant(&self, grantee: &str, share_id: &str) -> bool;

    // ── read-only link grants (proposal 0083 Part C) ──────────────────────────
    // Their own table, deliberately: see `migrations/0014_link_shares.sql`. A
    // link grant has no grantee, so it can never enter `Visibility`, an inbox,
    // or an accept path — that inertness is structural here, not a filter.

    /// Mint a link grant. `token_hash` is the SHA-256 of the bearer token (the
    /// plaintext never reaches the store); `path` is already canonicalized by
    /// the owning agent. Owner-scoped: errors unless `owner_user_id` owns
    /// `agent_id`. Returns the new grant id.
    async fn link_share_create(
        &self,
        owner_user_id: &str,
        agent_id: &str,
        path: &str,
        name: &str,
        token_hash: &str,
        expires_at: Option<i64>,
    ) -> anyhow::Result<String>;

    /// Resolve a bearer token hash to its grant — the ONLY read path for
    /// `/api/link/:token`. Returns `None` for unknown **or expired**, so the
    /// handler cannot accidentally tell those apart.
    async fn link_share_by_token_hash(&self, token_hash: &str) -> Option<LinkShareRow>;

    /// The owner's live link grants, newest first (the outbox rows).
    async fn link_share_outbox(&self, owner_user_id: &str) -> Vec<LinkShareRow>;

    /// Revoke one grant. Owner-scoped; true if a row went away.
    async fn link_share_revoke(&self, owner_user_id: &str, id: &str) -> bool;

    /// Replace a grant's token in place: the old URL dies at this instant, and
    /// the id, path and expiry are unchanged. Owner-scoped; true if it applied.
    async fn link_share_regenerate(&self, owner_user_id: &str, id: &str, token_hash: &str) -> bool;

    // ── email invites (proposal 0056 Part C) ───────────────────────────────────
    /// Create (or re-offer, upserting on `(email, resource)`) an email invite —
    /// the pre-account row that converts into a `share_invites` row when an
    /// account with this email appears. `converted` pre-stamps `converted_at`
    /// (used for known-user invites, which only need the row for its link token).
    /// Owner-scoped; caps live (unconverted, unexpired) invites per inviter —
    /// over the cap errors with the `CAP:` prefix so the handler answers 429.
    /// Returns `(invite_id, link_token)`.
    #[allow(clippy::too_many_arguments)]
    async fn email_invite_create(
        &self,
        inviter: &str,
        email: &str,
        kind: &str,
        agent_id: &str,
        session: Option<&str>,
        owner_peek: bool,
        converted: bool,
    ) -> anyhow::Result<(String, String)>;

    /// Convert every live (unconverted, unexpired) email invite for `email` into
    /// a normal `share_invites` row for `user_id` (the new account), stamping
    /// `converted_at`. Called from signup + the Google callback + the invite-info
    /// endpoint. Returns how many attached.
    async fn attach_email_invites(&self, user_id: &str, email: &str) -> usize;

    /// Look up a live (unexpired) email invite by its link token — the
    /// `/api/invite/:token` read. `None` for unknown/expired/revoked.
    async fn email_invite_by_token(&self, token: &str) -> Option<EmailInviteRow>;

    /// The inviter cancels an unconverted email invite by id (the outbox
    /// fall-through when `share_revoke` finds no `share_invites` row). Returns
    /// true if a row went away.
    async fn email_invite_revoke(&self, inviter: &str, id: &str) -> bool;

    /// The inviter's live (unconverted, unexpired) email invites — rendered in
    /// the outbox with status `"invited"` and the email as the counterpart.
    async fn email_invite_outbox(&self, inviter: &str) -> Vec<EmailInviteRow>;

    /// Would this exact create/re-offer trip the live-invite cap? The **pre-flight**
    /// `share::create` runs before it branches on whether the address has an
    /// account (proposal 0073 C1). It exists because the known-account arm calls
    /// `email_invite_create` *after* `share_create` has already written a row: a
    /// cap discovered there could only be swallowed, which would make the bound
    /// reachable in the unknown arm alone — an account-existence oracle ([0042]).
    /// Same inputs, same counting rule, and the store still enforces it.
    #[allow(clippy::too_many_arguments)]
    async fn email_invite_cap_exceeded(
        &self,
        inviter: &str,
        email: &str,
        kind: &str,
        agent_id: &str,
        session: Option<&str>,
    ) -> bool;

    // ── orgs (proposal 0063) ───────────────────────────────────────────────────
    /// Create an org with `creator` as its owner (one transaction: orgs insert +
    /// org_members owner row + audit append). Errors if the creator is already in
    /// an org (the v1 one-org constraint).
    async fn org_create(&self, creator: &str, name: &str) -> anyhow::Result<String>;
    /// The caller's org, if any: (org row, my role). One indexed lookup — the
    /// org-aware limits_for (0063 C1) and every org handler start here.
    async fn org_for_user(&self, user_id: &str) -> Option<(OrgRow, String)>;
    /// One org by id.
    async fn org_get(&self, org_id: &str) -> Option<OrgRow>;
    /// CLI lookup: by exact id first, else by (unique) name.
    async fn org_by_name_or_id(&self, key: &str) -> Option<OrgRow>;
    /// All members of an org, joined to users for email display.
    async fn org_members(&self, org_id: &str) -> Vec<OrgMemberRow>;
    /// Member ids only — the pooled counters (0063 C2/C3) consume this.
    async fn org_member_ids(&self, org_id: &str) -> Vec<String>;
    /// Role change (owner-only at the handler; `owner` target transfers ownership
    /// atomically: exactly one owner row exists at all times).
    async fn org_set_role(&self, org_id: &str, user_id: &str, role: &str) -> anyhow::Result<()>;
    /// Remove a member / leave. Refuses to remove the owner (transfer first —
    /// the error carries the `OWNER:` prefix so the handler answers 409).
    async fn org_remove_member(&self, org_id: &str, user_id: &str) -> anyhow::Result<()>;
    /// Hand-set the seat count (`cc-screen-hub org seats`, 0063 B4) — the
    /// pre-billing path; Stripe writes it via the webhook/reconcile otherwise.
    async fn org_set_seats(&self, org_id: &str, seats: i64) -> anyhow::Result<()>;

    // Org invites — the share_invites/email_invites shape (0005/0006), org-flavored.
    /// Create or re-invite (upsert on `(org, email)`): refresh a pending/terminal
    /// row back to `pending` with a fresh TTL + fresh token; an `accepted` row
    /// (already a member) is a no-op success. Returns `(id, status, token)`.
    async fn org_invite_create(&self, org_id: &str, inviter: &str, email: &str, role: &str)
        -> anyhow::Result<(String, String, String)>;
    /// Pending, unexpired org invites addressed to this email (the inbox feed).
    async fn org_invite_inbox(&self, email: &str) -> Vec<OrgInviteRow>;
    /// An org's sent invites across all statuses (the manage/cancel view).
    async fn org_invite_outbox(&self, org_id: &str) -> Vec<OrgInviteRow>;
    /// accept=true inserts the org_members row guarded by the seat gate (0064 B5)
    /// and the one-org constraint, in the same transaction as the status flip.
    /// `Err` carries `SEATS:` (→ 402) or `ONEORG:` (→ 409) prefixed messages.
    async fn org_invite_respond(&self, user_id: &str, id: &str, accept: bool)
        -> anyhow::Result<ShareOutcome>;
    async fn org_invite_revoke(&self, org_id: &str, id: &str) -> ShareOutcome;
    async fn org_invite_by_token(&self, token: &str) -> Option<OrgInviteRow>;
    /// Called from the main.rs sweep loop beside `share_sweep` (0063 B2).
    async fn org_invite_sweep(&self);

    // ── invite delivery (proposal 0073) ───────────────────────────────────────
    /// Claim a pending invite for a send attempt: stamps `delivery = 'sending'`
    /// only if the row is still `pending` **and still carries this token**. Returns
    /// false when a revoke or a re-invite has superseded the attempt, in which case
    /// the caller must not send. The token is the liveness check (B1).
    async fn org_invite_mark_sending(&self, id: &str, token: &str) -> bool;
    /// Stamp the outcome, again guarded on the token. Best-effort: a write failure
    /// is logged and dropped — losing the receipt must never fail the invitation.
    async fn org_invite_delivery_record(&self, id: &str, token: &str, delivery: &str);
    /// The `email_invites` twin. Liveness there is the row's continued existence
    /// carrying this token: `email_invite_revoke` DELETEs and a re-offer mints a
    /// fresh token on the same row, so `(id, token)` is the same claim check.
    async fn email_invite_mark_sending(&self, id: &str, token: &str) -> bool;
    /// The `email_invites` twin of [`Store::org_invite_delivery_record`].
    async fn email_invite_delivery_record(&self, id: &str, token: &str, delivery: &str);
    /// Fail-stamp attempts the process lost. The hub has no graceful shutdown, so
    /// a restart drops in-flight send tasks; without this a `sending` row would be
    /// indistinguishable from "never attempted" forever. Runs on the 60s sweep
    /// timer beside `org_invite_sweep`; only rows older than
    /// [`DELIVERY_STUCK_AFTER`] are touched, so a live attempt is never stolen.
    async fn invite_delivery_sweep(&self);

    /// How many invitations this **hub** has emailed since `since` (both invite
    /// tables). The send ceiling that actually maps to reality: the relay's quota
    /// is per *account*, not per org (Brevo's free tier is 300/day across the
    /// whole account), so N orgs each under a per-org ceiling can still exhaust it
    /// — after which every invitation silently fails to leave the building.
    async fn invites_emailed_since(&self, since: i64) -> i64;

    /// How many invitations attributable to one org have been emailed since
    /// `since`: its own org invites plus the share invites its members sent. Two
    /// indexed COUNTs in one statement.
    async fn org_invites_emailed_since(&self, org_id: &str, since: i64) -> i64;

    // Machine opt-out (0063 §consent) — owner-scoped like delete_agent.
    async fn set_team_visible(&self, owner_user_id: &str, agent_id: &str, visible: bool) -> bool;

    // Audit (0063 Part D). Fire-and-forget: never fails the mutation it records.
    async fn audit_append(&self, org_id: &str, actor: Option<&str>, action: &str,
                          target: Option<&str>, detail: Option<&str>);
    /// Keyset-paged read, newest first (`before` = an audit row id).
    async fn audit_page(&self, org_id: &str, before: Option<i64>, limit: i64) -> Vec<AuditRow>;

    // Pooled counters (0063 Part C).
    /// Machines owned by any of these users (one indexed COUNT over agents).
    async fn agent_count_for_users(&self, user_ids: &[String]) -> i64;

    /// Is this user in the founder cohort — `plan` OR `prior_plan` is 'beta'?
    /// The Team founder-price gate reads the ORG OWNER's signal through this
    /// (0064 B3: a founder who already bought Pro keeps the Team offer).
    async fn user_founder_cohort(&self, user_id: &str) -> bool;

    // ── org billing (proposal 0064) ────────────────────────────────────────────
    /// Resolve a Stripe customer id to its org (via idx_orgs_billing_customer).
    async fn org_by_billing_customer(&self, customer_id: &str) -> Option<String>;
    /// Every org carrying billing state, for the nightly reconcile.
    async fn org_billing_rows_for_reconcile(&self) -> Vec<OrgBillingRow>;
    /// Apply a subscription state to its target (user or org) — the reconcile's
    /// writer; the webhook goes through [`Store::billing_process_event`].
    async fn apply_sub(&self, apply: &SubApply) -> anyhow::Result<()>;

    // ── materialized team shares (proposal 0065 Part A) ────────────────────────
    /// Is this share row a materialized team grant? (share.rs's leave/revoke
    /// refusal — team rows are managed by membership, not individually.)
    async fn share_is_team(&self, share_id: &str) -> bool;
    /// Set-based, idempotent: one `kind='team'` row per (fellow member, visible
    /// machine) pair for this org (INSERT OR IGNORE over idx_shares_team).
    async fn team_shares_materialize(&self, org_id: &str);
    /// The inverse DELETE: every team row of this org no longer implied by
    /// current membership + opt-out flags.
    async fn team_shares_prune(&self, org_id: &str);
    /// Nightly invariant-restorer: materialize + prune for every org.
    async fn team_shares_reconcile(&self);
}

/// One `share_invites` row (proposal 0040).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareInviteRow {
    pub id: String,
    pub inviter_user_id: String,
    pub grantee_user_id: String,
    pub resource_kind: String,
    pub agent_id: String,
    pub session_name: Option<String>,
    pub owner_peek: bool,
    pub status: String,
    pub created_at: i64,
    pub responded_at: Option<i64>,
    pub expires_at: Option<i64>,
}

/// One `link_shares` row (proposal 0083 Part C) — a read-only, revocable grant
/// on ONE file, held by whoever has the URL.
///
/// The token is **not** in this struct and never leaves the store: only its
/// SHA-256 is persisted, and nothing needs the plaintext after the mint call
/// returns it once. Re-viewing a link later shows this metadata and offers
/// *Regenerate* — which is the answer to the re-viewability question [0073]
/// said hashing tokens was blocked on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkShareRow {
    pub id: String,
    pub agent_id: String,
    pub owner_user_id: String,
    /// Absolute path on the agent, canonicalized agent-side at mint.
    pub path: String,
    /// Basename at mint time — a display copy; `path` is the identity.
    pub name: String,
    pub created_at: i64,
    /// NULL/None = until revoked.
    pub expires_at: Option<i64>,
}

/// One `email_invites` row (proposal 0056 Part C) — a pre-account invitation
/// keyed by email, plus the link token the inviter hands out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailInviteRow {
    pub id: String,
    pub token: String,
    pub inviter_user_id: String,
    pub email: String,
    pub resource_kind: String,
    pub agent_id: String,
    pub session_name: Option<String>,
    pub owner_peek: bool,
    pub created_at: i64,
    pub expires_at: i64,
    pub converted_at: Option<i64>,
    /// When the last delivery attempt settled (proposal 0073 B2). NULL = no send
    /// was attempted — the permanent answer on a hub with no mailer.
    pub emailed_at: Option<i64>,
    /// `sending|sent|rejected|failed`, or NULL when nothing was attempted.
    pub delivery: Option<String>,
}

/// One `orgs` row (proposal 0063), including the 0064 billing mirror columns.
/// `seat_count > 0` is the "active org" gate: a dormant org (never subscribed /
/// canceled / no CLI seats) confers nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgRow {
    pub id: String,
    pub name: String,
    pub plan: String,
    pub created_at: i64,
    pub seat_count: i64,
    pub billing_customer_id: Option<String>,
    pub billing_subscription_id: Option<String>,
    pub plan_status: Option<String>,
    pub current_period_end: Option<i64>,
}

/// One org member, joined to users for email display (proposal 0063 A2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgMemberRow {
    pub user_id: String,
    pub email: String,
    pub role: String,
    pub created_at: i64,
}

/// One `org_invites` row (proposal 0063 B2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgInviteRow {
    pub id: String,
    pub token: String,
    pub org_id: String,
    pub inviter_user_id: String,
    pub email: String,
    pub role: String,
    pub status: String,
    pub created_at: i64,
    pub responded_at: Option<i64>,
    pub expires_at: i64,
    /// When the last delivery attempt settled (proposal 0073 B2). NULL = no send
    /// was attempted — the permanent answer on a hub with no mailer.
    pub emailed_at: Option<i64>,
    /// `sending|sent|rejected|failed`, or NULL when nothing was attempted.
    pub delivery: Option<String>,
}

/// One `audit_log` row (proposal 0063 Part D).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditRow {
    pub id: i64,
    pub org_id: String,
    pub at: i64,
    pub actor_user_id: Option<String>,
    pub action: String,
    pub target: Option<String>,
    pub detail: Option<String>,
}

/// One org's billing mirror, for the nightly reconcile (proposal 0064 B6).
/// `member_count` rides along so the reconcile can log over-seat orgs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgBillingRow {
    pub org_id: String,
    pub customer_id: Option<String>,
    pub subscription_id: Option<String>,
    pub plan: String,
    pub plan_status: Option<String>,
    pub seat_count: i64,
    pub member_count: i64,
}

/// The result of an invite transition — mirrors [`DevicePoll`]'s shape so handlers
/// map it to `200`/`409`/`404` without re-deriving the state-machine rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShareOutcome {
    /// The transition succeeded (or was a no-op); the row is in this status.
    Ok(String),
    /// The target row is in a terminal state that forbids this transition.
    Conflict,
    /// No such invite, or it isn't the caller's to act on (don't leak existence).
    NotFound,
}

/// One `shares` row joined to its agent's owner — re-exported from `registry` so
/// the (feature-gated) store and the (always-compiled) visibility predicate share
/// one type.
pub use crate::registry::ShareRow;

/// What a host's poll of `/api/device/token` resolves to (RFC 8628). Mirrors the
/// `authorization_pending` / `slow_down` / `expired_token` / `access_denied` /
/// success outcomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DevicePoll {
    Pending,
    SlowDown,
    Expired,
    Denied,
    Approved { token: String, agent_id: String },
    /// A `kind='client'` enrollment came back approved (proposal 0060): the
    /// one-time client-token handover plus the account email (echoed so the
    /// terminal can confirm *which* account it signed into).
    ApprovedClient { token: String, email: String },
}

/// What `/api/device/approve` bound (proposal 0060 B6): the enrollment's kind
/// ('agent' | 'client') and its display label (the machine name resp. the
/// client label), so the /activate page can say what was just approved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceApproval {
    pub kind: String,
    pub label: String,
}

/// One row of a user's minted terminal-client tokens (proposal 0060 B4). Only
/// metadata — the token itself exists in plaintext exactly once, at handover.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ClientTokenRow {
    pub id: String,
    pub label: String,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
}

/// A plan's enforced caps (proposal 0001 Phase 4). Resolved from `plan_limits`
/// joined on `users.plan`; falls back to [`PlanLimits::default`] if the plan row
/// is missing.
// `Eq` is dropped from the derive (proposal 0058 A2): `summary_user_budget_usd`
// is an `f64`, which isn't `Eq`. Only tests compare `PlanLimits`, and they assert
// individual fields.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanLimits {
    /// The plan's name (`users.plan`), surfaced by `/api/me` (proposal 0056 B1).
    pub plan: String,
    pub max_agents: i64,
    pub max_concurrent_sessions: i64,
    /// Whether this plan may CREATE shares/invites (proposal 0058 A3). Receiving
    /// shares is never gated. `DEFAULT 1` on the column keeps hand-tuned custom
    /// plans at the capability they effectively had.
    pub can_create_shares: bool,
    /// Per-plan per-user summarizer ceiling in USD (proposal 0058 A4). `None` =
    /// fall back to `CCHUB_SUMMARY_USER_BUDGET`; the config value is also a hard
    /// cap (min of plan + config when both are set).
    pub summary_user_budget_usd: Option<f64>,
}
impl Default for PlanLimits {
    fn default() -> Self {
        // The NEW free row (proposal 0058 A2), load-bearing: a missing/unknown
        // plan must never out-entitle free, so the default caps and capabilities
        // are exactly free's (2 machines, 5 sessions, no sharing, $0.25 summary
        // budget), never something more generous.
        PlanLimits {
            plan: "free".into(),
            max_agents: 2,
            max_concurrent_sessions: 5,
            can_create_shares: false,
            summary_user_budget_usd: Some(0.25),
        }
    }
}

/// The result of atomic webhook processing (proposal 0058 B3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventOutcome {
    /// The event id was new; the state change (if any) was applied.
    Applied,
    /// The event id was already recorded; nothing changed (a replay). Ack 200.
    Duplicate,
}

/// Which entity a subscription entitles (proposal 0064 B4): a user (Pro, the
/// 0058 shape) or an org (Team). One enum, not a parallel struct, so
/// [`Store::billing_process_event`]'s single-transaction contract covers both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubTarget {
    User(String),
    Org(String),
}

/// The subscription state to apply in one webhook transaction (proposal 0058 B3,
/// org-targeted per 0064 B4). `plan = None` is the terminal/cancel path — users
/// restore `COALESCE(prior_plan,'free')`; orgs zero their `seat_count` (the org
/// `plan` column stays `'team'`; a zero-seat org confers nothing, 0063 C1).
#[derive(Debug, Clone)]
pub struct SubApply {
    pub target: SubTarget,
    pub plan: Option<String>,
    pub status: String,
    pub subscription_id: Option<String>,
    pub customer_id: Option<String>,
    pub period_end: Option<i64>,
    /// Org targets: the mirrored seat quantity (`items.data[0].quantity`).
    /// `None` keeps the stored count. Ignored for user targets.
    pub seat_count: Option<i64>,
}

impl SubApply {
    /// Convenience for the 0058-era user-target tests and reconcile paths.
    pub fn user_id(&self) -> Option<&str> {
        match &self.target {
            SubTarget::User(u) => Some(u),
            SubTarget::Org(_) => None,
        }
    }
}

/// One user's billing mirror, for the nightly reconcile (proposal 0058 B5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BillingRow {
    pub user_id: String,
    pub customer_id: Option<String>,
    pub subscription_id: Option<String>,
    pub plan: String,
    pub plan_status: Option<String>,
}

/// One of a user's registered agents, for the dashboard (proposal 0001 Phase 3).
/// Live online status is annotated by the hub from its registry, not stored here.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentRow {
    pub agent_id: String,
    pub machine_id: String,
    pub created_at: i64,
    /// May this machine be visible to the owner's org (proposal 0063 §consent)?
    /// Owner-toggled; read by 0065's materializer. Meaningless without an org.
    pub team_visible: bool,
}

/// The minted codes a host receives from `/api/device/code`.
#[derive(Debug, Clone)]
pub struct DeviceCode {
    /// Opaque secret the host polls with.
    pub device_code: String,
    /// Short human code shown on the host, grouped `WXYZ-MJHT` for display.
    pub user_code_display: String,
    /// Seconds the host must wait between polls.
    pub interval: i64,
    /// Seconds until the code expires.
    pub expires_in: i64,
}

/// The SQLite-backed [`Store`].
pub struct SqliteStore {
    pool: Db,
}

impl SqliteStore {
    /// Open (creating the file if missing) and run forward-only migrations.
    pub async fn connect(url: &str) -> anyhow::Result<Self> {
        let opts = SqliteConnectOptions::from_str(url)?.create_if_missing(true);
        let pool = SqlitePoolOptions::new().max_connections(5).connect_with(opts).await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Self { pool })
    }

    /// The raw pool — for admin tooling and integration tests that need to seed
    /// rows (e.g. a squeezed `plan_limits` plan) the trait doesn't expose.
    pub fn pool(&self) -> &Db {
        &self.pool
    }

    /// Materialise an accepted invite into a 0039 `shares` grant — the row the
    /// visibility predicate reads. Reuses the owner-scoped grant inserts, so this
    /// re-validates ownership and the no-self-share rule.
    async fn materialise_grant(&self, inv: &ShareInviteRow) -> anyhow::Result<()> {
        match inv.session_name.as_deref() {
            Some(s) => self.share_session(&inv.inviter_user_id, &inv.agent_id, &inv.grantee_user_id, s).await.map(|_| ()),
            None => self.share_agent(&inv.inviter_user_id, &inv.agent_id, &inv.grantee_user_id, inv.owner_peek).await.map(|_| ()),
        }
    }

    /// Remove the 0039 grant an invite materialised (on revoke). Natural-keyed, so
    /// it's idempotent and needs no stored grant id; a no-op if none exists.
    async fn strip_grant(&self, inv: &ShareInviteRow) {
        let _ = sqlx::query(
            "DELETE FROM shares
              WHERE agent_id = ?1 AND grantee_user_id = ?2 AND kind = ?3
                AND ((?4 IS NULL AND session IS NULL) OR session = ?4)",
        )
        .bind(&inv.agent_id)
        .bind(&inv.grantee_user_id)
        .bind(&inv.resource_kind)
        .bind(inv.session_name.as_deref())
        .execute(&self.pool)
        .await;
    }

    /// The shared insert behind both `device_create` (kind 'agent') and
    /// `device_create_client` (kind 'client'): one pending enrollment row, one
    /// code vocabulary, one approve page.
    async fn device_insert(&self, device_id: &str, machine_id: &str, kind: &str) -> anyhow::Result<DeviceCode> {
        let device_code = cc_screen_auth::generate_token();
        let display = gen_user_code();
        let stored = normalize_user_code(&display);
        let expires_at = now_secs() as i64 + DEVICE_CODE_TTL;
        sqlx::query(
            "INSERT INTO device_enrollments
               (device_code, user_code, device_id, machine_id, status, interval, expires_at, kind)
             VALUES (?1, ?2, ?3, ?4, 'pending', ?5, ?6, ?7)",
        )
        .bind(&device_code)
        .bind(&stored)
        .bind(device_id)
        .bind(machine_id)
        .bind(DEVICE_POLL_INTERVAL)
        .bind(expires_at)
        .bind(kind)
        .execute(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!("device_create: {e}"))?;
        Ok(DeviceCode {
            device_code,
            user_code_display: display,
            interval: DEVICE_POLL_INTERVAL,
            expires_in: DEVICE_CODE_TTL,
        })
    }

    /// Guard a share insert: the agent must exist and be owned by `owner_user_id`,
    /// and a share to oneself is rejected (a no-op — the owner already sees all).
    async fn assert_owns(&self, owner_user_id: &str, agent_id: &str, grantee_user_id: &str) -> anyhow::Result<()> {
        anyhow::ensure!(owner_user_id != grantee_user_id, "cannot share with yourself");
        self.assert_owns_agent(owner_user_id, agent_id).await
    }

    /// The ownership half of [`Self::assert_owns`], for callers with no grantee
    /// user yet (email invites, proposal 0056 Part C).
    async fn assert_owns_agent(&self, owner_user_id: &str, agent_id: &str) -> anyhow::Result<()> {
        let owner: Option<String> = sqlx::query("SELECT user_id FROM agents WHERE id = ?1")
            .bind(agent_id)
            .fetch_optional(&self.pool)
            .await?
            .and_then(|r| r.try_get("user_id").ok());
        anyhow::ensure!(owner.as_deref() == Some(owner_user_id), "not your agent");
        Ok(())
    }

    /// The id of the email-invite row a create for this `(email, kind, agent,
    /// session)` would upsert onto, if any — the re-offer target. Shared by
    /// `email_invite_create` and the cap pre-flight so both agree on which row is
    /// being refreshed (and therefore which one to exclude from the count).
    async fn email_invite_existing_id(
        &self,
        email: &str,
        kind: &str,
        agent_id: &str,
        session: Option<&str>,
    ) -> Option<String> {
        sqlx::query(
            "SELECT id FROM email_invites
              WHERE email = ?1 AND agent_id = ?2 AND resource_kind = ?3
                AND ((?4 IS NULL AND session_name IS NULL) OR session_name = ?4)",
        )
        .bind(email)
        .bind(agent_id)
        .bind(kind)
        .bind(session)
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten()
        .and_then(|r| r.try_get("id").ok())
    }

    /// Live (unconverted, unexpired) email invites for `inviter`, ignoring
    /// `exclude_id` (pass `""` for none). Rides `idx_email_invites_inviter`.
    async fn email_invite_live_excluding(&self, inviter: &str, exclude_id: &str) -> i64 {
        sqlx::query(
            "SELECT count(*) AS n FROM email_invites
              WHERE inviter_user_id = ?1 AND converted_at IS NULL AND expires_at > ?2
                AND id <> ?3",
        )
        .bind(inviter)
        .bind(now_secs() as i64)
        .bind(exclude_id)
        .fetch_one(&self.pool)
        .await
        .ok()
        .and_then(|r| r.try_get::<i64, _>("n").ok())
        .unwrap_or(0)
    }

    /// The org-scoped live-invite cap (proposal 0073 C1). Deliberately **not**
    /// per-inviter, unlike its `email_invites` sibling: `org_invites` has no index
    /// on `inviter_user_id` (0010 indexes only `email`), so a per-inviter COUNT
    /// table-scans; the re-offer UPDATE *reassigns* `inviter_user_id`, so a
    /// per-inviter count isn't even stable across re-offers; and an org-scoped cap
    /// rides the existing `UNIQUE (org_id, email)` index, matches the abuse model
    /// (the org is what has the billing relationship) and cannot be reset by
    /// inviting from a second admin account.
    ///
    /// `exclude_id` is the row a re-offer is about to refresh (`""` for a fresh
    /// insert): a re-offer of an already-pending row doesn't grow the live set, so
    /// it must never be blocked by the cap it is itself a member of.
    async fn assert_org_invite_headroom(&self, org_id: &str, exclude_id: &str) -> anyhow::Result<()> {
        let live: i64 = sqlx::query(
            "SELECT count(*) AS n FROM org_invites
              WHERE org_id = ?1 AND status = 'pending' AND expires_at > ?2 AND id <> ?3",
        )
        .bind(org_id)
        .bind(now_secs() as i64)
        .bind(exclude_id)
        .fetch_one(&self.pool)
        .await
        .ok()
        .and_then(|r| r.try_get::<i64, _>("n").ok())
        .unwrap_or(0);
        anyhow::ensure!(
            live < ORG_INVITE_CAP,
            "CAP:this team has too many pending invitations — cancel some from the team page first"
        );
        Ok(())
    }

    /// One `plan_limits` row by name (the org branch of `limits_for` reads the
    /// per-seat 'team' row through this). `None` if unseeded.
    async fn plan_row(&self, plan: &str) -> Option<PlanLimits> {
        sqlx::query(
            "SELECT plan, max_agents, max_concurrent_sessions, can_create_shares,
                    summary_user_budget_usd
               FROM plan_limits WHERE plan = ?1",
        )
        .bind(plan)
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten()
        .and_then(|r| {
            Some(PlanLimits {
                plan: r.try_get("plan").ok()?,
                max_agents: r.try_get("max_agents").ok()?,
                max_concurrent_sessions: r.try_get("max_concurrent_sessions").ok()?,
                can_create_shares: r.try_get::<i64, _>("can_create_shares").ok()? != 0,
                summary_user_budget_usd: r
                    .try_get::<Option<f64>, _>("summary_user_budget_usd")
                    .ok()
                    .flatten(),
            })
        })
    }

    /// An org invite row by id (internal to the respond/revoke transitions).
    async fn org_invite_get(&self, id: &str) -> Option<OrgInviteRow> {
        let row = sqlx::query("SELECT * FROM org_invites WHERE id = ?1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .ok()??;
        org_invite_row(&row)
    }

    #[cfg(test)]
    pub(crate) async fn in_memory() -> Self {
        // One connection so the `:memory:` db is shared across the pool's calls.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(SqliteConnectOptions::from_str("sqlite::memory:").unwrap())
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        Self { pool }
    }
}

#[async_trait::async_trait]
impl Store for SqliteStore {
    async fn resolve_agent(&self, machine_id: &str, token: Option<&str>) -> Option<(String, String)> {
        let token = token?;
        let hash = cc_screen_auth::sha256_hex(token);
        let row = sqlx::query("SELECT id, user_id FROM agents WHERE machine_id = ?1 AND token_hash = ?2")
            .bind(machine_id)
            .bind(&hash)
            .fetch_optional(&self.pool)
            .await
            .ok()??;
        let agent_id: String = row.try_get("id").ok()?;
        let user_id: String = row.try_get("user_id").ok()?;
        Some((user_id, agent_id))
    }

    async fn verify_login(&self, email: &str, password: &str) -> Option<String> {
        let email = normalize_email(email);
        let row = sqlx::query("SELECT id, password_hash FROM users WHERE email = ?1")
            .bind(&email)
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten();
        let (id, hash) = match row {
            Some(row) => {
                let id: String = row.try_get("id").ok()?;
                let hash: Option<String> = row.try_get("password_hash").ok()?;
                (Some(id), hash)
            }
            None => (None, None),
        };
        match (id, hash) {
            (Some(id), Some(hash)) => verify_password(password, &hash).then_some(id),
            // Unknown email or an OAuth-only account: burn a comparable Argon2
            // verify against a fixed baked hash so response timing doesn't leak
            // account existence (proposal 0053 Part E / 0042 candidate #5).
            _ => {
                dummy_verify(password);
                None
            }
        }
    }

    async fn user_email(&self, user_id: &str) -> Option<String> {
        let row = sqlx::query("SELECT email FROM users WHERE id = ?1")
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await
            .ok()??;
        row.try_get("email").ok()
    }

    async fn user_id_by_email(&self, email: &str) -> Option<String> {
        let email = normalize_email(email);
        let row = sqlx::query("SELECT id FROM users WHERE email = ?1")
            .bind(&email)
            .fetch_optional(&self.pool)
            .await
            .ok()??;
        row.try_get("id").ok()
    }

    async fn delete_user(&self, email: &str) -> bool {
        sqlx::query("DELETE FROM users WHERE email = ?1")
            .bind(normalize_email(email))
            .execute(&self.pool)
            .await
            .map(|r| r.rows_affected() > 0)
            .unwrap_or(false)
    }

    async fn create_user(&self, email: &str, password: &str) -> anyhow::Result<String> {
        let email = normalize_email(email);
        // Signup validates too (proposal 0073 C3, tightened after review): the
        // account's own address is interpolated into every invitation's subject
        // as `inviter`, and signup never verified — nor even parsed — it, so an
        // account created as `a@b.com\r\nBcc: x@y` would have fed the mailer
        // from a path the invite validators never see. This covers the CLI
        // (`cc-screen-hub user add`) as well.
        validate_email_address(&email)?;
        // 12-char minimum on public signup, aligned with the hub's own
        // CCWEB_PASSWORD warning bar (proposal 0053 Part E).
        anyhow::ensure!(password.len() >= MIN_PASSWORD_LEN, "password must be at least {MIN_PASSWORD_LEN} characters");
        let id = cc_screen_auth::generate_token();
        let hash = hash_password(password)?;
        sqlx::query("INSERT INTO users (id, email, password_hash, created_at) VALUES (?1, ?2, ?3, ?4)")
            .bind(&id)
            .bind(&email)
            .bind(&hash)
            .bind(now_secs() as i64)
            .execute(&self.pool)
            .await
            .map_err(|e| anyhow::anyhow!("create_user (duplicate email?): {e}"))?;
        Ok(id)
    }

    async fn upsert_google_user(&self, google_sub: &str, email: &str) -> anyhow::Result<String> {
        let email = normalize_email(email);
        anyhow::ensure!(!google_sub.is_empty() && !email.is_empty(), "google_sub + email required");
        // 1) Returning user — authoritative match on the stable subject.
        if let Some(row) = sqlx::query("SELECT id FROM users WHERE google_sub = ?1")
            .bind(google_sub)
            .fetch_optional(&self.pool)
            .await?
        {
            return Ok(row.try_get("id")?);
        }
        // 2) First Google sign-in for a known email → link the accounts (only if
        // that row isn't already bound to a different subject).
        if let Some(row) = sqlx::query("SELECT id FROM users WHERE email = ?1 AND google_sub IS NULL")
            .bind(&email)
            .fetch_optional(&self.pool)
            .await?
        {
            let id: String = row.try_get("id")?;
            sqlx::query("UPDATE users SET google_sub = ?1 WHERE id = ?2")
                .bind(google_sub)
                .bind(&id)
                .execute(&self.pool)
                .await?;
            return Ok(id);
        }
        // 3) Brand-new OAuth-only account (no password).
        let id = cc_screen_auth::generate_token();
        sqlx::query("INSERT INTO users (id, email, google_sub, created_at) VALUES (?1, ?2, ?3, ?4)")
            .bind(&id)
            .bind(&email)
            .bind(google_sub)
            .bind(now_secs() as i64)
            .execute(&self.pool)
            .await
            .map_err(|e| anyhow::anyhow!("upsert_google_user: {e}"))?;
        Ok(id)
    }

    async fn upsert_agent(&self, user_id: &str, machine_id: &str) -> anyhow::Result<(String, String)> {
        let token = cc_screen_auth::generate_token();
        let token_hash = cc_screen_auth::sha256_hex(&token);
        let id = cc_screen_auth::generate_token();
        let row = sqlx::query(
            "INSERT INTO agents (id, user_id, machine_id, token_hash, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(user_id, machine_id) DO UPDATE SET token_hash = excluded.token_hash
             RETURNING id",
        )
        .bind(&id)
        .bind(user_id)
        .bind(machine_id)
        .bind(&token_hash)
        .bind(now_secs() as i64)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!("upsert_agent: {e}"))?;
        let agent_id: String = row.try_get("id")?;
        Ok((token, agent_id))
    }

    async fn device_create(&self, device_id: &str, machine_id: &str) -> anyhow::Result<DeviceCode> {
        self.device_insert(device_id, machine_id, "agent").await
    }

    async fn device_create_client(&self, label: &str) -> anyhow::Result<DeviceCode> {
        // No stable device identity for a one-shot terminal sign-in — a random
        // placeholder satisfies the NOT NULL column; the label rides machine_id.
        self.device_insert(&cc_screen_auth::generate_token(), label, "client").await
    }

    async fn device_poll(&self, device_code: &str, kind: &str) -> DevicePoll {
        let now = now_secs() as i64;
        let Ok(Some(row)) = sqlx::query(
            "SELECT status, agent_id, user_id, uplink_token, expires_at, last_polled_at, interval
               FROM device_enrollments WHERE device_code = ?1 AND kind = ?2",
        )
        .bind(device_code)
        .bind(kind)
        .fetch_optional(&self.pool)
        .await
        else {
            return DevicePoll::Expired; // unknown (incl. cross-kind) ⇒ treat as expired
        };
        let expires_at: i64 = row.try_get("expires_at").unwrap_or(0);
        if expires_at < now {
            let _ = sqlx::query("DELETE FROM device_enrollments WHERE device_code = ?1")
                .bind(device_code)
                .execute(&self.pool)
                .await;
            return DevicePoll::Expired;
        }
        // slow_down: polled faster than `interval` since the last poll. Decide off
        // the OLD timestamp, then always stamp now so a tight loop stays throttled.
        let last: Option<i64> = row.try_get("last_polled_at").ok();
        let interval: i64 = row.try_get("interval").unwrap_or(DEVICE_POLL_INTERVAL);
        let too_fast = last.is_some_and(|l| now - l < interval);
        let _ = sqlx::query("UPDATE device_enrollments SET last_polled_at = ?1 WHERE device_code = ?2")
            .bind(now)
            .bind(device_code)
            .execute(&self.pool)
            .await;
        if too_fast {
            return DevicePoll::SlowDown;
        }
        match row.try_get::<String, _>("status").as_deref() {
            Ok("pending") => DevicePoll::Pending,
            Ok("denied") => DevicePoll::Denied,
            Ok("approved") => {
                let token: Option<String> = row.try_get("uplink_token").ok();
                let agent_id: Option<String> = row.try_get("agent_id").ok();
                let user_id: Option<String> = row.try_get("user_id").ok();
                // Single-use: hand the parked token over exactly once, then delete.
                let _ = sqlx::query("DELETE FROM device_enrollments WHERE device_code = ?1")
                    .bind(device_code)
                    .execute(&self.pool)
                    .await;
                if kind == "client" {
                    // 0060: the parked plaintext is a client token; echo the
                    // account email so the terminal confirms the right account.
                    let email = match user_id {
                        Some(uid) => self.user_email(&uid).await.unwrap_or_default(),
                        None => String::new(),
                    };
                    return match token {
                        Some(token) => DevicePoll::ApprovedClient { token, email },
                        None => DevicePoll::Expired,
                    };
                }
                match (token, agent_id) {
                    (Some(token), Some(agent_id)) => DevicePoll::Approved { token, agent_id },
                    _ => DevicePoll::Expired,
                }
            }
            _ => DevicePoll::Expired,
        }
    }

    async fn device_approve(&self, user_id: &str, user_code: &str) -> anyhow::Result<DeviceApproval> {
        let now = now_secs() as i64;
        let code = normalize_user_code(user_code);
        let row = sqlx::query(
            "SELECT device_code, machine_id, kind FROM device_enrollments
              WHERE user_code = ?1 AND status = 'pending' AND expires_at > ?2",
        )
        .bind(&code)
        .bind(now)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| anyhow::anyhow!("unknown or expired code"))?;
        let device_code: String = row.try_get("device_code")?;
        let machine_id: String = row.try_get("machine_id")?;
        let kind: String = row.try_get("kind").unwrap_or_else(|_| "agent".into());

        if kind == "client" {
            // 0060: a terminal sign-in mints a per-user client token — no machine
            // plan cap, no agents row. Park the plaintext for the one poll pickup.
            let token = cc_screen_auth::generate_token();
            sqlx::query(
                "INSERT INTO client_tokens (id, user_id, label, token_hash, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )
            .bind(cc_screen_auth::generate_token())
            .bind(user_id)
            .bind(&machine_id)
            .bind(cc_screen_auth::sha256_hex(&token))
            .bind(now)
            .execute(&self.pool)
            .await
            .map_err(|e| anyhow::anyhow!("device_approve (client token): {e}"))?;
            sqlx::query(
                "UPDATE device_enrollments
                    SET status = 'approved', user_id = ?1, uplink_token = ?2
                  WHERE device_code = ?3",
            )
            .bind(user_id)
            .bind(&token)
            .bind(&device_code)
            .execute(&self.pool)
            .await
            .map_err(|e| anyhow::anyhow!("device_approve: {e}"))?;
            return Ok(DeviceApproval { kind, label: machine_id });
        }

        // Plan gate (§8.3): a genuinely NEW machine past the cap is refused; a
        // re-enroll of an existing label reuses its row and doesn't count. The
        // "LIMIT:" prefix lets the handler answer 402 (not 404). For an active-org
        // member the count is the POOL's (proposal 0063 C2): machines owned by
        // any org member, against the seat-multiplied cap.
        if !self.has_machine(user_id, &machine_id).await {
            let limits = self.limits_for(user_id).await;
            let pool = match self.org_for_user(user_id).await {
                Some((org, _)) if org.seat_count > 0 => Some(org),
                _ => None,
            };
            let count = match &pool {
                Some(org) => {
                    let ids = self.org_member_ids(&org.id).await;
                    self.agent_count_for_users(&ids).await
                }
                None => self.agent_count(user_id).await,
            };
            if count >= limits.max_agents {
                match pool {
                    Some(org) => anyhow::bail!(
                        "LIMIT:Team machine pool full ({} across {} seats). Unlink a machine or add seats.",
                        limits.max_agents,
                        org.seat_count
                    ),
                    None => anyhow::bail!(
                        "LIMIT:Machine limit reached for your plan ({}). Unlink one or ask for an upgrade.",
                        limits.max_agents
                    ),
                }
            }
        }

        // Mint (or rotate) the agent + its token, then park the plaintext for the
        // host's next poll to claim exactly once.
        let (token, agent_id) = self.upsert_agent(user_id, &machine_id).await?;
        // New-machine hook (proposal 0065 A3): a member's fresh machine becomes
        // team-visible without further action. Idempotent set-repair; a no-op for
        // re-enrolls and org-less users.
        if let Some((org, _)) = self.org_for_user(user_id).await {
            self.team_shares_materialize(&org.id).await;
        }
        sqlx::query(
            "UPDATE device_enrollments
                SET status = 'approved', user_id = ?1, agent_id = ?2, uplink_token = ?3
              WHERE device_code = ?4",
        )
        .bind(user_id)
        .bind(&agent_id)
        .bind(&token)
        .bind(&device_code)
        .execute(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!("device_approve: {e}"))?;
        Ok(DeviceApproval { kind, label: machine_id })
    }

    async fn user_by_client_token_hash(&self, hash: &str) -> Option<String> {
        let row = sqlx::query("SELECT id, user_id, last_used_at FROM client_tokens WHERE token_hash = ?1")
            .bind(hash)
            .fetch_optional(&self.pool)
            .await
            .ok()??;
        let id: String = row.try_get("id").ok()?;
        let user_id: String = row.try_get("user_id").ok()?;
        // Touch last_used_at, throttled to ~1/min so the hot path is read-mostly.
        let now = now_secs() as i64;
        let last: Option<i64> = row.try_get("last_used_at").ok().flatten();
        if last.map_or(true, |l| now - l >= 60) {
            let _ = sqlx::query("UPDATE client_tokens SET last_used_at = ?1 WHERE id = ?2")
                .bind(now)
                .bind(&id)
                .execute(&self.pool)
                .await;
        }
        Some(user_id)
    }

    async fn list_client_tokens(&self, user_id: &str) -> Vec<ClientTokenRow> {
        let rows = sqlx::query(
            "SELECT id, label, created_at, last_used_at FROM client_tokens
              WHERE user_id = ?1 ORDER BY created_at DESC",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();
        rows.iter()
            .filter_map(|r| {
                Some(ClientTokenRow {
                    id: r.try_get("id").ok()?,
                    label: r.try_get("label").ok()?,
                    created_at: r.try_get("created_at").ok()?,
                    last_used_at: r.try_get::<Option<i64>, _>("last_used_at").ok().flatten(),
                })
            })
            .collect()
    }

    async fn delete_client_token(&self, user_id: &str, id: &str) -> bool {
        sqlx::query("DELETE FROM client_tokens WHERE id = ?1 AND user_id = ?2")
            .bind(id)
            .bind(user_id)
            .execute(&self.pool)
            .await
            .map(|r| r.rows_affected() > 0)
            .unwrap_or(false)
    }

    async fn delete_client_token_by_hash(&self, hash: &str) -> bool {
        sqlx::query("DELETE FROM client_tokens WHERE token_hash = ?1")
            .bind(hash)
            .execute(&self.pool)
            .await
            .map(|r| r.rows_affected() > 0)
            .unwrap_or(false)
    }

    async fn list_agents(&self, user_id: &str) -> Vec<AgentRow> {
        let rows = sqlx::query(
            "SELECT id, machine_id, created_at, team_visible FROM agents WHERE user_id = ?1 ORDER BY created_at DESC",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();
        rows.iter()
            .filter_map(|r| {
                Some(AgentRow {
                    agent_id: r.try_get("id").ok()?,
                    machine_id: r.try_get("machine_id").ok()?,
                    created_at: r.try_get("created_at").ok()?,
                    team_visible: r.try_get::<i64, _>("team_visible").unwrap_or(1) != 0,
                })
            })
            .collect()
    }

    async fn delete_agent(&self, user_id: &str, agent_id: &str) -> bool {
        sqlx::query("DELETE FROM agents WHERE id = ?1 AND user_id = ?2")
            .bind(agent_id)
            .bind(user_id)
            .execute(&self.pool)
            .await
            .map(|r| r.rows_affected() > 0)
            .unwrap_or(false)
    }

    async fn limits_for(&self, user_id: &str) -> PlanLimits {
        // Org inheritance (proposal 0063 C1): an ACTIVE org (seat_count > 0)
        // replaces the member's personal caps with the pooled org caps. The
        // plan_limits 'team' row stores PER-SEAT contributions; the pool is
        // row × seats. can_create_shares and the summary budget are per-member
        // semantics — never multiplied. A dormant org (seat_count = 0) confers
        // nothing: members keep their personal plans, so creating an org can
        // never reduce anyone's entitlements.
        if let Some((org, _role)) = self.org_for_user(user_id).await {
            if org.seat_count > 0 {
                if let Some(row) = self.plan_row(&org.plan).await {
                    return PlanLimits {
                        plan: row.plan,
                        max_agents: row.max_agents * org.seat_count,
                        max_concurrent_sessions: row.max_concurrent_sessions * org.seat_count,
                        can_create_shares: row.can_create_shares,
                        summary_user_budget_usd: row.summary_user_budget_usd,
                    };
                }
            }
        }
        sqlx::query(
            "SELECT pl.plan, pl.max_agents, pl.max_concurrent_sessions,
                    pl.can_create_shares, pl.summary_user_budget_usd
               FROM users u JOIN plan_limits pl ON pl.plan = u.plan
              WHERE u.id = ?1",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten()
        .and_then(|r| {
            Some(PlanLimits {
                plan: r.try_get("plan").ok()?,
                max_agents: r.try_get("max_agents").ok()?,
                max_concurrent_sessions: r.try_get("max_concurrent_sessions").ok()?,
                can_create_shares: r.try_get::<i64, _>("can_create_shares").ok()? != 0,
                summary_user_budget_usd: r
                    .try_get::<Option<f64>, _>("summary_user_budget_usd")
                    .ok()
                    .flatten(),
            })
        })
        .unwrap_or_default()
    }

    async fn agent_count(&self, user_id: &str) -> i64 {
        sqlx::query("SELECT count(*) AS n FROM agents WHERE user_id = ?1")
            .bind(user_id)
            .fetch_one(&self.pool)
            .await
            .ok()
            .and_then(|r| r.try_get::<i64, _>("n").ok())
            .unwrap_or(0)
    }

    async fn has_machine(&self, user_id: &str, machine_id: &str) -> bool {
        sqlx::query("SELECT 1 AS x FROM agents WHERE user_id = ?1 AND machine_id = ?2")
            .bind(user_id)
            .bind(machine_id)
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten()
            .is_some()
    }

    async fn set_plan(&self, email: &str, plan: &str) -> anyhow::Result<()> {
        // Org-only plans never land on users (proposal 0063 A1): the 'team' row
        // stores PER-SEAT contributions and is meaningless without an org's seat
        // multiplier — `user plan x@y team` would "work" and entitle nothing sane.
        anyhow::ensure!(
            plan != "team",
            "'team' is an org plan — use `cc-screen-hub org seats`, not `user plan`"
        );
        // Guard the plan exists, so a typo can't strand a user on an unknown plan
        // (which would silently fall back to the conservative default).
        let known = sqlx::query("SELECT 1 AS x FROM plan_limits WHERE plan = ?1")
            .bind(plan)
            .fetch_optional(&self.pool)
            .await?
            .is_some();
        anyhow::ensure!(known, "unknown plan '{plan}'");
        let res = sqlx::query("UPDATE users SET plan = ?1 WHERE email = ?2")
            .bind(plan)
            .bind(normalize_email(email))
            .execute(&self.pool)
            .await?;
        anyhow::ensure!(res.rows_affected() > 0, "no such user: {email}");
        Ok(())
    }

    async fn billing_event_insert(&self, id: &str, payload_hash: &str, received_at: i64) -> anyhow::Result<u64> {
        let res = sqlx::query(
            "INSERT INTO billing_events (id, received_at, payload_hash) VALUES (?1, ?2, ?3)
             ON CONFLICT(id) DO NOTHING",
        )
        .bind(id)
        .bind(received_at)
        .bind(payload_hash)
        .execute(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!("billing_event_insert: {e}"))?;
        Ok(res.rows_affected())
    }

    async fn billing_ids(&self, user_id: &str) -> (Option<String>, Option<String>) {
        sqlx::query("SELECT billing_customer_id, billing_subscription_id FROM users WHERE id = ?1")
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten()
            .map(|r| {
                (
                    r.try_get::<Option<String>, _>("billing_customer_id").ok().flatten(),
                    r.try_get::<Option<String>, _>("billing_subscription_id").ok().flatten(),
                )
            })
            .unwrap_or((None, None))
    }

    async fn billing_status(&self, user_id: &str) -> (Option<String>, Option<i64>) {
        sqlx::query("SELECT plan_status, current_period_end FROM users WHERE id = ?1")
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten()
            .map(|r| {
                (
                    r.try_get::<Option<String>, _>("plan_status").ok().flatten(),
                    r.try_get::<Option<i64>, _>("current_period_end").ok().flatten(),
                )
            })
            .unwrap_or((None, None))
    }

    async fn user_by_billing_customer(&self, customer_id: &str) -> Option<String> {
        sqlx::query("SELECT id FROM users WHERE billing_customer_id = ?1")
            .bind(customer_id)
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten()
            .and_then(|r| r.try_get("id").ok())
    }

    async fn apply_subscription_state(
        &self,
        user_id: &str,
        plan: Option<&str>,
        status: &str,
        subscription_id: Option<&str>,
        customer_id: Option<&str>,
        period_end: Option<i64>,
    ) -> anyhow::Result<()> {
        let mut conn = self.pool.acquire().await?;
        apply_sub_state(&mut conn, user_id, plan, status, subscription_id, customer_id, period_end).await
    }

    async fn billing_process_event(
        &self,
        id: &str,
        payload_hash: &str,
        received_at: i64,
        apply: Option<SubApply>,
    ) -> anyhow::Result<EventOutcome> {
        let mut tx = self.pool.begin().await?;
        // Idempotency insert — first delivery wins, a replay affects 0 rows.
        let res = sqlx::query(
            "INSERT INTO billing_events (id, received_at, payload_hash) VALUES (?1, ?2, ?3)
             ON CONFLICT(id) DO NOTHING",
        )
        .bind(id)
        .bind(received_at)
        .bind(payload_hash)
        .execute(&mut *tx)
        .await?;
        if res.rows_affected() == 0 {
            // Already processed: commit (nothing changed) and ack.
            tx.commit().await?;
            return Ok(EventOutcome::Duplicate);
        }
        // The state change shares this transaction: an error here drops `tx`,
        // rolling back the idempotency insert so Stripe's retry reprocesses.
        if let Some(a) = apply {
            match &a.target {
                SubTarget::User(uid) => {
                    apply_sub_state(
                        &mut tx,
                        uid,
                        a.plan.as_deref(),
                        &a.status,
                        a.subscription_id.as_deref(),
                        a.customer_id.as_deref(),
                        a.period_end,
                    )
                    .await?;
                }
                SubTarget::Org(oid) => {
                    apply_org_sub_state(
                        &mut tx,
                        oid,
                        a.plan.as_deref(),
                        &a.status,
                        a.subscription_id.as_deref(),
                        a.customer_id.as_deref(),
                        a.period_end,
                        a.seat_count,
                    )
                    .await?;
                }
            }
        }
        tx.commit().await?;
        Ok(EventOutcome::Applied)
    }

    async fn apply_sub(&self, a: &SubApply) -> anyhow::Result<()> {
        let mut conn = self.pool.acquire().await?;
        match &a.target {
            SubTarget::User(uid) => {
                apply_sub_state(
                    &mut conn,
                    uid,
                    a.plan.as_deref(),
                    &a.status,
                    a.subscription_id.as_deref(),
                    a.customer_id.as_deref(),
                    a.period_end,
                )
                .await
            }
            SubTarget::Org(oid) => {
                apply_org_sub_state(
                    &mut conn,
                    oid,
                    a.plan.as_deref(),
                    &a.status,
                    a.subscription_id.as_deref(),
                    a.customer_id.as_deref(),
                    a.period_end,
                    a.seat_count,
                )
                .await
            }
        }
    }

    async fn billing_rows_for_reconcile(&self) -> Vec<BillingRow> {
        let rows = sqlx::query(
            "SELECT id, billing_customer_id, billing_subscription_id, plan, plan_status
               FROM users
              WHERE billing_customer_id IS NOT NULL OR plan_status IS NOT NULL",
        )
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();
        rows.iter()
            .filter_map(|r| {
                Some(BillingRow {
                    user_id: r.try_get("id").ok()?,
                    customer_id: r.try_get::<Option<String>, _>("billing_customer_id").ok().flatten(),
                    subscription_id: r.try_get::<Option<String>, _>("billing_subscription_id").ok().flatten(),
                    plan: r.try_get("plan").ok()?,
                    plan_status: r.try_get::<Option<String>, _>("plan_status").ok().flatten(),
                })
            })
            .collect()
    }

    async fn visibility_rows(&self, user_id: &str) -> Vec<ShareRow> {
        let rows = sqlx::query(
            "SELECT s.id, s.agent_id, a.user_id AS owner_user_id, s.grantee_user_id,
                    s.kind, s.session, s.owner_peek, s.created_at, s.org_id
               FROM shares s JOIN agents a ON a.id = s.agent_id
              WHERE s.grantee_user_id = ?1 OR a.user_id = ?1",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();
        rows.iter().filter_map(share_row).collect()
    }

    async fn share_agent(&self, owner_user_id: &str, agent_id: &str, grantee_user_id: &str, owner_peek: bool) -> anyhow::Result<String> {
        self.assert_owns(owner_user_id, agent_id, grantee_user_id).await?;
        // Upsert by (agent, grantee, kind='agent') explicitly: an agent share has
        // session NULL, and SQLite treats NULLs as DISTINCT in the UNIQUE key, so
        // ON CONFLICT never fires for it. Re-issuing keeps the row id and just flips
        // owner_peek (proposal 0039 §2).
        let existing: Option<String> = sqlx::query(
            "SELECT id FROM shares WHERE agent_id = ?1 AND grantee_user_id = ?2 AND kind = 'agent'",
        )
        .bind(agent_id)
        .bind(grantee_user_id)
        .fetch_optional(&self.pool)
        .await?
        .and_then(|r| r.try_get("id").ok());
        if let Some(id) = existing {
            sqlx::query("UPDATE shares SET owner_peek = ?1 WHERE id = ?2")
                .bind(owner_peek as i64)
                .bind(&id)
                .execute(&self.pool)
                .await
                .map_err(|e| anyhow::anyhow!("share_agent (update peek): {e}"))?;
            return Ok(id);
        }
        let id = cc_screen_auth::generate_token();
        sqlx::query(
            "INSERT INTO shares (id, agent_id, grantee_user_id, kind, session, owner_peek, created_at)
             VALUES (?1, ?2, ?3, 'agent', NULL, ?4, ?5)",
        )
        .bind(&id)
        .bind(agent_id)
        .bind(grantee_user_id)
        .bind(owner_peek as i64)
        .bind(now_secs() as i64)
        .execute(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!("share_agent: {e}"))?;
        Ok(id)
    }

    async fn share_session(&self, owner_user_id: &str, agent_id: &str, grantee_user_id: &str, session: &str) -> anyhow::Result<String> {
        self.assert_owns(owner_user_id, agent_id, grantee_user_id).await?;
        anyhow::ensure!(!session.trim().is_empty(), "session is required for a session share");
        let id = cc_screen_auth::generate_token();
        let row = sqlx::query(
            "INSERT INTO shares (id, agent_id, grantee_user_id, kind, session, owner_peek, created_at)
             VALUES (?1, ?2, ?3, 'session', ?4, 0, ?5)
             ON CONFLICT(agent_id, grantee_user_id, kind, session) DO UPDATE SET id = id
             RETURNING id",
        )
        .bind(&id)
        .bind(agent_id)
        .bind(grantee_user_id)
        .bind(session)
        .bind(now_secs() as i64)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!("share_session: {e}"))?;
        Ok(row.try_get("id")?)
    }

    async fn shares_by_owner(&self, owner_user_id: &str) -> Vec<ShareRow> {
        // Team rows are excluded: the outbox is the *personal* "shared by me"
        // view, and materialized team grants are managed by membership (0065 A4),
        // not individually revocable rows.
        let rows = sqlx::query(
            "SELECT s.id, s.agent_id, a.user_id AS owner_user_id, s.grantee_user_id,
                    s.kind, s.session, s.owner_peek, s.created_at, s.org_id
               FROM shares s JOIN agents a ON a.id = s.agent_id
              WHERE a.user_id = ?1 AND s.kind != 'team'
              ORDER BY s.created_at DESC",
        )
        .bind(owner_user_id)
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();
        rows.iter().filter_map(share_row).collect()
    }

    async fn revoke_share(&self, owner_user_id: &str, share_id: &str) -> bool {
        // Team rows are not individually revocable (0065 A4) — the way out is the
        // per-machine opt-out or membership change; share.rs answers 409.
        sqlx::query(
            "DELETE FROM shares
              WHERE id = ?1 AND kind != 'team'
                AND agent_id IN (SELECT id FROM agents WHERE user_id = ?2)",
        )
        .bind(share_id)
        .bind(owner_user_id)
        .execute(&self.pool)
        .await
        .map(|r| r.rows_affected() > 0)
        .unwrap_or(false)
    }

    async fn share_create(
        &self,
        inviter: &str,
        grantee: &str,
        kind: &str,
        agent_id: &str,
        session: Option<&str>,
        owner_peek: bool,
    ) -> anyhow::Result<(String, String)> {
        // Owner-scoped + no self-invite (same guard the 0039 grant insert uses).
        self.assert_owns(inviter, agent_id, grantee).await?;
        anyhow::ensure!(kind == "agent" || kind == "session", "bad resource_kind");
        anyhow::ensure!((kind == "session") == session.is_some(), "session iff session-kind");
        let now = now_secs() as i64;
        let expires = now + SHARE_INVITE_TTL;

        // Upsert by (grantee, agent, kind, session) explicitly — a NULL session for
        // an agent invite is DISTINCT under the UNIQUE index, so ON CONFLICT can't
        // be relied on (proposal 0040 §2 / §4).
        let existing = sqlx::query(
            "SELECT id, status FROM share_invites
              WHERE grantee_user_id = ?1 AND agent_id = ?2 AND resource_kind = ?3
                AND ((?4 IS NULL AND session_name IS NULL) OR session_name = ?4)",
        )
        .bind(grantee)
        .bind(agent_id)
        .bind(kind)
        .bind(session)
        .fetch_optional(&self.pool)
        .await?;

        if let Some(row) = existing {
            let id: String = row.try_get("id")?;
            let status: String = row.try_get("status")?;
            // Already shared → no-op success; the owner can't silently re-offer over
            // a live grant (§4).
            if status == "accepted" {
                return Ok((id, status));
            }
            // pending/declined/revoked/expired → (re)offer: refresh to pending.
            sqlx::query(
                "UPDATE share_invites
                    SET status = 'pending', owner_peek = ?1, created_at = ?2,
                        expires_at = ?3, responded_at = NULL
                  WHERE id = ?4",
            )
            .bind(owner_peek as i64)
            .bind(now)
            .bind(expires)
            .bind(&id)
            .execute(&self.pool)
            .await?;
            return Ok((id, "pending".into()));
        }

        let id = cc_screen_auth::generate_token();
        sqlx::query(
            "INSERT INTO share_invites
                (id, inviter_user_id, grantee_user_id, resource_kind, agent_id,
                 session_name, owner_peek, status, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', ?8, ?9)",
        )
        .bind(&id)
        .bind(inviter)
        .bind(grantee)
        .bind(kind)
        .bind(agent_id)
        .bind(session)
        .bind(owner_peek as i64)
        .bind(now)
        .bind(expires)
        .execute(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!("share_create: {e}"))?;
        Ok((id, "pending".into()))
    }

    async fn share_inbox(&self, grantee: &str) -> Vec<ShareInviteRow> {
        let now = now_secs() as i64;
        // Lazy expiry: flip overdue pending rows first, then list only live pending.
        let _ = sqlx::query(
            "UPDATE share_invites SET status = 'expired'
              WHERE status = 'pending' AND expires_at IS NOT NULL AND expires_at < ?1",
        )
        .bind(now)
        .execute(&self.pool)
        .await;
        let rows = sqlx::query(
            "SELECT * FROM share_invites
              WHERE grantee_user_id = ?1 AND status = 'pending'
              ORDER BY created_at DESC",
        )
        .bind(grantee)
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();
        rows.iter().filter_map(share_invite_row).collect()
    }

    async fn share_outbox(&self, inviter: &str) -> Vec<ShareInviteRow> {
        let now = now_secs() as i64;
        let _ = sqlx::query(
            "UPDATE share_invites SET status = 'expired'
              WHERE status = 'pending' AND expires_at IS NOT NULL AND expires_at < ?1",
        )
        .bind(now)
        .execute(&self.pool)
        .await;
        let rows = sqlx::query(
            "SELECT * FROM share_invites WHERE inviter_user_id = ?1 ORDER BY created_at DESC",
        )
        .bind(inviter)
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();
        rows.iter().filter_map(share_invite_row).collect()
    }

    async fn share_get(&self, id: &str) -> Option<ShareInviteRow> {
        let row = sqlx::query("SELECT * FROM share_invites WHERE id = ?1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .ok()??;
        share_invite_row(&row)
    }

    async fn share_respond(&self, grantee: &str, id: &str, accept: bool) -> ShareOutcome {
        let Some(inv) = self.share_get(id).await else { return ShareOutcome::NotFound };
        // Not yours ⇒ 404 (don't leak existence).
        if inv.grantee_user_id != grantee {
            return ShareOutcome::NotFound;
        }
        let now = now_secs() as i64;
        // Lazy expiry of an overdue pending row.
        let status = if inv.status == "pending" && inv.expires_at.is_some_and(|e| e < now) {
            let _ = sqlx::query("UPDATE share_invites SET status = 'expired' WHERE id = ?1")
                .bind(id)
                .execute(&self.pool)
                .await;
            "expired".to_string()
        } else {
            inv.status.clone()
        };

        let target = if accept { "accepted" } else { "declined" };
        // Idempotent no-op if already in the target state.
        if status == target {
            return ShareOutcome::Ok(status);
        }
        // Only a live pending row may transition.
        if status != "pending" {
            return ShareOutcome::Conflict;
        }
        if sqlx::query("UPDATE share_invites SET status = ?1, responded_at = ?2 WHERE id = ?3 AND status = 'pending'")
            .bind(target)
            .bind(now)
            .bind(id)
            .execute(&self.pool)
            .await
            .map(|r| r.rows_affected() == 0)
            .unwrap_or(true)
        {
            // Lost a race (another writer moved it out of pending) ⇒ conflict.
            return ShareOutcome::Conflict;
        }
        // Accept materialises the 0039 grant the visibility predicate reads.
        if accept {
            if let Err(e) = self.materialise_grant(&inv).await {
                tracing::warn!("share_respond: failed to materialise grant for {id}: {e}");
            }
        }
        ShareOutcome::Ok(target.into())
    }

    async fn share_revoke(&self, inviter: &str, id: &str) -> ShareOutcome {
        let Some(inv) = self.share_get(id).await else { return ShareOutcome::NotFound };
        if inv.inviter_user_id != inviter {
            return ShareOutcome::NotFound;
        }
        // Forgiving: revoke is the cancel path — any already-not-granting state is a
        // no-op success (§4). Only pending/accepted actually transition.
        if matches!(inv.status.as_str(), "revoked" | "declined" | "expired") {
            return ShareOutcome::Ok(inv.status);
        }
        let now = now_secs() as i64;
        let _ = sqlx::query("UPDATE share_invites SET status = 'revoked', responded_at = ?1 WHERE id = ?2")
            .bind(now)
            .bind(id)
            .execute(&self.pool)
            .await;
        // Strip any materialised grant (no-op if it was only pending).
        self.strip_grant(&inv).await;
        ShareOutcome::Ok("revoked".into())
    }

    async fn shares_to_me(&self, grantee: &str) -> Vec<ShareRow> {
        let rows = sqlx::query(
            "SELECT s.id, s.agent_id, a.user_id AS owner_user_id, s.grantee_user_id,
                    s.kind, s.session, s.owner_peek, s.created_at, s.org_id
               FROM shares s JOIN agents a ON a.id = s.agent_id
              WHERE s.grantee_user_id = ?1
              ORDER BY s.created_at DESC",
        )
        .bind(grantee)
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();
        rows.iter().filter_map(share_row).collect()
    }

    async fn leave_grant(&self, grantee: &str, share_id: &str) -> bool {
        // Resolve the grant (grantee-scoped) so we can settle its invite too.
        // Team rows never reach here — share.rs refuses them with 409 first.
        let Some(row) = sqlx::query(
            "SELECT agent_id, kind, session FROM shares WHERE id = ?1 AND grantee_user_id = ?2 AND kind != 'team'",
        )
        .bind(share_id)
        .bind(grantee)
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten() else {
            return false;
        };
        let agent_id: String = match row.try_get("agent_id") {
            Ok(v) => v,
            Err(_) => return false,
        };
        let kind: String = row.try_get("kind").unwrap_or_default();
        let session: Option<String> =
            row.try_get::<Option<String>, _>("session").ok().flatten().filter(|s| !s.is_empty());

        let gone = sqlx::query("DELETE FROM shares WHERE id = ?1 AND grantee_user_id = ?2")
            .bind(share_id)
            .bind(grantee)
            .execute(&self.pool)
            .await
            .map(|r| r.rows_affected() > 0)
            .unwrap_or(false);
        if gone {
            // Settle the matching invite so it doesn't re-grant or read as active.
            let _ = sqlx::query(
                "UPDATE share_invites SET status = 'declined', responded_at = ?1
                  WHERE grantee_user_id = ?2 AND agent_id = ?3 AND resource_kind = ?4
                    AND ((?5 IS NULL AND session_name IS NULL) OR session_name = ?5)",
            )
            .bind(now_secs() as i64)
            .bind(grantee)
            .bind(&agent_id)
            .bind(&kind)
            .bind(session.as_deref())
            .execute(&self.pool)
            .await;
        }
        gone
    }

    // ── read-only link grants (proposal 0083 Part C) ──────────────────────────

    async fn link_share_create(
        &self,
        owner_user_id: &str,
        agent_id: &str,
        path: &str,
        name: &str,
        token_hash: &str,
        expires_at: Option<i64>,
    ) -> anyhow::Result<String> {
        self.assert_owns_agent(owner_user_id, agent_id).await?;
        anyhow::ensure!(path.starts_with('/'), "path must be absolute");
        anyhow::ensure!(token_hash.len() == 64, "token_hash must be a sha256 hex digest");
        let id = cc_screen_auth::generate_token();
        sqlx::query(
            "INSERT INTO link_shares (id, agent_id, owner_user_id, token_hash, path, name, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )
        .bind(&id)
        .bind(agent_id)
        .bind(owner_user_id)
        .bind(token_hash)
        .bind(path)
        .bind(name)
        .bind(now_secs() as i64)
        .bind(expires_at)
        .execute(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!("link_share_create: {e}"))?;
        Ok(id)
    }

    async fn link_share_by_token_hash(&self, token_hash: &str) -> Option<LinkShareRow> {
        // Expiry is part of the WHERE clause on purpose: an expired grant is
        // indistinguishable from an unknown one all the way down to the store,
        // so no handler can grow a branch that tells them apart.
        let now = now_secs() as i64;
        let row = sqlx::query(
            "SELECT * FROM link_shares
              WHERE token_hash = ?1 AND (expires_at IS NULL OR expires_at > ?2)",
        )
        .bind(token_hash)
        .bind(now)
        .fetch_optional(&self.pool)
        .await
        .ok()??;
        link_share_row(&row)
    }

    async fn link_share_outbox(&self, owner_user_id: &str) -> Vec<LinkShareRow> {
        let now = now_secs() as i64;
        let rows = sqlx::query(
            "SELECT * FROM link_shares
              WHERE owner_user_id = ?1 AND (expires_at IS NULL OR expires_at > ?2)
              ORDER BY created_at DESC",
        )
        .bind(owner_user_id)
        .bind(now)
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();
        rows.iter().filter_map(link_share_row).collect()
    }

    async fn link_share_revoke(&self, owner_user_id: &str, id: &str) -> bool {
        sqlx::query("DELETE FROM link_shares WHERE id = ?1 AND owner_user_id = ?2")
            .bind(id)
            .bind(owner_user_id)
            .execute(&self.pool)
            .await
            .map(|r| r.rows_affected() > 0)
            .unwrap_or(false)
    }

    async fn link_share_regenerate(&self, owner_user_id: &str, id: &str, token_hash: &str) -> bool {
        if token_hash.len() != 64 {
            return false;
        }
        sqlx::query("UPDATE link_shares SET token_hash = ?1 WHERE id = ?2 AND owner_user_id = ?3")
            .bind(token_hash)
            .bind(id)
            .bind(owner_user_id)
            .execute(&self.pool)
            .await
            .map(|r| r.rows_affected() > 0)
            .unwrap_or(false)
    }

    async fn email_invite_create(
        &self,
        inviter: &str,
        email: &str,
        kind: &str,
        agent_id: &str,
        session: Option<&str>,
        owner_peek: bool,
        converted: bool,
    ) -> anyhow::Result<(String, String)> {
        self.assert_owns_agent(inviter, agent_id).await?;
        anyhow::ensure!(kind == "agent" || kind == "session", "bad resource_kind");
        anyhow::ensure!((kind == "session") == session.is_some(), "session iff session-kind");
        let email = normalize_email(email);
        // Defense in depth (proposal 0073 C3): both handlers validate at the API
        // boundary so a bad address 400s before any row is written; this is the
        // backstop for the CLI and for any future caller.
        validate_email_address(&email)?;
        let now = now_secs() as i64;
        let expires = now + SHARE_INVITE_TTL;
        let converted_at = converted.then_some(now);

        // Upsert by (email, kind, agent, session) explicitly — a NULL session for
        // an agent invite is DISTINCT under the UNIQUE index (same discipline as
        // share_invites). A re-offer keeps the row id but mints a FRESH token
        // (the old link dies) and refreshes the TTL.
        let existing: Option<String> =
            self.email_invite_existing_id(&email, kind, agent_id, session).await;

        // Abuse bound (proposal 0056 C2, holes closed by 0073 C1): cap the
        // inviter's LIVE (unconverted, unexpired) email invites. Three things
        // moved here from below the re-offer early-return:
        //   * it now runs on the RE-OFFER path too (D2's Resend is a re-offer,
        //     and re-offers used to be bounded by nothing at all);
        //   * it no longer skips `converted` (known-account) rows — skipping the
        //     known arm made the cap reachable in one arm only, which is exactly
        //     the account-existence oracle [0042] forbids. A pre-converted row
        //     still never *counts* toward the total, it is merely subject to it;
        //   * the row this call is about to refresh is excluded from the count,
        //     so re-offering one of your own live invites is never blocked by
        //     the cap it is itself a member of.
        let live = self.email_invite_live_excluding(inviter, existing.as_deref().unwrap_or_default()).await;
        anyhow::ensure!(
            live < EMAIL_INVITE_CAP,
            "CAP:too many pending invitations — cancel some from your outbox first"
        );

        let token = cc_screen_auth::generate_token();
        if let Some(id) = existing {
            // Same as org_invite_create's re-offer: a fresh token means a fresh
            // delivery receipt (proposal 0073 B2), never attempt one's.
            sqlx::query(
                "UPDATE email_invites
                    SET token = ?1, owner_peek = ?2, created_at = ?3, expires_at = ?4,
                        converted_at = ?5, delivery = NULL, emailed_at = NULL
                  WHERE id = ?6",
            )
            .bind(&token)
            .bind(owner_peek as i64)
            .bind(now)
            .bind(expires)
            .bind(converted_at)
            .bind(&id)
            .execute(&self.pool)
            .await?;
            return Ok((id, token));
        }

        let id = cc_screen_auth::generate_token();
        sqlx::query(
            "INSERT INTO email_invites
                (id, token, inviter_user_id, email, resource_kind, agent_id,
                 session_name, owner_peek, created_at, expires_at, converted_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        )
        .bind(&id)
        .bind(&token)
        .bind(inviter)
        .bind(&email)
        .bind(kind)
        .bind(agent_id)
        .bind(session)
        .bind(owner_peek as i64)
        .bind(now)
        .bind(expires)
        .bind(converted_at)
        .execute(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!("email_invite_create: {e}"))?;
        Ok((id, token))
    }

    async fn attach_email_invites(&self, user_id: &str, email: &str) -> usize {
        let email = normalize_email(email);
        let now = now_secs() as i64;
        let rows = sqlx::query(
            "SELECT * FROM email_invites
              WHERE email = ?1 AND converted_at IS NULL AND expires_at > ?2",
        )
        .bind(&email)
        .bind(now)
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();
        let mut attached = 0;
        for row in rows.iter().filter_map(email_invite_row) {
            // The existing 0040 upsert — re-validates ownership + no-self-share,
            // so an invite to an address the inviter later registers themselves
            // simply doesn't attach.
            let ok = self
                .share_create(
                    &row.inviter_user_id,
                    user_id,
                    &row.resource_kind,
                    &row.agent_id,
                    row.session_name.as_deref(),
                    row.owner_peek,
                )
                .await
                .is_ok();
            if ok {
                let _ = sqlx::query("UPDATE email_invites SET converted_at = ?1 WHERE id = ?2")
                    .bind(now)
                    .bind(&row.id)
                    .execute(&self.pool)
                    .await;
                attached += 1;
            }
        }
        attached
    }

    async fn email_invite_by_token(&self, token: &str) -> Option<EmailInviteRow> {
        let now = now_secs() as i64;
        let row = sqlx::query("SELECT * FROM email_invites WHERE token = ?1 AND expires_at > ?2")
            .bind(token)
            .bind(now)
            .fetch_optional(&self.pool)
            .await
            .ok()??;
        email_invite_row(&row)
    }

    async fn email_invite_revoke(&self, inviter: &str, id: &str) -> bool {
        // Only an unconverted row is the invite's live lifecycle record — once
        // converted, the share_invites row (share_revoke) is authoritative.
        sqlx::query(
            "DELETE FROM email_invites
              WHERE id = ?1 AND inviter_user_id = ?2 AND converted_at IS NULL",
        )
        .bind(id)
        .bind(inviter)
        .execute(&self.pool)
        .await
        .map(|r| r.rows_affected() > 0)
        .unwrap_or(false)
    }

    async fn email_invite_outbox(&self, inviter: &str) -> Vec<EmailInviteRow> {
        let now = now_secs() as i64;
        let rows = sqlx::query(
            "SELECT * FROM email_invites
              WHERE inviter_user_id = ?1 AND converted_at IS NULL AND expires_at > ?2
              ORDER BY created_at DESC",
        )
        .bind(inviter)
        .bind(now)
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();
        rows.iter().filter_map(email_invite_row).collect()
    }

    async fn email_invite_cap_exceeded(
        &self,
        inviter: &str,
        email: &str,
        kind: &str,
        agent_id: &str,
        session: Option<&str>,
    ) -> bool {
        let email = normalize_email(email);
        let existing = self.email_invite_existing_id(&email, kind, agent_id, session).await;
        self.email_invite_live_excluding(inviter, existing.as_deref().unwrap_or_default()).await
            >= EMAIL_INVITE_CAP
    }

    // ── orgs (proposal 0063) ───────────────────────────────────────────────────

    async fn org_create(&self, creator: &str, name: &str) -> anyhow::Result<String> {
        let name = name.trim();
        // Proposal 0073 C3: the org name is interpolated into the invitation
        // subject/body, so a control character in it is a header-injection
        // vector. Enforced HERE (not just at the route) so `cc-screen-hub org
        // create` and any future rename go through the same rule.
        validate_org_name(name)?;
        anyhow::ensure!(
            self.org_for_user(creator).await.is_none(),
            "ONEORG:you're already in a team — leave it before creating another"
        );
        let id = cc_screen_auth::generate_token();
        let now = now_secs() as i64;
        let mut tx = self.pool.begin().await?;
        sqlx::query("INSERT INTO orgs (id, name, plan, created_at) VALUES (?1, ?2, 'team', ?3)")
            .bind(&id)
            .bind(name)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        // UNIQUE(user_id) is the one-org race backstop: a concurrent join fails
        // the insert and rolls the whole org back.
        sqlx::query(
            "INSERT INTO org_members (org_id, user_id, role, created_at) VALUES (?1, ?2, 'owner', ?3)",
        )
        .bind(&id)
        .bind(creator)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(|_| anyhow::anyhow!("ONEORG:you're already in a team"))?;
        tx.commit().await?;
        self.audit_append(&id, Some(creator), "org.created", None, Some(&format!("{{\"name\":{}}}", serde_json::to_string(name).unwrap_or_default()))).await;
        Ok(id)
    }

    async fn org_for_user(&self, user_id: &str) -> Option<(OrgRow, String)> {
        let row = sqlx::query(
            "SELECT o.*, m.role AS my_role
               FROM org_members m JOIN orgs o ON o.id = m.org_id
              WHERE m.user_id = ?1",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .ok()??;
        let role: String = row.try_get("my_role").ok()?;
        Some((org_row(&row)?, role))
    }

    async fn org_get(&self, org_id: &str) -> Option<OrgRow> {
        let row = sqlx::query("SELECT * FROM orgs WHERE id = ?1")
            .bind(org_id)
            .fetch_optional(&self.pool)
            .await
            .ok()??;
        org_row(&row)
    }

    async fn org_by_name_or_id(&self, key: &str) -> Option<OrgRow> {
        if let Some(org) = self.org_get(key).await {
            return Some(org);
        }
        let rows = sqlx::query("SELECT * FROM orgs WHERE name = ?1 LIMIT 2")
            .bind(key)
            .fetch_all(&self.pool)
            .await
            .ok()?;
        match rows.as_slice() {
            [only] => org_row(only),
            _ => None, // unknown, or an ambiguous name — the CLI asks for the id
        }
    }

    async fn org_members(&self, org_id: &str) -> Vec<OrgMemberRow> {
        let rows = sqlx::query(
            "SELECT m.user_id, u.email, m.role, m.created_at
               FROM org_members m JOIN users u ON u.id = m.user_id
              WHERE m.org_id = ?1
              ORDER BY m.created_at ASC",
        )
        .bind(org_id)
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();
        rows.iter()
            .filter_map(|r| {
                Some(OrgMemberRow {
                    user_id: r.try_get("user_id").ok()?,
                    email: r.try_get("email").ok()?,
                    role: r.try_get("role").ok()?,
                    created_at: r.try_get("created_at").ok()?,
                })
            })
            .collect()
    }

    async fn org_member_ids(&self, org_id: &str) -> Vec<String> {
        let rows = sqlx::query("SELECT user_id FROM org_members WHERE org_id = ?1")
            .bind(org_id)
            .fetch_all(&self.pool)
            .await
            .unwrap_or_default();
        rows.iter().filter_map(|r| r.try_get("user_id").ok()).collect()
    }

    async fn org_set_role(&self, org_id: &str, user_id: &str, role: &str) -> anyhow::Result<()> {
        anyhow::ensure!(matches!(role, "owner" | "admin" | "member"), "unknown role '{role}'");
        let members = self.org_members(org_id).await;
        let Some(target) = members.iter().find(|m| m.user_id == user_id) else {
            anyhow::bail!("no such member");
        };
        if role == "owner" {
            // Ownership transfer: exactly one owner row exists at all times —
            // demote the current owner and promote the target in one transaction.
            let mut tx = self.pool.begin().await?;
            sqlx::query("UPDATE org_members SET role = 'admin' WHERE org_id = ?1 AND role = 'owner'")
                .bind(org_id)
                .execute(&mut *tx)
                .await?;
            let res = sqlx::query(
                "UPDATE org_members SET role = 'owner' WHERE org_id = ?1 AND user_id = ?2",
            )
            .bind(org_id)
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
            anyhow::ensure!(res.rows_affected() == 1, "no such member");
            tx.commit().await?;
            return Ok(());
        }
        // The owner never gets demoted in place — transfer ownership first, so
        // there is always exactly one owner.
        anyhow::ensure!(target.role != "owner", "OWNER:transfer ownership first");
        sqlx::query("UPDATE org_members SET role = ?1 WHERE org_id = ?2 AND user_id = ?3")
            .bind(role)
            .bind(org_id)
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn org_remove_member(&self, org_id: &str, user_id: &str) -> anyhow::Result<()> {
        let members = self.org_members(org_id).await;
        let Some(target) = members.iter().find(|m| m.user_id == user_id) else {
            anyhow::bail!("no such member");
        };
        anyhow::ensure!(target.role != "owner", "OWNER:transfer ownership first");
        sqlx::query("DELETE FROM org_members WHERE org_id = ?1 AND user_id = ?2")
            .bind(org_id)
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        // Membership gone ⇒ their team visibility goes with it, both directions.
        self.team_shares_prune(org_id).await;
        Ok(())
    }

    async fn org_set_seats(&self, org_id: &str, seats: i64) -> anyhow::Result<()> {
        anyhow::ensure!(seats >= 0, "seats must be ≥ 0");
        let res = sqlx::query("UPDATE orgs SET seat_count = ?1 WHERE id = ?2")
            .bind(seats)
            .bind(org_id)
            .execute(&self.pool)
            .await?;
        anyhow::ensure!(res.rows_affected() > 0, "no such org");
        Ok(())
    }

    async fn org_invite_create(
        &self,
        org_id: &str,
        inviter: &str,
        email: &str,
        role: &str,
    ) -> anyhow::Result<(String, String, String)> {
        anyhow::ensure!(matches!(role, "admin" | "member"), "invite role must be admin or member");
        let email = normalize_email(email);
        // Defense in depth (proposal 0073 C3) — the handler already answered 400.
        validate_email_address(&email)?;
        let now = now_secs() as i64;
        let expires = now + SHARE_INVITE_TTL;

        // Already a member? No-op success (0040 §4): mint/refresh the row as
        // accepted so the create response stays uniform (no oracle, 0042).
        let already_member = match self.user_id_by_email(&email).await {
            Some(uid) => self.org_member_ids(org_id).await.contains(&uid),
            None => false,
        };

        let existing = sqlx::query("SELECT id, token, status FROM org_invites WHERE org_id = ?1 AND email = ?2")
            .bind(org_id)
            .bind(&email)
            .fetch_optional(&self.pool)
            .await?;

        if let Some(row) = &existing {
            let id: String = row.try_get("id")?;
            let token: String = row.try_get("token")?;
            let status: String = row.try_get("status")?;
            if status == "accepted" || already_member {
                return Ok((id, "accepted".into(), token));
            }
            // pending/terminal → (re)offer: refresh to pending with a FRESH token
            // (the old link dies) and a fresh TTL, like email_invite_create. The
            // delivery receipt is cleared in the same statement (proposal 0073
            // B2), so a re-invite starts clean rather than showing attempt one's.
            // The org-scoped live-invite cap (0073 C1). Reached only on the
            // (re)offer path — the `accepted` arm above returns first, and rightly
            // so: it adds no pending row. That exemption is not an
            // account-existence oracle either, since "already a member" is a fact
            // about the caller's OWN org, which `/api/orgs/mine` already shows
            // them; for every address that is not a member, the cap fires
            // identically whether or not that address has an account.
            self.assert_org_invite_headroom(org_id, &id).await?;
            let token = cc_screen_auth::generate_token();
            sqlx::query(
                "UPDATE org_invites
                    SET status = 'pending', role = ?1, token = ?2, inviter_user_id = ?3,
                        created_at = ?4, expires_at = ?5, responded_at = NULL,
                        delivery = NULL, emailed_at = NULL
                  WHERE id = ?6",
            )
            .bind(role)
            .bind(&token)
            .bind(inviter)
            .bind(now)
            .bind(expires)
            .bind(&id)
            .execute(&self.pool)
            .await?;
            return Ok((id, "pending".into(), token));
        }

        if !already_member {
            self.assert_org_invite_headroom(org_id, "").await?;
        }
        let id = cc_screen_auth::generate_token();
        let token = cc_screen_auth::generate_token();
        let status = if already_member { "accepted" } else { "pending" };
        sqlx::query(
            "INSERT INTO org_invites
                (id, token, org_id, inviter_user_id, email, role, status, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )
        .bind(&id)
        .bind(&token)
        .bind(org_id)
        .bind(inviter)
        .bind(&email)
        .bind(role)
        .bind(status)
        .bind(now)
        .bind(expires)
        .execute(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!("org_invite_create: {e}"))?;
        Ok((id, status.into(), token))
    }

    async fn org_invite_inbox(&self, email: &str) -> Vec<OrgInviteRow> {
        let email = normalize_email(email);
        let now = now_secs() as i64;
        // Lazy expiry, like share_inbox.
        let _ = sqlx::query(
            "UPDATE org_invites SET status = 'expired' WHERE status = 'pending' AND expires_at < ?1",
        )
        .bind(now)
        .execute(&self.pool)
        .await;
        let rows = sqlx::query(
            "SELECT * FROM org_invites WHERE email = ?1 AND status = 'pending' ORDER BY created_at DESC",
        )
        .bind(&email)
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();
        rows.iter().filter_map(org_invite_row).collect()
    }

    async fn org_invite_outbox(&self, org_id: &str) -> Vec<OrgInviteRow> {
        let now = now_secs() as i64;
        let _ = sqlx::query(
            "UPDATE org_invites SET status = 'expired' WHERE status = 'pending' AND expires_at < ?1",
        )
        .bind(now)
        .execute(&self.pool)
        .await;
        let rows = sqlx::query("SELECT * FROM org_invites WHERE org_id = ?1 ORDER BY created_at DESC")
            .bind(org_id)
            .fetch_all(&self.pool)
            .await
            .unwrap_or_default();
        rows.iter().filter_map(org_invite_row).collect()
    }

    async fn org_invite_respond(&self, user_id: &str, id: &str, accept: bool) -> anyhow::Result<ShareOutcome> {
        let Some(inv) = self.org_invite_get(id).await else {
            return Ok(ShareOutcome::NotFound);
        };
        // Addressed to someone else's email ⇒ 404 (don't leak existence).
        if self.user_email(user_id).await.as_deref() != Some(inv.email.as_str()) {
            return Ok(ShareOutcome::NotFound);
        }
        let now = now_secs() as i64;
        let status = if inv.status == "pending" && inv.expires_at < now {
            let _ = sqlx::query("UPDATE org_invites SET status = 'expired' WHERE id = ?1")
                .bind(id)
                .execute(&self.pool)
                .await;
            "expired".to_string()
        } else {
            inv.status.clone()
        };
        let target = if accept { "accepted" } else { "declined" };
        if status == target {
            return Ok(ShareOutcome::Ok(status)); // idempotent no-op (0040 §4)
        }
        if status != "pending" {
            return Ok(ShareOutcome::Conflict);
        }

        if !accept {
            let _ = sqlx::query(
                "UPDATE org_invites SET status = 'declined', responded_at = ?1 WHERE id = ?2 AND status = 'pending'",
            )
            .bind(now)
            .bind(id)
            .execute(&self.pool)
            .await;
            self.audit_append(&inv.org_id, Some(user_id), "invite.declined", Some(&inv.email), None).await;
            return Ok(ShareOutcome::Ok("declined".into()));
        }

        // One-org check up front for the honest 409; the UNIQUE(user_id)
        // constraint is the race backstop below.
        if let Some((org, _)) = self.org_for_user(user_id).await {
            if org.id == inv.org_id {
                // Already a member (e.g. a raced double-accept): converge.
                let _ = sqlx::query("UPDATE org_invites SET status = 'accepted', responded_at = ?1 WHERE id = ?2")
                    .bind(now)
                    .bind(id)
                    .execute(&self.pool)
                    .await;
                return Ok(ShareOutcome::Ok("accepted".into()));
            }
            anyhow::bail!("ONEORG:you're already in another team — leave it first");
        }
        let org = self
            .org_get(&inv.org_id)
            .await
            .ok_or_else(|| anyhow::anyhow!("no such team"))?;
        if org.seat_count == 0 {
            // Covers both the dormant pre-billing org and the dead subscription
            // (0064 A4 zeroes seat_count on subscription.deleted).
            anyhow::bail!("SEATS:This team has no seats yet. An admin needs to set up billing (or seats) first.");
        }

        // Accept, in one transaction (0063 A2): the guarded INSERT is the
        // seat-gate serialization point — the member count is re-checked in the
        // same statement that inserts, so two accepts racing onto the last seat
        // can't both land.
        let mut tx = self.pool.begin().await?;
        let res = sqlx::query(
            "INSERT INTO org_members (org_id, user_id, role, created_at)
             SELECT ?1, ?2, ?3, ?4
              WHERE (SELECT count(*) FROM org_members WHERE org_id = ?1)
                    < (SELECT seat_count FROM orgs WHERE id = ?1)",
        )
        .bind(&inv.org_id)
        .bind(user_id)
        .bind(&inv.role)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(|_| anyhow::anyhow!("ONEORG:you're already in another team — leave it first"))?;
        if res.rows_affected() == 0 {
            anyhow::bail!(
                "SEATS:This team is out of seats ({}). An admin can add seats from Billing.",
                org.seat_count
            );
        }
        sqlx::query("UPDATE org_invites SET status = 'accepted', responded_at = ?1 WHERE id = ?2 AND status = 'pending'")
            .bind(now)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        self.audit_append(&inv.org_id, Some(user_id), "member.joined", Some(&inv.email), Some(&format!("{{\"role\":\"{}\"}}", inv.role))).await;
        // Membership activated ⇒ materialize team visibility both directions
        // (0065 A3). Idempotent set-repair; the nightly reconcile is the backstop.
        self.team_shares_materialize(&inv.org_id).await;
        Ok(ShareOutcome::Ok("accepted".into()))
    }

    async fn org_invite_revoke(&self, org_id: &str, id: &str) -> ShareOutcome {
        let Some(inv) = self.org_invite_get(id).await else { return ShareOutcome::NotFound };
        if inv.org_id != org_id {
            return ShareOutcome::NotFound;
        }
        // Forgiving on already-dead rows (0040 §4); an accepted invite is a
        // member now — removal is its own action, so revoke conflicts.
        if matches!(inv.status.as_str(), "revoked" | "declined" | "expired") {
            return ShareOutcome::Ok(inv.status);
        }
        if inv.status == "accepted" {
            return ShareOutcome::Conflict;
        }
        let _ = sqlx::query("UPDATE org_invites SET status = 'revoked', responded_at = ?1 WHERE id = ?2")
            .bind(now_secs() as i64)
            .bind(id)
            .execute(&self.pool)
            .await;
        ShareOutcome::Ok("revoked".into())
    }

    async fn org_invite_by_token(&self, token: &str) -> Option<OrgInviteRow> {
        let now = now_secs() as i64;
        let row = sqlx::query("SELECT * FROM org_invites WHERE token = ?1 AND expires_at > ?2 AND status = 'pending'")
            .bind(token)
            .bind(now)
            .fetch_optional(&self.pool)
            .await
            .ok()??;
        org_invite_row(&row)
    }

    async fn org_invite_sweep(&self) {
        let now = now_secs() as i64;
        // Flip overdue pending rows, auditing each expiry with a NULL actor.
        let overdue = sqlx::query(
            "SELECT id, org_id, email FROM org_invites WHERE status = 'pending' AND expires_at < ?1",
        )
        .bind(now)
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();
        for row in &overdue {
            let (Ok(id), Ok(org_id), Ok(email)) = (
                row.try_get::<String, _>("id"),
                row.try_get::<String, _>("org_id"),
                row.try_get::<String, _>("email"),
            ) else {
                continue;
            };
            let _ = sqlx::query("UPDATE org_invites SET status = 'expired' WHERE id = ?1 AND status = 'pending'")
                .bind(&id)
                .execute(&self.pool)
                .await;
            self.audit_append(&org_id, None, "invite.expired", Some(&email), None).await;
        }
        // Hard-delete long-dead terminal rows (keep accepted — membership history).
        let _ = sqlx::query(
            "DELETE FROM org_invites
              WHERE status IN ('declined','revoked','expired')
                AND COALESCE(responded_at, created_at) < ?1",
        )
        .bind(now - SHARE_INVITE_REAP_AFTER)
        .execute(&self.pool)
        .await;
    }

    // ── invite delivery (proposal 0073 B2) ────────────────────────────────────

    async fn org_invite_mark_sending(&self, id: &str, token: &str) -> bool {
        sqlx::query(
            "UPDATE org_invites SET delivery = 'sending'
              WHERE id = ?1 AND token = ?2 AND status = 'pending'",
        )
        .bind(id)
        .bind(token)
        .execute(&self.pool)
        .await
        .map(|r| r.rows_affected() > 0)
        .unwrap_or(false)
    }

    async fn org_invite_delivery_record(&self, id: &str, token: &str, delivery: &str) {
        if let Err(e) = sqlx::query(
            "UPDATE org_invites SET delivery = ?1, emailed_at = ?2 WHERE id = ?3 AND token = ?4",
        )
        .bind(delivery)
        .bind(now_secs() as i64)
        .bind(id)
        .bind(token)
        .execute(&self.pool)
        .await
        {
            tracing::warn!("org_invite_delivery_record({id}): {e}");
        }
    }

    async fn email_invite_mark_sending(&self, id: &str, token: &str) -> bool {
        // No status column here: a revoke DELETEs the row and a re-offer mints a
        // fresh token on it, so `(id, token)` is the whole liveness check.
        sqlx::query("UPDATE email_invites SET delivery = 'sending' WHERE id = ?1 AND token = ?2")
            .bind(id)
            .bind(token)
            .execute(&self.pool)
            .await
            .map(|r| r.rows_affected() > 0)
            .unwrap_or(false)
    }

    async fn email_invite_delivery_record(&self, id: &str, token: &str, delivery: &str) {
        if let Err(e) = sqlx::query(
            "UPDATE email_invites SET delivery = ?1, emailed_at = ?2 WHERE id = ?3 AND token = ?4",
        )
        .bind(delivery)
        .bind(now_secs() as i64)
        .bind(id)
        .bind(token)
        .execute(&self.pool)
        .await
        {
            tracing::warn!("email_invite_delivery_record({id}): {e}");
        }
    }

    async fn invite_delivery_sweep(&self) {
        // A row whose attempt outlived the process: `sending` is the pre-attempt
        // stamp, so anything still wearing it long after the row was (re)created
        // belongs to a task that no longer exists. COALESCE because `emailed_at`
        // is NULL until an attempt settles — the create time is the attempt time.
        let cutoff = now_secs() as i64 - DELIVERY_STUCK_AFTER;
        for table in ["org_invites", "email_invites"] {
            let sql = format!(
                "UPDATE {table} SET delivery = 'failed', emailed_at = ?1
                  WHERE delivery = 'sending' AND COALESCE(emailed_at, created_at) < ?2"
            );
            let _ = sqlx::query(&sql).bind(now_secs() as i64).bind(cutoff).execute(&self.pool).await;
        }
    }

    async fn invites_emailed_since(&self, since: i64) -> i64 {
        sqlx::query(
            "SELECT (SELECT count(*) FROM org_invites   WHERE emailed_at > ?1)
                  + (SELECT count(*) FROM email_invites WHERE emailed_at > ?1) AS n",
        )
        .bind(since)
        .fetch_one(&self.pool)
        .await
        .ok()
        .and_then(|r| r.try_get::<i64, _>("n").ok())
        .unwrap_or(0)
    }

    async fn org_invites_emailed_since(&self, org_id: &str, since: i64) -> i64 {
        sqlx::query(
            "SELECT (SELECT count(*) FROM org_invites
                      WHERE org_id = ?1 AND emailed_at > ?2)
                  + (SELECT count(*) FROM email_invites e
                       JOIN org_members m ON m.user_id = e.inviter_user_id
                      WHERE m.org_id = ?1 AND e.emailed_at > ?2) AS n",
        )
        .bind(org_id)
        .bind(since)
        .fetch_one(&self.pool)
        .await
        .ok()
        .and_then(|r| r.try_get::<i64, _>("n").ok())
        .unwrap_or(0)
    }

    async fn set_team_visible(&self, owner_user_id: &str, agent_id: &str, visible: bool) -> bool {
        sqlx::query("UPDATE agents SET team_visible = ?1 WHERE id = ?2 AND user_id = ?3")
            .bind(visible as i64)
            .bind(agent_id)
            .bind(owner_user_id)
            .execute(&self.pool)
            .await
            .map(|r| r.rows_affected() > 0)
            .unwrap_or(false)
    }

    async fn audit_append(
        &self,
        org_id: &str,
        actor: Option<&str>,
        action: &str,
        target: Option<&str>,
        detail: Option<&str>,
    ) {
        // Fire-and-forget: the audit log is an accountability trail, not a
        // ledger — it must never fail or slow the mutation it records (0063 D1).
        if let Err(e) = sqlx::query(
            "INSERT INTO audit_log (org_id, at, actor_user_id, action, target, detail)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .bind(org_id)
        .bind(now_secs() as i64)
        .bind(actor)
        .bind(action)
        .bind(target)
        .bind(detail)
        .execute(&self.pool)
        .await
        {
            tracing::warn!("audit_append({action}) failed: {e}");
        }
    }

    async fn audit_page(&self, org_id: &str, before: Option<i64>, limit: i64) -> Vec<AuditRow> {
        let limit = limit.clamp(1, 200);
        let rows = sqlx::query(
            "SELECT * FROM audit_log
              WHERE org_id = ?1 AND (?2 IS NULL OR id < ?2)
              ORDER BY id DESC LIMIT ?3",
        )
        .bind(org_id)
        .bind(before)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();
        rows.iter()
            .filter_map(|r| {
                Some(AuditRow {
                    id: r.try_get("id").ok()?,
                    org_id: r.try_get("org_id").ok()?,
                    at: r.try_get("at").ok()?,
                    actor_user_id: r.try_get::<Option<String>, _>("actor_user_id").ok().flatten(),
                    action: r.try_get("action").ok()?,
                    target: r.try_get::<Option<String>, _>("target").ok().flatten(),
                    detail: r.try_get::<Option<String>, _>("detail").ok().flatten(),
                })
            })
            .collect()
    }

    async fn agent_count_for_users(&self, user_ids: &[String]) -> i64 {
        if user_ids.is_empty() {
            return 0;
        }
        let placeholders = vec!["?"; user_ids.len()].join(",");
        let sql = format!("SELECT count(*) AS n FROM agents WHERE user_id IN ({placeholders})");
        let mut q = sqlx::query(&sql);
        for id in user_ids {
            q = q.bind(id);
        }
        q.fetch_one(&self.pool)
            .await
            .ok()
            .and_then(|r| r.try_get::<i64, _>("n").ok())
            .unwrap_or(0)
    }

    async fn user_founder_cohort(&self, user_id: &str) -> bool {
        sqlx::query("SELECT 1 AS x FROM users WHERE id = ?1 AND (plan = 'beta' OR prior_plan = 'beta')")
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten()
            .is_some()
    }

    async fn org_by_billing_customer(&self, customer_id: &str) -> Option<String> {
        sqlx::query("SELECT id FROM orgs WHERE billing_customer_id = ?1")
            .bind(customer_id)
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten()
            .and_then(|r| r.try_get("id").ok())
    }

    async fn org_billing_rows_for_reconcile(&self) -> Vec<OrgBillingRow> {
        let rows = sqlx::query(
            "SELECT o.id, o.billing_customer_id, o.billing_subscription_id, o.plan,
                    o.plan_status, o.seat_count,
                    (SELECT count(*) FROM org_members m WHERE m.org_id = o.id) AS member_count
               FROM orgs o
              WHERE o.billing_customer_id IS NOT NULL OR o.plan_status IS NOT NULL",
        )
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();
        rows.iter()
            .filter_map(|r| {
                Some(OrgBillingRow {
                    org_id: r.try_get("id").ok()?,
                    customer_id: r.try_get::<Option<String>, _>("billing_customer_id").ok().flatten(),
                    subscription_id: r.try_get::<Option<String>, _>("billing_subscription_id").ok().flatten(),
                    plan: r.try_get("plan").ok()?,
                    plan_status: r.try_get::<Option<String>, _>("plan_status").ok().flatten(),
                    seat_count: r.try_get("seat_count").ok()?,
                    member_count: r.try_get("member_count").ok()?,
                })
            })
            .collect()
    }

    async fn share_is_team(&self, share_id: &str) -> bool {
        sqlx::query("SELECT 1 AS x FROM shares WHERE id = ?1 AND kind = 'team'")
            .bind(share_id)
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten()
            .is_some()
    }

    async fn team_shares_materialize(&self, org_id: &str) {
        // Cross-join of members × fellow-members' visible agents (0065 A3);
        // INSERT OR IGNORE rides idx_shares_team. Row count is |members|·|agents|
        // — tens to low hundreds at the 3–20-seat scale this tier targets.
        if let Err(e) = sqlx::query(
            "INSERT OR IGNORE INTO shares (id, agent_id, grantee_user_id, kind, session,
                                           owner_peek, created_at, org_id)
             SELECT lower(hex(randomblob(16))), a.id, m.user_id, 'team', NULL, 0, ?1, ?2
               FROM org_members m
               JOIN org_members o ON o.org_id = m.org_id
               JOIN agents a      ON a.user_id = o.user_id AND a.team_visible = 1
              WHERE m.org_id = ?2 AND m.user_id != o.user_id",
        )
        .bind(now_secs() as i64)
        .bind(org_id)
        .execute(&self.pool)
        .await
        {
            tracing::warn!("team_shares_materialize({org_id}) failed: {e}");
        }
    }

    async fn team_shares_prune(&self, org_id: &str) {
        // The inverse DELETE: every team row of this org whose (agent, grantee)
        // pair is no longer implied by current membership + opt-out flags.
        // Personal 0039 shares (org_id NULL) are untouched by construction.
        if let Err(e) = sqlx::query(
            "DELETE FROM shares
              WHERE kind = 'team' AND org_id = ?1
                AND NOT EXISTS (
                    SELECT 1
                      FROM org_members m
                      JOIN org_members o ON o.org_id = m.org_id
                      JOIN agents a      ON a.user_id = o.user_id AND a.team_visible = 1
                     WHERE m.org_id = ?1
                       AND m.user_id = shares.grantee_user_id
                       AND m.user_id != o.user_id
                       AND a.id = shares.agent_id
                )",
        )
        .bind(org_id)
        .execute(&self.pool)
        .await
        {
            tracing::warn!("team_shares_prune({org_id}) failed: {e}");
        }
    }

    async fn team_shares_reconcile(&self) {
        // Nightly invariant-restorer (0065 A3): a missed hook self-heals. Orphan
        // team rows of a deleted org are already reaped by ON DELETE CASCADE.
        let orgs: Vec<String> = sqlx::query("SELECT id FROM orgs")
            .fetch_all(&self.pool)
            .await
            .unwrap_or_default()
            .iter()
            .filter_map(|r| r.try_get("id").ok())
            .collect();
        for org_id in orgs {
            self.team_shares_materialize(&org_id).await;
            self.team_shares_prune(&org_id).await;
        }
    }

    async fn share_sweep(&self) {
        let now = now_secs() as i64;
        // Flip overdue pending rows.
        let _ = sqlx::query(
            "UPDATE share_invites SET status = 'expired'
              WHERE status = 'pending' AND expires_at IS NOT NULL AND expires_at < ?1",
        )
        .bind(now)
        .execute(&self.pool)
        .await;
        // Hard-delete long-dead terminal rows (keep accepted ones — they are the
        // live grant's lifecycle record).
        let _ = sqlx::query(
            "DELETE FROM share_invites
              WHERE status IN ('declined','revoked','expired')
                AND COALESCE(responded_at, created_at) < ?1",
        )
        .bind(now - SHARE_INVITE_REAP_AFTER)
        .execute(&self.pool)
        .await;
        // Email invites (proposal 0056 C3): reap expired unconverted rows and
        // long-converted ones (their lifecycle lives in share_invites now).
        let _ = sqlx::query(
            "DELETE FROM email_invites
              WHERE (converted_at IS NULL AND expires_at < ?1)
                 OR (converted_at IS NOT NULL AND converted_at < ?2)",
        )
        .bind(now)
        .bind(now - SHARE_INVITE_REAP_AFTER)
        .execute(&self.pool)
        .await;
    }

    async fn device_sweep(&self) {
        let now = now_secs() as i64;
        let _ = sqlx::query(
            "DELETE FROM device_enrollments
              WHERE expires_at < ?1
                 OR (status = 'approved' AND last_polled_at IS NOT NULL AND last_polled_at < ?2)",
        )
        .bind(now)
        .bind(now - 3600) // approved-but-never-claimed ages out after an hour
        .execute(&self.pool)
        .await;
    }
}

/// Per RFC 8628 §3.5: ~10-minute code lifetime, and the host waits this long
/// between polls.
const DEVICE_CODE_TTL: i64 = 600;
const DEVICE_POLL_INTERVAL: i64 = 5;

/// A pending share invite lives 14 days before it lazily expires (proposal 0040 §7).
const SHARE_INVITE_TTL: i64 = 14 * 86_400;
/// Dead terminal invite rows are hard-deleted by the sweep this long after they
/// settled, bounding table growth without yanking a just-declined row from a view.
const SHARE_INVITE_REAP_AFTER: i64 = 7 * 86_400;
/// Live (unconverted, unexpired) email invites per inviter (proposal 0056 C2's
/// abuse bound); over it the handler answers 429.
const EMAIL_INVITE_CAP: i64 = 20;
/// Live (pending, unexpired) invitations per **org** (proposal 0073 C1) — the
/// `org_invites` twin of [`EMAIL_INVITE_CAP`], org-scoped rather than
/// inviter-scoped for the reasons on [`SqliteStore::assert_org_invite_headroom`].
/// Comfortably above any real team's onboarding batch; over it the handler
/// answers 429 through `store_err`'s `CAP:` arm.
const ORG_INVITE_CAP: i64 = 25;
/// Invitations one org may have *emailed* in a rolling 24h (proposal 0073 C1).
/// Unlike the two live-invite caps this bounds mail, not rows: over it the
/// invitation is still created and the copyable link still returned — only the
/// send is skipped. Inert on a hub with no mailer (nothing is ever sent).
pub const ORG_MAIL_PER_DAY: i64 = 100;
/// The same ceiling for the whole hub, sized under Brevo's free-tier 300/day
/// account quota with headroom for the password-reset mail this transport will
/// eventually also carry. The per-org ceiling cannot substitute for it: the
/// quota is per relay *account*, so N orgs each under their own ceiling still
/// exhaust it.
pub const HUB_MAIL_PER_DAY: i64 = 250;
/// Public-signup password minimum, aligned with the CCWEB_PASSWORD warning bar
/// (proposal 0053 Part E).
pub const MIN_PASSWORD_LEN: usize = 12;
/// How long a `delivery = 'sending'` stamp may stand before the sweep calls it
/// `failed` (proposal 0073 B2). Comfortably past the mailer's 20s `SEND_TIMEOUT`,
/// so only an attempt whose process is gone is ever fail-stamped.
const DELIVERY_STUCK_AFTER: i64 = 300;

/// The one normalization every email-keyed lookup and insert goes through.
/// `pub(crate)` so the two invite handlers can address the mail to the same
/// string the row is keyed by (proposal 0073 B1).
pub(crate) fn normalize_email(email: &str) -> String {
    email.trim().to_lowercase()
}

/// Is this a C0 control character (U+0000–U+001F) or DEL? These are the header
/// -injection alphabet: a bare `\r`/`\n` in a value the mailer interpolates can
/// start a header of the attacker's choosing (`Bcc:` is the classic).
pub(crate) fn is_control_char(c: char) -> bool {
    (c as u32) < 0x20 || c == '\u{7f}'
}

/// The address check the two invite endpoints run **at the API boundary**, so a
/// malformed address answers 400 before an invite row is written (proposal 0073
/// C3 — before this, `normalize_email` was `trim().to_lowercase()` and the only
/// other check was `!is_empty()`, so `victim@x.com\r\nBcc: …` was accepted and
/// persisted, with the browser's `<input type="email">` the entire defense).
///
/// The rules, applied to the normalized (trimmed, lowercased) value:
///   * non-empty, and at most 254 bytes (RFC 5321's path limit);
///   * **exactly one** `@`, with a non-empty local part and a non-empty domain;
///   * no C0 control character and no DEL anywhere;
///   * no `,` and no `;` — the address-list separators;
///   * no whitespace, `<` or `>` — i.e. a **bare** address, never the
///     `Display Name <a@b.com>` form. That form passes every other rule here and
///     `lettre` accepts it, but the envelope would carry `a@b.com` while the
///     `To:` header carried attacker-chosen display text into the invitee's
///     client, and the *stored* value would stop being a bare address — which
///     matters because the accept flow compares an authenticated account's email
///     against exactly this string ([0056] C4).
///
/// Note what it deliberately does **not** require: a dot in the domain. A
/// self-hosted hub on a corporate LAN legitimately invites `bob@intranet`, and
/// rejecting that would break a hub that sends no mail at all. Deliverability is
/// not this function's job; injection defense is. `lettre`'s typed builder would
/// also catch these, but a bad address must never reach the database in the first
/// place, and resting a whole class of injection on one library's parser is an
/// assumption that survives right up until someone switches transports.
pub(crate) fn validate_email_address(email: &str) -> anyhow::Result<()> {
    anyhow::ensure!(!email.is_empty(), "email is required");
    anyhow::ensure!(email.len() <= 254, "email address is too long");
    anyhow::ensure!(
        !email.chars().any(is_control_char),
        "email address contains a control character"
    );
    anyhow::ensure!(
        !email.contains(',') && !email.contains(';'),
        "email address contains a separator character"
    );
    anyhow::ensure!(
        !email.chars().any(char::is_whitespace) && !email.contains('<') && !email.contains('>'),
        "email address must be a plain address, without a display name"
    );
    let mut parts = email.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
        anyhow::bail!("email address must contain exactly one @");
    };
    anyhow::ensure!(!local.is_empty() && !domain.is_empty(), "email address is not a valid address");
    Ok(())
}

/// The org-name check (proposal 0073 C3): the name is interpolated into an
/// invitation's subject and body, so a `\r\n` in it is the same injection vector
/// as one in the address. `org_create` accepted any characters within 80 —
/// control codes included — before this.
pub(crate) fn validate_org_name(name: &str) -> anyhow::Result<()> {
    anyhow::ensure!(!name.trim().is_empty(), "a team name is required");
    anyhow::ensure!(name.len() <= 80, "team name too long");
    anyhow::ensure!(!name.chars().any(is_control_char), "team name contains a control character");
    Ok(())
}

/// A user code as typed/stored: uppercased with separators stripped, so "wdjb-mjht"
/// and "WDJB MJHT" both match the stored "WDJBMJHT".
fn normalize_user_code(code: &str) -> String {
    code.chars().filter(|c| c.is_ascii_alphanumeric()).flat_map(|c| c.to_uppercase()).collect()
}

/// A fresh 8-char user code in Crockford base32 with ambiguous glyphs
/// (I/L/O/U/0/1) removed, returned grouped for display ("WDJB-MJHT").
fn gen_user_code() -> String {
    use argon2::password_hash::rand_core::{OsRng, RngCore};
    const ALPHABET: &[u8] = b"23456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut buf = [0u8; 8];
    OsRng.fill_bytes(&mut buf);
    let c: Vec<char> = buf.iter().map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char).collect();
    format!("{}{}{}{}-{}{}{}{}", c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7])
}

/// Map a `shares ⋈ agents` result row to a [`ShareRow`]; `None` if a column is
/// missing/typed wrong (skipped rather than failing the whole query).
fn share_row(r: &sqlx::sqlite::SqliteRow) -> Option<ShareRow> {
    Some(ShareRow {
        id: r.try_get("id").ok()?,
        agent_id: r.try_get("agent_id").ok()?,
        owner_user_id: r.try_get("owner_user_id").ok()?,
        grantee_user_id: r.try_get("grantee_user_id").ok()?,
        kind: r.try_get("kind").ok()?,
        session: r.try_get::<Option<String>, _>("session").ok().flatten().filter(|s| !s.is_empty()),
        owner_peek: r.try_get::<i64, _>("owner_peek").ok()? != 0,
        created_at: r.try_get("created_at").ok()?,
        org_id: r.try_get::<Option<String>, _>("org_id").ok().flatten(),
    })
}

/// Map an `orgs` result row to an [`OrgRow`].
fn org_row(r: &sqlx::sqlite::SqliteRow) -> Option<OrgRow> {
    Some(OrgRow {
        id: r.try_get("id").ok()?,
        name: r.try_get("name").ok()?,
        plan: r.try_get("plan").ok()?,
        created_at: r.try_get("created_at").ok()?,
        seat_count: r.try_get("seat_count").unwrap_or(0),
        billing_customer_id: r.try_get::<Option<String>, _>("billing_customer_id").ok().flatten(),
        billing_subscription_id: r.try_get::<Option<String>, _>("billing_subscription_id").ok().flatten(),
        plan_status: r.try_get::<Option<String>, _>("plan_status").ok().flatten(),
        current_period_end: r.try_get::<Option<i64>, _>("current_period_end").ok().flatten(),
    })
}

/// Map an `org_invites` result row to an [`OrgInviteRow`].
fn org_invite_row(r: &sqlx::sqlite::SqliteRow) -> Option<OrgInviteRow> {
    Some(OrgInviteRow {
        id: r.try_get("id").ok()?,
        token: r.try_get("token").ok()?,
        org_id: r.try_get("org_id").ok()?,
        inviter_user_id: r.try_get("inviter_user_id").ok()?,
        email: r.try_get("email").ok()?,
        role: r.try_get("role").ok()?,
        status: r.try_get("status").ok()?,
        created_at: r.try_get("created_at").ok()?,
        responded_at: r.try_get::<Option<i64>, _>("responded_at").ok().flatten(),
        expires_at: r.try_get("expires_at").ok()?,
        emailed_at: r.try_get::<Option<i64>, _>("emailed_at").ok().flatten(),
        delivery: r.try_get::<Option<String>, _>("delivery").ok().flatten(),
    })
}

/// Map a `share_invites` result row to a [`ShareInviteRow`].
fn share_invite_row(r: &sqlx::sqlite::SqliteRow) -> Option<ShareInviteRow> {
    Some(ShareInviteRow {
        id: r.try_get("id").ok()?,
        inviter_user_id: r.try_get("inviter_user_id").ok()?,
        grantee_user_id: r.try_get("grantee_user_id").ok()?,
        resource_kind: r.try_get("resource_kind").ok()?,
        agent_id: r.try_get("agent_id").ok()?,
        session_name: r.try_get::<Option<String>, _>("session_name").ok().flatten().filter(|s| !s.is_empty()),
        owner_peek: r.try_get::<i64, _>("owner_peek").ok()? != 0,
        status: r.try_get("status").ok()?,
        created_at: r.try_get("created_at").ok()?,
        responded_at: r.try_get::<Option<i64>, _>("responded_at").ok().flatten(),
        expires_at: r.try_get::<Option<i64>, _>("expires_at").ok().flatten(),
    })
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// The billing state write (proposal 0058 B3), on a caller-supplied connection so
/// the webhook can run it inside the idempotency transaction and the reconcile can
/// run it on a pooled connection. See [`Store::apply_subscription_state`] for the
/// `plan`/`subscription_id`/`customer_id` semantics.
async fn apply_sub_state(
    conn: &mut sqlx::SqliteConnection,
    user_id: &str,
    plan: Option<&str>,
    status: &str,
    subscription_id: Option<&str>,
    customer_id: Option<&str>,
    period_end: Option<i64>,
) -> anyhow::Result<()> {
    match plan {
        Some(p) => {
            // Guard the target plan exists (like set_plan) — a bad price→plan map
            // can never strand a user on an unknown plan.
            let known = sqlx::query("SELECT 1 AS x FROM plan_limits WHERE plan = ?1")
                .bind(p)
                .fetch_optional(&mut *conn)
                .await?
                .is_some();
            anyhow::ensure!(known, "unknown plan '{p}'");
            // `prior_plan` and `plan` on the RHS read the row's PRE-update values
            // (SQLite semantics), so this stamps the plan the user was on when the
            // subscription first activated — once (COALESCE), never overwritten.
            sqlx::query(
                "UPDATE users
                    SET prior_plan = COALESCE(prior_plan, plan),
                        plan = ?1,
                        plan_status = ?2,
                        billing_subscription_id = ?3,
                        billing_customer_id = COALESCE(?4, billing_customer_id),
                        current_period_end = ?5
                  WHERE id = ?6",
            )
            .bind(p)
            .bind(status)
            .bind(subscription_id)
            .bind(customer_id)
            .bind(period_end)
            .bind(user_id)
            .execute(&mut *conn)
            .await?;
        }
        None => {
            // Terminal/cancel: restore COALESCE(prior_plan,'free'), guarding a
            // stale prior_plan against the current plan table. `prior_plan`
            // untouched; subscription id cleared; customer id kept.
            sqlx::query(
                "UPDATE users
                    SET plan = CASE
                                 WHEN prior_plan IS NOT NULL
                                  AND prior_plan IN (SELECT plan FROM plan_limits)
                                 THEN prior_plan ELSE 'free' END,
                        plan_status = ?1,
                        billing_subscription_id = ?2,
                        billing_customer_id = COALESCE(?3, billing_customer_id),
                        current_period_end = ?4
                  WHERE id = ?5",
            )
            .bind(status)
            .bind(subscription_id)
            .bind(customer_id)
            .bind(period_end)
            .bind(user_id)
            .execute(&mut *conn)
            .await?;
        }
    }
    Ok(())
}

/// The org-side billing write (proposal 0064 B4), the `apply_sub_state` twin. A
/// paid plan mirrors `seat_count` from the re-fetched quantity; the terminal
/// path zeroes seats (the entitlement gate, 0063 C1) but keeps `plan = 'team'`
/// (the column is NOT NULL and a zero-seat org confers nothing anyway), clears
/// the subscription id, and keeps the customer id for resubscribe. Membership
/// rows are never touched — enforcement compares at creation, never reconciles
/// what exists (0064 A4).
#[allow(clippy::too_many_arguments)]
async fn apply_org_sub_state(
    conn: &mut sqlx::SqliteConnection,
    org_id: &str,
    plan: Option<&str>,
    status: &str,
    subscription_id: Option<&str>,
    customer_id: Option<&str>,
    period_end: Option<i64>,
    seat_count: Option<i64>,
) -> anyhow::Result<()> {
    match plan {
        Some(p) => {
            // Guard the target plan exists (the set_plan/apply_sub_state
            // convention) — a bad price→plan map can never strand an org.
            let known = sqlx::query("SELECT 1 AS x FROM plan_limits WHERE plan = ?1")
                .bind(p)
                .fetch_optional(&mut *conn)
                .await?
                .is_some();
            anyhow::ensure!(known, "unknown plan '{p}'");
            sqlx::query(
                "UPDATE orgs
                    SET plan = ?1,
                        plan_status = ?2,
                        billing_subscription_id = ?3,
                        billing_customer_id = COALESCE(?4, billing_customer_id),
                        current_period_end = ?5,
                        seat_count = COALESCE(?6, seat_count)
                  WHERE id = ?7",
            )
            .bind(p)
            .bind(status)
            .bind(subscription_id)
            .bind(customer_id)
            .bind(period_end)
            .bind(seat_count)
            .bind(org_id)
            .execute(&mut *conn)
            .await?;
        }
        None => {
            sqlx::query(
                "UPDATE orgs
                    SET plan_status = ?1,
                        billing_subscription_id = ?2,
                        billing_customer_id = COALESCE(?3, billing_customer_id),
                        current_period_end = ?4,
                        seat_count = 0
                  WHERE id = ?5",
            )
            .bind(status)
            .bind(subscription_id)
            .bind(customer_id)
            .bind(period_end)
            .bind(org_id)
            .execute(&mut *conn)
            .await?;
        }
    }
    Ok(())
}

/// argon2id PHC string for `pw`.
fn hash_password(pw: &str) -> anyhow::Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(pw.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| anyhow::anyhow!("argon2 hash: {e}"))
}

/// Constant-time-ish argon2 verify (the crate handles the comparison).
fn verify_password(pw: &str, phc: &str) -> bool {
    PasswordHash::new(phc)
        .map(|parsed| Argon2::default().verify_password(pw.as_bytes(), &parsed).is_ok())
        .unwrap_or(false)
}

/// Burn one Argon2 verification against a fixed hash so a login for an unknown
/// email costs the same as one for a real account (no timing oracle, proposal
/// 0053 Part E). The hash is computed once per process with the same default
/// parameters real accounts use. Always "fails".
fn dummy_verify(pw: &str) {
    static DUMMY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let phc = DUMMY.get_or_init(|| {
        hash_password("cc-screen-dummy-baseline-password").unwrap_or_default()
    });
    let _ = verify_password(pw, phc);
}

/// Map an `email_invites` result row to an [`EmailInviteRow`].
fn link_share_row(r: &sqlx::sqlite::SqliteRow) -> Option<LinkShareRow> {
    Some(LinkShareRow {
        id: r.try_get("id").ok()?,
        agent_id: r.try_get("agent_id").ok()?,
        owner_user_id: r.try_get("owner_user_id").ok()?,
        path: r.try_get("path").ok()?,
        name: r.try_get("name").ok()?,
        created_at: r.try_get("created_at").ok()?,
        expires_at: r.try_get::<Option<i64>, _>("expires_at").ok().flatten(),
    })
}

fn email_invite_row(r: &sqlx::sqlite::SqliteRow) -> Option<EmailInviteRow> {
    Some(EmailInviteRow {
        id: r.try_get("id").ok()?,
        token: r.try_get("token").ok()?,
        inviter_user_id: r.try_get("inviter_user_id").ok()?,
        email: r.try_get("email").ok()?,
        resource_kind: r.try_get("resource_kind").ok()?,
        agent_id: r.try_get("agent_id").ok()?,
        session_name: r.try_get::<Option<String>, _>("session_name").ok().flatten().filter(|s| !s.is_empty()),
        owner_peek: r.try_get::<i64, _>("owner_peek").ok()? != 0,
        created_at: r.try_get("created_at").ok()?,
        expires_at: r.try_get("expires_at").ok()?,
        converted_at: r.try_get::<Option<i64>, _>("converted_at").ok().flatten(),
        emailed_at: r.try_get::<Option<i64>, _>("emailed_at").ok().flatten(),
        delivery: r.try_get::<Option<String>, _>("delivery").ok().flatten(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn create_and_verify_login() {
        let s = SqliteStore::in_memory().await;
        let uid = s.create_user("Alice@Example.com", "correct horse").await.unwrap();
        // Email is normalized (case/space-insensitive), password verified by argon2.
        assert_eq!(s.verify_login("alice@example.com", "correct horse").await.as_deref(), Some(uid.as_str()));
        assert_eq!(s.verify_login(" ALICE@example.com ", "correct horse").await.as_deref(), Some(uid.as_str()));
        assert_eq!(s.verify_login("alice@example.com", "wrong").await, None);
        assert_eq!(s.verify_login("nobody@example.com", "correct horse").await, None);
        assert_eq!(s.user_email(&uid).await.as_deref(), Some("alice@example.com"));
    }

    #[tokio::test]
    async fn duplicate_email_and_short_password_rejected() {
        let s = SqliteStore::in_memory().await;
        s.create_user("a@b.com", "longenough123").await.unwrap();
        assert!(s.create_user("a@b.com", "longenough123").await.is_err(), "duplicate email");
        assert!(s.create_user("c@d.com", "short").await.is_err(), "short password");
        // The public-signup minimum is 12 (proposal 0053 Part E): 11 fails, 12 passes.
        assert!(s.create_user("c@d.com", "elevenchars").await.is_err(), "11 chars rejected");
        assert!(s.create_user("c@d.com", "twelve-chars").await.is_ok(), "12 chars accepted");
    }

    // Proposal 0056 Part C — email invites: create/upsert/cap, attach-on-signup,
    // revoke, token lookup, and the known-user pre-converted variant.
    #[tokio::test]
    async fn email_invites_create_attach_and_revoke() {
        let s = SqliteStore::in_memory().await;
        let alice = s.create_user("alice@x.com", "password12345").await.unwrap();
        let (_t, agent) = s.upsert_agent(&alice, "laptop").await.unwrap();

        // Not-your-agent is rejected; bad kinds are rejected.
        let bob = s.create_user("bob@x.com", "password23456").await.unwrap();
        assert!(s.email_invite_create(&bob, "g@x.com", "agent", &agent, None, false, false).await.is_err());
        assert!(s.email_invite_create(&alice, "g@x.com", "nope", &agent, None, false, false).await.is_err());

        // Create → the row is live, in the outbox, and findable by token.
        let (id, token) =
            s.email_invite_create(&alice, "Ghost@X.com", "agent", &agent, None, true, false).await.unwrap();
        let row = s.email_invite_by_token(&token).await.expect("live token");
        assert_eq!(row.email, "ghost@x.com", "email normalized");
        assert!(row.owner_peek && row.converted_at.is_none());
        assert_eq!(s.email_invite_outbox(&alice).await.len(), 1);

        // Re-offer upserts the same row but mints a fresh token (old link dies).
        let (id2, token2) =
            s.email_invite_create(&alice, "ghost@x.com", "agent", &agent, None, false, false).await.unwrap();
        assert_eq!(id, id2);
        assert_ne!(token, token2);
        assert!(s.email_invite_by_token(&token).await.is_none(), "old token dead");

        // Attach on signup: ghost's new account gets a normal pending 0040 invite.
        let ghost = s.create_user("ghost@x.com", "ghostpass12345").await.unwrap();
        assert_eq!(s.attach_email_invites(&ghost, "ghost@x.com").await, 1);
        let inbox = s.share_inbox(&ghost).await;
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].inviter_user_id, alice);
        // Converted: no longer in the outbox, and a second attach is a no-op.
        assert!(s.email_invite_outbox(&alice).await.is_empty());
        assert_eq!(s.attach_email_invites(&ghost, "ghost@x.com").await, 0);

        // Revoke pre-signup: a fresh invite revoked before signup never attaches.
        let (rid, _rtok) =
            s.email_invite_create(&alice, "ghost2@x.com", "agent", &agent, None, false, false).await.unwrap();
        assert!(!s.email_invite_revoke(&bob, &rid).await, "not the inviter's to revoke");
        assert!(s.email_invite_revoke(&alice, &rid).await);
        let ghost2 = s.create_user("ghost2@x.com", "ghost2pass1234").await.unwrap();
        assert_eq!(s.attach_email_invites(&ghost2, "ghost2@x.com").await, 0, "revoked ⇒ nothing attaches");

        // Known-user variant: pre-converted rows don't attach and don't count
        // toward the cap, but their token still resolves (for the invite link).
        let (_kid, ktok) =
            s.email_invite_create(&alice, "bob@x.com", "agent", &agent, None, false, true).await.unwrap();
        assert!(s.email_invite_by_token(&ktok).await.is_some());
        assert_eq!(s.attach_email_invites(&bob, "bob@x.com").await, 0, "pre-converted never attaches");
    }

    #[tokio::test]
    async fn email_invite_cap_bounds_live_invites() {
        let s = SqliteStore::in_memory().await;
        let alice = s.create_user("alice@x.com", "password12345").await.unwrap();
        let (_t, agent) = s.upsert_agent(&alice, "laptop").await.unwrap();
        for i in 0..EMAIL_INVITE_CAP {
            s.email_invite_create(&alice, &format!("g{i}@x.com"), "agent", &agent, None, false, false)
                .await
                .unwrap();
        }
        let err = s
            .email_invite_create(&alice, "one-too-many@x.com", "agent", &agent, None, false, false)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("CAP:"), "got: {err}");
        // A re-offer of an existing live invite is NOT blocked by the cap.
        assert!(s.email_invite_create(&alice, "g0@x.com", "agent", &agent, None, false, false).await.is_ok());
    }

    /// Proposal 0073 C1: the two holes the 0056 cap left open. The `converted`
    /// (known-account) arm is now subject to the same bound — a cap reachable in
    /// one arm only is an account-existence oracle — and the pre-flight the share
    /// handler runs before it branches agrees with the store, row for row.
    #[tokio::test]
    async fn email_invite_cap_covers_the_known_account_arm() {
        let s = SqliteStore::in_memory().await;
        let alice = s.create_user("alice@x.com", "password12345").await.unwrap();
        let bob = s.create_user("bob@x.com", "password23456").await.unwrap();
        let (_t, agent) = s.upsert_agent(&alice, "laptop").await.unwrap();
        assert!(!s.email_invite_cap_exceeded(&alice, "bob@x.com", "agent", &agent, None).await);
        for i in 0..EMAIL_INVITE_CAP {
            s.email_invite_create(&alice, &format!("g{i}@x.com"), "agent", &agent, None, false, false)
                .await
                .unwrap();
        }
        // The pre-flight and the store agree, in BOTH arms.
        assert!(s.email_invite_cap_exceeded(&alice, "bob@x.com", "agent", &agent, None).await);
        assert!(s.email_invite_cap_exceeded(&alice, "stranger@x.com", "agent", &agent, None).await);
        let err = s
            .email_invite_create(&alice, "bob@x.com", "agent", &agent, None, false, true)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("CAP:"), "the known-account arm is bounded too; got: {err}");
        // …and a re-offer of a live row still isn't, in either place.
        assert!(!s.email_invite_cap_exceeded(&alice, "g0@x.com", "agent", &agent, None).await);
        let _ = bob;
    }

    /// Proposal 0073 C1: `org_invites` gets a live-invite cap, scoped to the ORG
    /// (not the inviter — the re-offer UPDATE reassigns `inviter_user_id`, so a
    /// per-inviter count isn't even stable, and a second admin account would reset
    /// it). Signalled with the `CAP:` prefix `store_err` maps to 429.
    #[tokio::test]
    async fn org_invite_cap_is_org_scoped_and_survives_a_second_admin() {
        let s = SqliteStore::in_memory().await;
        let alice = s.create_user("alice@x.com", "password12345").await.unwrap();
        let bob = s.create_user("bob@x.com", "password23456").await.unwrap();
        let org = s.org_create(&alice, "acme").await.unwrap();
        for i in 0..ORG_INVITE_CAP {
            s.org_invite_create(&org, &alice, &format!("c{i}@x.com"), "member").await.unwrap();
        }
        let err = s
            .org_invite_create(&org, &alice, "one-too-many@x.com", "member")
            .await
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("CAP:"), "got: {err}");
        // A second admin inviting into the SAME org gets the same answer — the cap
        // is not per-inviter, so it can't be reset by switching accounts.
        let err = s.org_invite_create(&org, &bob, "also-too-many@x.com", "member").await.unwrap_err();
        assert!(err.to_string().starts_with("CAP:"), "got: {err}");
        // A re-offer of one of the live invites is never blocked by the cap it is
        // itself a member of (the row under refresh is excluded from the count).
        assert!(s.org_invite_create(&org, &alice, "c0@x.com", "member").await.is_ok());
        // Cancel one and the headroom comes back.
        let outbox = s.org_invite_outbox(&org).await;
        let victim = outbox.iter().find(|r| r.email == "c1@x.com").unwrap().id.clone();
        s.org_invite_revoke(&org, &victim).await;
        assert!(s.org_invite_create(&org, &alice, "fresh@x.com", "member").await.is_ok());
    }

    /// Proposal 0073 C3: the address validator. Injection defense, not
    /// deliverability — note that `bob@intranet` (a self-hosted hub on a corporate
    /// LAN) is deliberately still accepted.
    #[test]
    fn email_address_validator_rejects_injection_not_intranets() {
        for ok in ["bob@intranet", "a@b", "alice@example.com", "a.b+c@sub.example.co.uk"] {
            assert!(validate_email_address(ok).is_ok(), "should accept {ok}");
        }
        for bad in [
            "",
            "victim@x.com\r\nbcc: attacker@x.com",
            "victim@x.com\nbcc: attacker@x.com",
            "victim@x.com\u{7f}",
            "a@b,c@d",
            "a@b;c@d",
            "no-at-sign",
            "two@at@signs",
            "@nolocal.com",
            "nodomain@",
            // The display-name form: one `@`, no control characters, no
            // separators — it passes every other rule and `lettre` accepts it,
            // but the envelope would carry `victim@x.com` while the `To:` header
            // carried attacker-chosen text, and the stored value would stop being
            // the bare address the accept flow compares against.
            "attacker <victim@x.com>",
            "victim@x.com>",
            "bcc: x <victim@x.com>",
            "victim@exa mple.com",
            "victim @x.com",
        ] {
            assert!(validate_email_address(bad).is_err(), "should reject {bad:?}");
        }
        // ≤254 bytes.
        let long = format!("{}@x.com", "a".repeat(250));
        assert!(validate_email_address(&long).is_err(), "254-byte ceiling");
        let just_fits = format!("{}@x.com", "a".repeat(248));
        assert!(validate_email_address(&just_fits).is_ok(), "{} bytes", just_fits.len());
    }

    /// Proposal 0073 C3: an org name is interpolated into an invitation body, so a
    /// control character in it is the same injection vector. Enforced in the store
    /// so the CLI's `org create` is covered too.
    #[tokio::test]
    async fn org_name_rejects_control_characters() {
        assert!(validate_org_name("acme").is_ok());
        assert!(validate_org_name("acme \u{e9}quipe — ok").is_ok());
        assert!(validate_org_name("acme\r\nBcc: attacker@x").is_err());
        assert!(validate_org_name("acme\u{0}").is_err());
        assert!(validate_org_name("   ").is_err());
        let s = SqliteStore::in_memory().await;
        let alice = s.create_user("alice@x.com", "password12345").await.unwrap();
        assert!(s.org_create(&alice, "acme\r\nBcc: attacker@x").await.is_err(), "refused at create");
        assert!(s.org_create(&alice, "acme").await.is_ok());
    }

    /// Proposal 0073 C1: the two send ceilings read `emailed_at`, so they count
    /// only invitations that were actually mailed — a hub with no mailer counts
    /// zero forever and nothing about its behaviour changes.
    #[tokio::test]
    async fn emailed_since_counts_both_tables_and_scopes_to_the_org() {
        let s = SqliteStore::in_memory().await;
        let alice = s.create_user("alice@x.com", "password12345").await.unwrap();
        let (_t, agent) = s.upsert_agent(&alice, "laptop").await.unwrap();
        let org = s.org_create(&alice, "acme").await.unwrap();
        let since = now_secs() as i64 - 86_400;
        assert_eq!(s.invites_emailed_since(since).await, 0, "nothing sent ⇒ no ceiling pressure");
        assert_eq!(s.org_invites_emailed_since(&org, since).await, 0);

        let (oid, _st, otok) = s.org_invite_create(&org, &alice, "carol@x.com", "member").await.unwrap();
        s.org_invite_delivery_record(&oid, &otok, "sent").await;
        let (eid, etok) = s
            .email_invite_create(&alice, "ghost@x.com", "agent", &agent, None, false, false)
            .await
            .unwrap();
        s.email_invite_delivery_record(&eid, &etok, "sent").await;

        assert_eq!(s.invites_emailed_since(since).await, 2, "hub-wide spans both tables");
        assert_eq!(
            s.org_invites_emailed_since(&org, since).await,
            2,
            "an org's own invites plus its members' share invites"
        );
        // A window that starts after the stamps sees nothing.
        assert_eq!(s.invites_emailed_since(now_secs() as i64 + 60).await, 0);
    }

    // Proposal 0083 Part C — link grants: mint, resolve-by-hash, expiry,
    // revoke, regenerate, ownership scoping, and the inertness that matters.
    #[tokio::test]
    async fn link_shares_mint_resolve_revoke_and_regenerate() {
        let s = SqliteStore::in_memory().await;
        let alice = s.create_user("alice@x.com", "password12345").await.unwrap();
        let bob = s.create_user("bob@x.com", "password23456").await.unwrap();
        let (_t, agent) = s.upsert_agent(&alice, "laptop").await.unwrap();
        let hash = |t: &str| cc_screen_auth::sha256_hex(t);

        // Owner-scoped: you cannot publish a file on someone else's machine.
        assert!(s
            .link_share_create(&bob, &agent, "/home/a/x.md", "x.md", &hash("tok-bob"), None)
            .await
            .is_err());
        // Shape checks: absolute path, real digest.
        assert!(s.link_share_create(&alice, &agent, "x.md", "x.md", &hash("t"), None).await.is_err());
        assert!(s.link_share_create(&alice, &agent, "/home/a/x.md", "x.md", "nothex", None).await.is_err());

        let id = s
            .link_share_create(&alice, &agent, "/home/a/tasks.md", "tasks.md", &hash("tok-1"), None)
            .await
            .unwrap();

        // Resolution is by HASH — the plaintext is never stored, so a lookup by
        // the token itself finds nothing.
        let row = s.link_share_by_token_hash(&hash("tok-1")).await.expect("live grant resolves");
        assert_eq!(row.id, id);
        assert_eq!(row.path, "/home/a/tasks.md");
        assert_eq!(row.name, "tasks.md");
        assert!(s.link_share_by_token_hash("tok-1").await.is_none(), "plaintext is not a key");
        assert!(s.link_share_by_token_hash(&hash("guessed")).await.is_none());

        // The owner's outbox lists it; nobody else's does.
        assert_eq!(s.link_share_outbox(&alice).await.len(), 1);
        assert!(s.link_share_outbox(&bob).await.is_empty());

        // Regenerate: the old URL dies at that instant, the id/path survive.
        assert!(!s.link_share_regenerate(&bob, &id, &hash("tok-2")).await, "not bob's to rotate");
        assert!(s.link_share_regenerate(&alice, &id, &hash("tok-2")).await);
        assert!(s.link_share_by_token_hash(&hash("tok-1")).await.is_none(), "old token is dead");
        let row = s.link_share_by_token_hash(&hash("tok-2")).await.unwrap();
        assert_eq!(row.id, id, "same grant, new token");
        assert_eq!(row.path, "/home/a/tasks.md");

        // Expiry is enforced in the store, so no handler can branch on it.
        let past = now_secs() as i64 - 60;
        s.link_share_create(&alice, &agent, "/home/a/old.md", "old.md", &hash("tok-old"), Some(past))
            .await
            .unwrap();
        assert!(s.link_share_by_token_hash(&hash("tok-old")).await.is_none(), "expired ⇒ unknown");
        assert_eq!(s.link_share_outbox(&alice).await.len(), 1, "expired rows are not listed");

        // Revoke is owner-scoped and terminal.
        assert!(!s.link_share_revoke(&bob, &id).await);
        assert!(s.link_share_revoke(&alice, &id).await);
        assert!(s.link_share_by_token_hash(&hash("tok-2")).await.is_none());
        assert!(!s.link_share_revoke(&alice, &id).await, "idempotent");
    }

    // The inertness claim, tested where it can actually be observed: a link
    // grant confers NOTHING through the sharing pipeline. It cannot appear in a
    // Visibility (which never reads this table), an inbox, or a received list.
    #[tokio::test]
    async fn a_link_grant_is_inert_everywhere_but_its_own_table() {
        let s = SqliteStore::in_memory().await;
        let alice = s.create_user("alice@x.com", "password12345").await.unwrap();
        let bob = s.create_user("bob@x.com", "password23456").await.unwrap();
        let (_t, agent) = s.upsert_agent(&alice, "laptop").await.unwrap();
        s.link_share_create(
            &alice,
            &agent,
            "/home/a/tasks.md",
            "tasks.md",
            &cc_screen_auth::sha256_hex("tok"),
            None,
        )
        .await
        .unwrap();

        for who in [&alice, &bob] {
            assert!(s.visibility_rows(who).await.is_empty(), "never a visibility row");
            assert!(s.share_inbox(who).await.is_empty(), "never an inbox row");
            assert!(s.shares_to_me(who).await.is_empty(), "never a received grant");
            assert!(s.share_outbox(who).await.is_empty(), "not a share invite");
        }
        // And unlinking the machine takes its links with it (FK cascade).
        assert_eq!(s.link_share_outbox(&alice).await.len(), 1);
        assert!(s.delete_agent(&alice, &agent).await);
        assert!(s.link_share_outbox(&alice).await.is_empty(), "unlink revokes its link grants");
    }

    // The §4.1 keystone's data half: a token resolves to its OWNER's agent and
    // never to another tenant's, even when both tenants reuse the same machine_id.
    #[tokio::test]
    async fn agent_token_resolves_to_owning_tenant_only() {
        let s = SqliteStore::in_memory().await;
        let alice = s.create_user("alice@x.com", "password12345").await.unwrap();
        let bob = s.create_user("bob@x.com", "password23456").await.unwrap();
        // Both name a machine "laptop" — collision across tenants is expected.
        let (alice_tok, alice_agent) = s.upsert_agent(&alice, "laptop").await.unwrap();
        let (bob_tok, bob_agent) = s.upsert_agent(&bob, "laptop").await.unwrap();
        assert_ne!(alice_agent, bob_agent, "distinct agent ids despite same machine_id");

        // Each token resolves to exactly its owner's agent.
        assert_eq!(s.resolve_agent("laptop", Some(&alice_tok)).await, Some((alice.clone(), alice_agent.clone())));
        assert_eq!(s.resolve_agent("laptop", Some(&bob_tok)).await, Some((bob.clone(), bob_agent)));
        // No token, wrong token, or right token + wrong machine ⇒ rejected.
        assert_eq!(s.resolve_agent("laptop", None).await, None);
        assert_eq!(s.resolve_agent("laptop", Some("garbage")).await, None);
        assert_eq!(s.resolve_agent("server", Some(&alice_tok)).await, None);
    }

    #[tokio::test]
    async fn google_upsert_creates_links_and_returns() {
        let s = SqliteStore::in_memory().await;
        // New OAuth-only user.
        let id = s.upsert_google_user("sub-123", "Gmail@Example.com").await.unwrap();
        // Returning sign-in → same id (and email normalized).
        assert_eq!(s.upsert_google_user("sub-123", "gmail@example.com").await.unwrap(), id);
        assert_eq!(s.user_email(&id).await.as_deref(), Some("gmail@example.com"));
        // OAuth-only account has no password, so the password path never matches.
        assert_eq!(s.verify_login("gmail@example.com", "anything").await, None);

        // Linking: a pre-existing password account adopts the google_sub on first
        // Google sign-in, keeping the same id.
        let pw_id = s.create_user("link@example.com", "password12345").await.unwrap();
        assert_eq!(s.upsert_google_user("sub-link", "link@example.com").await.unwrap(), pw_id);
        // Subsequent sign-in matches on the subject.
        assert_eq!(s.upsert_google_user("sub-link", "link@example.com").await.unwrap(), pw_id);
    }

    #[tokio::test]
    async fn plan_limits_gate_new_machines() {
        let s = SqliteStore::in_memory().await;
        let u = s.create_user("u@x.com", "password12345").await.unwrap();
        // Default 'free' plan = 10 agents; confirm + then squeeze to 1 via 'pro'?
        // Simpler: set a tiny custom plan to test the gate deterministically.
        sqlx::query("INSERT INTO plan_limits (plan, max_agents, max_concurrent_sessions) VALUES ('tiny', 1, 1)")
            .execute(&s.pool)
            .await
            .unwrap();
        s.set_plan("u@x.com", "tiny").await.unwrap();
        assert_eq!(s.limits_for(&u).await.max_agents, 1);

        // First machine via the device flow: approved.
        let c1 = s.device_create("d1", "laptop").await.unwrap();
        assert!(s.device_approve(&u, &c1.user_code_display).await.is_ok());
        // A second, NEW machine is over the cap → LIMIT error.
        let c2 = s.device_create("d2", "server").await.unwrap();
        let err = s.device_approve(&u, &c2.user_code_display).await.unwrap_err().to_string();
        assert!(err.starts_with("LIMIT:"), "got: {err}");
        // Re-enrolling the EXISTING machine is fine (rotate, not a new count).
        let c3 = s.device_create("d3", "laptop").await.unwrap();
        assert!(s.device_approve(&u, &c3.user_code_display).await.is_ok());

        // set_plan rejects an unknown plan and an unknown user.
        assert!(s.set_plan("u@x.com", "nope").await.is_err());
        assert!(s.set_plan("ghost@x.com", "pro").await.is_err());
    }

    // Proposal 0058 A2: the dark-safe migration adds the two columns + the `beta`
    // grandfather row; `Default` is the new free row; the guarded runbook reprice
    // touches the seed rows but leaves a hand-tuned row alone.
    #[tokio::test]
    async fn plan_repricing_columns_and_beta_row() {
        let s = SqliteStore::in_memory().await;
        let u = s.create_user("u@x.com", "password12345").await.unwrap();

        // `Default` equals the new free row (load-bearing: a missing plan can't
        // out-entitle free).
        let d = PlanLimits::default();
        assert_eq!((d.max_agents, d.max_concurrent_sessions), (2, 5));
        assert!(!d.can_create_shares);
        assert_eq!(d.summary_user_budget_usd, Some(0.25));

        // Migration 0007 seeded `beta` at 10/50, sharing on, $2.00 summary budget.
        s.set_plan("u@x.com", "beta").await.unwrap();
        let beta = s.limits_for(&u).await;
        assert_eq!((beta.max_agents, beta.max_concurrent_sessions), (10, 50));
        assert!(beta.can_create_shares);
        assert_eq!(beta.summary_user_budget_usd, Some(2.00));

        // The 0003 `free` row still carries its old caps here (the reprice is a
        // runbook one-shot, not in the migration), but the new columns exist with
        // capability-preserving defaults (can_create_shares=1, budget NULL).
        s.set_plan("u@x.com", "free").await.unwrap();
        let free_pre = s.limits_for(&u).await;
        assert_eq!((free_pre.max_agents, free_pre.max_concurrent_sessions), (10, 50));
        assert!(free_pre.can_create_shares, "default column preserves capability");
        assert_eq!(free_pre.summary_user_budget_usd, None, "NULL = env fallback");

        // A hand-tuned free-like custom row must NOT be touched by the guarded
        // reprice (the UPDATE twin of INSERT OR IGNORE).
        sqlx::query(
            "INSERT INTO plan_limits (plan, max_agents, max_concurrent_sessions) VALUES ('custom', 3, 7)",
        )
        .execute(&s.pool)
        .await
        .unwrap();

        // Apply the Part D T-0 runbook reprice (guarded on the 0003 seed values).
        sqlx::query(
            "UPDATE plan_limits
                SET max_agents = 2, max_concurrent_sessions = 5,
                    can_create_shares = 0, summary_user_budget_usd = 0.25
              WHERE plan = 'free' AND max_agents = 10 AND max_concurrent_sessions = 50",
        )
        .execute(&s.pool)
        .await
        .unwrap();

        s.set_plan("u@x.com", "free").await.unwrap();
        let free = s.limits_for(&u).await;
        assert_eq!((free.max_agents, free.max_concurrent_sessions), (2, 5));
        assert!(!free.can_create_shares);
        assert_eq!(free.summary_user_budget_usd, Some(0.25));

        // The hand-tuned row is left alone (it never matched 10/50).
        s.set_plan("u@x.com", "custom").await.unwrap();
        let custom = s.limits_for(&u).await;
        assert_eq!((custom.max_agents, custom.max_concurrent_sessions), (3, 7));
        assert!(custom.can_create_shares, "custom row untouched, default cap kept");
    }

    // Proposal 0058 Part B: the billing writer + idempotency + prior_plan restore.
    #[tokio::test]
    async fn billing_state_machine_and_idempotency() {
        let s = SqliteStore::in_memory().await;
        let u = s.create_user("u@x.com", "password12345").await.unwrap();
        s.set_plan("u@x.com", "beta").await.unwrap();

        // Helper to read the user's raw billing/plan columns.
        async fn cols(s: &SqliteStore, uid: &str) -> (String, Option<String>, Option<String>, Option<String>, Option<i64>) {
            let r = sqlx::query(
                "SELECT plan, plan_status, prior_plan, billing_subscription_id, current_period_end
                   FROM users WHERE id = ?1",
            )
            .bind(uid)
            .fetch_one(&s.pool)
            .await
            .unwrap();
            (
                r.try_get("plan").unwrap(),
                r.try_get::<Option<String>, _>("plan_status").unwrap(),
                r.try_get::<Option<String>, _>("prior_plan").unwrap(),
                r.try_get::<Option<String>, _>("billing_subscription_id").unwrap(),
                r.try_get::<Option<i64>, _>("current_period_end").unwrap(),
            )
        }

        // First delivery of the checkout event: activate pro.
        let apply = SubApply {
            target: SubTarget::User(u.clone()),
            plan: Some("pro".into()),
            status: "active".into(),
            subscription_id: Some("sub_1".into()),
            customer_id: Some("cus_1".into()),
            period_end: Some(1_800_000_000),
            seat_count: None,
        };
        assert_eq!(s.billing_process_event("evt_1", "h1", 1, Some(apply.clone())).await.unwrap(), EventOutcome::Applied);
        let (plan, status, prior, sub, pend) = cols(&s, &u).await;
        assert_eq!((plan.as_str(), status.as_deref()), ("pro", Some("active")));
        assert_eq!(prior.as_deref(), Some("beta"), "prior_plan stamped once to the pre-activation plan");
        assert_eq!(sub.as_deref(), Some("sub_1"));
        assert_eq!(pend, Some(1_800_000_000));
        assert_eq!(s.limits_for(&u).await.max_agents, 100, "pro caps read with no Stripe call");
        assert_eq!(s.billing_ids(&u).await, (Some("cus_1".into()), Some("sub_1".into())));
        assert_eq!(s.user_by_billing_customer("cus_1").await.as_deref(), Some(u.as_str()));

        // Replay of the same event id → Duplicate, one row, no state change.
        assert_eq!(s.billing_process_event("evt_1", "h1", 2, Some(apply)).await.unwrap(), EventOutcome::Duplicate);
        let n: i64 = sqlx::query("SELECT count(*) AS n FROM billing_events")
            .fetch_one(&s.pool).await.unwrap().try_get("n").unwrap();
        assert_eq!(n, 1);

        // Grace: payment_failed keeps the paid plan, only status flips.
        let pd = SubApply {
            target: SubTarget::User(u.clone()), plan: Some("pro".into()), status: "past_due".into(),
            subscription_id: Some("sub_1".into()), customer_id: Some("cus_1".into()),
            period_end: Some(1_800_000_000), seat_count: None,
        };
        s.billing_process_event("evt_pd", "h", 3, Some(pd)).await.unwrap();
        let (plan, status, prior, _, _) = cols(&s, &u).await;
        assert_eq!((plan.as_str(), status.as_deref()), ("pro", Some("past_due")), "grace: plan unchanged");
        assert_eq!(prior.as_deref(), Some("beta"), "prior_plan never overwritten");

        // Terminal: subscription.deleted → restore prior_plan (beta), clear sub id,
        // keep customer id.
        let del = SubApply {
            target: SubTarget::User(u.clone()), plan: None, status: "canceled".into(),
            subscription_id: None, customer_id: Some("cus_1".into()), period_end: None, seat_count: None,
        };
        s.billing_process_event("evt_del", "h", 4, Some(del)).await.unwrap();
        let (plan, status, prior, sub, _) = cols(&s, &u).await;
        assert_eq!((plan.as_str(), status.as_deref()), ("beta", Some("canceled")), "restored to beta, not free");
        assert_eq!(prior.as_deref(), Some("beta"));
        assert_eq!(sub, None, "subscription id cleared");
        assert_eq!(s.billing_ids(&u).await.0.as_deref(), Some("cus_1"), "customer id kept for resubscribe");

        // Out-of-order: a late 'updated' whose re-fetch still says canceled applies
        // None again → stays beta, no resurrection.
        let late = SubApply {
            target: SubTarget::User(u.clone()), plan: None, status: "canceled".into(),
            subscription_id: None, customer_id: Some("cus_1".into()), period_end: None, seat_count: None,
        };
        s.billing_process_event("evt_late", "h", 5, Some(late)).await.unwrap();
        assert_eq!(cols(&s, &u).await.0, "beta", "no resurrection of pro");

        // Standalone idempotency insert (reconcile/test primitive).
        assert_eq!(s.billing_event_insert("evt_x", "h", 6).await.unwrap(), 1);
        assert_eq!(s.billing_event_insert("evt_x", "h", 7).await.unwrap(), 0);

        // Unknown-plan guard: a bad price→plan map is refused (tx rolls back).
        let bad = SubApply {
            target: SubTarget::User(u.clone()), plan: Some("nope".into()), status: "active".into(),
            subscription_id: Some("sub_2".into()), customer_id: Some("cus_1".into()), period_end: None, seat_count: None,
        };
        assert!(s.billing_process_event("evt_bad", "h", 8, Some(bad)).await.is_err());
        // The rollback also undid the idempotency insert → retry can reprocess.
        let has_bad: Option<String> = sqlx::query("SELECT id FROM billing_events WHERE id = 'evt_bad'")
            .fetch_optional(&s.pool).await.unwrap().and_then(|r| r.try_get("id").ok());
        assert!(has_bad.is_none(), "failed apply rolled back the idempotency insert");

        // Reconcile feed carries the row.
        let rows = s.billing_rows_for_reconcile().await;
        assert!(rows.iter().any(|r| r.user_id == u && r.customer_id.as_deref() == Some("cus_1")));
    }

    #[tokio::test]
    async fn list_and_delete_agents_are_owner_scoped() {
        let s = SqliteStore::in_memory().await;
        let alice = s.create_user("alice@x.com", "password12345").await.unwrap();
        let bob = s.create_user("bob@x.com", "password23456").await.unwrap();
        let (_t, alice_agent) = s.upsert_agent(&alice, "laptop").await.unwrap();
        s.upsert_agent(&alice, "server").await.unwrap();
        s.upsert_agent(&bob, "laptop").await.unwrap();

        assert_eq!(s.list_agents(&alice).await.len(), 2, "alice sees only her two");
        assert_eq!(s.list_agents(&bob).await.len(), 1);
        // Bob cannot delete alice's agent (owner-scoped) — no row removed.
        assert!(!s.delete_agent(&bob, &alice_agent).await);
        assert_eq!(s.list_agents(&alice).await.len(), 2, "still there");
        // Alice can delete her own.
        assert!(s.delete_agent(&alice, &alice_agent).await);
        assert_eq!(s.list_agents(&alice).await.len(), 1);
    }

    #[tokio::test]
    async fn device_flow_throttle_and_pending() {
        let s = SqliteStore::in_memory().await;
        let code = s.device_create("dev-1", "laptop").await.unwrap();
        assert!(code.user_code_display.contains('-'), "display is grouped");
        // First poll (no prior poll) is Pending; an immediate second is throttled.
        assert_eq!(s.device_poll(&code.device_code, "agent").await, DevicePoll::Pending);
        assert_eq!(s.device_poll(&code.device_code, "agent").await, DevicePoll::SlowDown);
        // Unknown code ⇒ treated as expired.
        assert_eq!(s.device_poll("nope", "agent").await, DevicePoll::Expired);
    }

    // The headline: approve binds the code to the tenant and the host claims the
    // minted token exactly once (approve before the first poll, so the claim poll
    // isn't throttled).
    #[tokio::test]
    async fn device_flow_approve_and_single_use_claim() {
        let s = SqliteStore::in_memory().await;
        let alice = s.create_user("alice@x.com", "password12345").await.unwrap();
        let code = s.device_create("dev-1", "laptop").await.unwrap();

        // Approve case/dash-insensitively → binds to alice's "laptop".
        let approved = s.device_approve(&alice, &code.user_code_display.to_lowercase()).await.unwrap();
        assert_eq!(approved, DeviceApproval { kind: "agent".into(), label: "laptop".into() });

        // First poll (last_polled NULL ⇒ not throttled) yields the token once.
        let (token, agent_id) = match s.device_poll(&code.device_code, "agent").await {
            DevicePoll::Approved { token, agent_id } => (token, agent_id),
            other => panic!("expected Approved, got {other:?}"),
        };
        assert_eq!(s.resolve_agent("laptop", Some(&token)).await, Some((alice, agent_id)));
        // Single-use: the row is gone, so a repeat poll is Expired.
        assert_eq!(s.device_poll(&code.device_code, "agent").await, DevicePoll::Expired);

        // A bad/unknown code can't be approved.
        assert!(s.device_approve("someone", "ZZZZ-ZZZZ").await.is_err());
    }

    // Proposal 0060: the terminal-client flow mints a per-user client token —
    // hash at rest, one-time handover, revocable, and NEVER interchangeable with
    // an agent uplink token (the two-credential invariant).
    #[tokio::test]
    async fn client_device_flow_mints_revocable_client_token() {
        let s = SqliteStore::in_memory().await;
        let alice = s.create_user("alice@x.com", "password12345").await.unwrap();
        let code = s.device_create_client("alice@orchid").await.unwrap();

        // Cross-kind polls are uniform dead ends: the client code on the agent
        // endpoint (and vice versa) reads as Expired, never a token.
        assert_eq!(s.device_poll(&code.device_code, "agent").await, DevicePoll::Expired);

        let approved = s.device_approve(&alice, &code.user_code_display).await.unwrap();
        assert_eq!(approved, DeviceApproval { kind: "client".into(), label: "alice@orchid".into() });
        // No agents row was created — a client sign-in consumes no machine slot.
        assert_eq!(s.agent_count(&alice).await, 0);

        let (token, email) = match s.device_poll(&code.device_code, "client").await {
            DevicePoll::ApprovedClient { token, email } => (token, email),
            other => panic!("expected ApprovedClient, got {other:?}"),
        };
        assert_eq!(email, "alice@x.com");
        // Single-use handover: the row is gone.
        assert_eq!(s.device_poll(&code.device_code, "client").await, DevicePoll::Expired);

        // The token resolves alice through the CLIENT resolver only. The uplink
        // resolver must reject it (a client token can't impersonate a machine).
        let hash = cc_screen_auth::sha256_hex(&token);
        assert_eq!(s.user_by_client_token_hash(&hash).await.as_deref(), Some(alice.as_str()));
        assert_eq!(s.resolve_agent("alice@orchid", Some(&token)).await, None);
        // …and an uplink token never resolves as a client credential.
        let (uplink, _aid) = s.upsert_agent(&alice, "laptop").await.unwrap();
        assert_eq!(s.user_by_client_token_hash(&cc_screen_auth::sha256_hex(&uplink)).await, None);

        // Listed with its label (metadata only), last_used_at stamped by the resolve.
        let rows = s.list_client_tokens(&alice).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "alice@orchid");
        assert!(rows[0].last_used_at.is_some());

        // Owner-scoped delete: bob can't revoke alice's token; alice can.
        let bob = s.create_user("bob@x.com", "password23456").await.unwrap();
        assert!(!s.delete_client_token(&bob, &rows[0].id).await);
        assert!(s.delete_client_token(&alice, &rows[0].id).await);
        assert_eq!(s.user_by_client_token_hash(&hash).await, None, "revoked immediately");

        // Self-revoke by hash (`ccs logout`): mint another, revoke by its hash.
        let code2 = s.device_create_client("alice@ssh").await.unwrap();
        s.device_approve(&alice, &code2.user_code_display).await.unwrap();
        let token2 = match s.device_poll(&code2.device_code, "client").await {
            DevicePoll::ApprovedClient { token, .. } => token,
            other => panic!("expected ApprovedClient, got {other:?}"),
        };
        assert!(s.delete_client_token_by_hash(&cc_screen_auth::sha256_hex(&token2)).await);
        assert!(!s.delete_client_token_by_hash(&cc_screen_auth::sha256_hex(&token2)).await);
    }

    // Proposal 0039: share inserts are owner-scoped + idempotent; visibility_rows
    // returns the union (granted-in + issued-out); revoke is owner-scoped; agent
    // delete cascades the grant away.
    #[tokio::test]
    async fn shares_owner_scoped_idempotent_and_cascade() {
        let s = SqliteStore::in_memory().await;
        let alice = s.create_user("alice@x.com", "password12345").await.unwrap();
        let bob = s.create_user("bob@x.com", "password23456").await.unwrap();
        let (_t, agent) = s.upsert_agent(&alice, "laptop").await.unwrap();

        // Self-share and not-your-agent are rejected.
        assert!(s.share_agent(&alice, &agent, &alice, false).await.is_err(), "self-share");
        assert!(s.share_agent(&bob, &agent, &alice, false).await.is_err(), "not bob's agent");

        // Alice agent-shares to bob; idempotent re-issue flips peek, same row id.
        let id1 = s.share_agent(&alice, &agent, &bob, false).await.unwrap();
        let id2 = s.share_agent(&alice, &agent, &bob, true).await.unwrap();
        assert_eq!(id1, id2, "agent share is idempotent on (agent, grantee, kind)");

        // A session share to bob coexists with the agent share (distinct key).
        let sid = s.share_session(&alice, &agent, &bob, "claude-x").await.unwrap();
        assert_ne!(sid, id1);

        // visibility_rows(bob): both grants, owner is alice, agent share now peeked.
        let bob_rows = s.visibility_rows(&bob).await;
        assert_eq!(bob_rows.len(), 2);
        assert!(bob_rows.iter().all(|r| r.owner_user_id == alice && r.grantee_user_id == bob));
        assert!(bob_rows.iter().any(|r| r.kind == "agent" && r.owner_peek));
        assert!(bob_rows.iter().any(|r| r.kind == "session" && r.session.as_deref() == Some("claude-x")));

        // shares_by_owner(alice) sees both; bob (a grantee, not owner) sees none out.
        assert_eq!(s.shares_by_owner(&alice).await.len(), 2);
        assert_eq!(s.shares_by_owner(&bob).await.len(), 0);

        // Revoke is owner-scoped: bob cannot revoke alice's share; alice can.
        assert!(!s.revoke_share(&bob, &sid).await, "grantee can't revoke");
        assert!(s.revoke_share(&alice, &sid).await);
        assert_eq!(s.visibility_rows(&bob).await.len(), 1, "session share gone");

        // Deleting the agent cascades the remaining grant away.
        assert!(s.delete_agent(&alice, &agent).await);
        assert!(s.visibility_rows(&bob).await.is_empty(), "cascade reaped the agent share");
    }

    // Proposal 0040: the invite lifecycle + idempotency + materialisation into the
    // 0039 grant the visibility predicate reads.
    #[tokio::test]
    async fn share_invite_lifecycle_and_materialisation() {
        let s = SqliteStore::in_memory().await;
        let alice = s.create_user("alice@x.com", "password12345").await.unwrap();
        let bob = s.create_user("bob@x.com", "password23456").await.unwrap();
        let (_t, agent) = s.upsert_agent(&alice, "laptop").await.unwrap();

        // Self-invite + not-your-agent are rejected.
        assert!(s.share_create(&alice, &alice, "agent", &agent, None, false).await.is_err());
        assert!(s.share_create(&bob, &agent, "agent", &agent, None, false).await.is_err());

        // Alice invites bob to the agent (with owner-peek). Re-invite is idempotent.
        let (id, st) = s.share_create(&alice, &bob, "agent", &agent, None, true).await.unwrap();
        assert_eq!(st, "pending");
        let (id2, _) = s.share_create(&alice, &bob, "agent", &agent, None, true).await.unwrap();
        assert_eq!(id, id2, "re-invite upserts the one row");

        // Inbox shows it for bob, not alice; outbox shows it for alice.
        assert_eq!(s.share_inbox(&bob).await.len(), 1);
        assert_eq!(s.share_inbox(&alice).await.len(), 0);
        assert_eq!(s.share_outbox(&alice).await.len(), 1);

        // No grant exists yet — pending confers nothing (0039 query empty for bob).
        assert!(s.visibility_rows(&bob).await.is_empty(), "pending ≠ grant");

        // Wrong actor can't drive an edge.
        assert_eq!(s.share_respond(&alice, &id, true).await, ShareOutcome::NotFound, "inviter can't accept");
        assert_eq!(s.share_revoke(&bob, &id).await, ShareOutcome::NotFound, "grantee can't revoke");

        // Bob accepts → grant materialises with owner_peek carried over.
        assert_eq!(s.share_respond(&bob, &id, true).await, ShareOutcome::Ok("accepted".into()));
        let rows = s.visibility_rows(&bob).await;
        assert_eq!(rows.len(), 1);
        assert!(rows[0].kind == "agent" && rows[0].owner_peek, "peek carried onto the grant");
        // Double-accept is an idempotent no-op; it leaves the inbox empty (accepted).
        assert_eq!(s.share_respond(&bob, &id, true).await, ShareOutcome::Ok("accepted".into()));
        assert_eq!(s.share_inbox(&bob).await.len(), 0);

        // Alice revokes (post-accept) → grant stripped, idempotent thereafter.
        assert_eq!(s.share_revoke(&alice, &id).await, ShareOutcome::Ok("revoked".into()));
        assert!(s.visibility_rows(&bob).await.is_empty(), "revoke removes the grant");
        assert_eq!(s.share_revoke(&alice, &id).await, ShareOutcome::Ok("revoked".into()), "re-revoke no-op");
        // Accepting a revoked invite is a conflict, not a resurrection.
        assert_eq!(s.share_respond(&bob, &id, true).await, ShareOutcome::Conflict);

        // Re-invite after a terminal state resets to pending (the only revival path).
        let (id3, st3) = s.share_create(&alice, &bob, "agent", &agent, None, false).await.unwrap();
        assert_eq!((id3.as_str(), st3.as_str()), (id.as_str(), "pending"));
        // Decline path.
        assert_eq!(s.share_respond(&bob, &id, false).await, ShareOutcome::Ok("declined".into()));
        assert!(s.visibility_rows(&bob).await.is_empty());
    }

    // Accepting an agent invite then unlinking the agent reaps both the invite and
    // the materialised grant via FK cascade.
    #[tokio::test]
    async fn unlink_agent_reaps_invites_and_grants() {
        let s = SqliteStore::in_memory().await;
        let alice = s.create_user("alice@x.com", "password12345").await.unwrap();
        let bob = s.create_user("bob@x.com", "password23456").await.unwrap();
        let (_t, agent) = s.upsert_agent(&alice, "laptop").await.unwrap();
        let (id, _) = s.share_create(&alice, &bob, "agent", &agent, None, false).await.unwrap();
        s.share_respond(&bob, &id, true).await;
        assert_eq!(s.visibility_rows(&bob).await.len(), 1);

        // shares_to_me sees the accepted grant before the unlink.
        assert_eq!(s.shares_to_me(&bob).await.len(), 1, "received grant visible");
        assert!(s.shares_to_me(&alice).await.is_empty(), "owner isn't a grantee of their own");

        assert!(s.delete_agent(&alice, &agent).await);
        assert!(s.share_outbox(&alice).await.is_empty(), "invite cascaded");
        assert!(s.visibility_rows(&bob).await.is_empty(), "grant cascaded");
        assert!(s.shares_to_me(&bob).await.is_empty(), "received grant cascaded");
    }

    // ── Proposal 0063: orgs, membership, invites, audit ───────────────────────

    async fn seed_user(s: &SqliteStore, email: &str) -> String {
        s.create_user(email, "password123456").await.unwrap()
    }

    /// Owner + an activated 3-seat org, via the public surface (create + CLI
    /// seats + invite/accept).
    async fn seed_org(s: &SqliteStore, owner: &str, seats: i64) -> String {
        let org = s.org_create(owner, "acme").await.unwrap();
        s.org_set_seats(&org, seats).await.unwrap();
        org
    }

    async fn join(s: &SqliteStore, org: &str, inviter: &str, email: &str) -> String {
        let (id, st, _tok) = s.org_invite_create(org, inviter, email, "member").await.unwrap();
        assert_eq!(st, "pending");
        let uid = s.user_id_by_email(email).await.unwrap();
        match s.org_invite_respond(&uid, &id, true).await.unwrap() {
            ShareOutcome::Ok(st) => assert_eq!(st, "accepted"),
            other => panic!("accept failed: {other:?}"),
        }
        uid
    }

    #[tokio::test]
    async fn org_create_membership_and_one_org_constraint() {
        let s = SqliteStore::in_memory().await;
        let alice = seed_user(&s, "alice@x.com").await;
        let bob = seed_user(&s, "bob@x.com").await;
        let org = seed_org(&s, &alice, 3).await;

        let (row, role) = s.org_for_user(&alice).await.expect("alice is a member");
        assert_eq!((row.id.as_str(), role.as_str()), (org.as_str(), "owner"));
        assert_eq!(row.seat_count, 3);
        // One org per user: a second create by alice fails; org.created audited.
        let err = s.org_create(&alice, "second").await.unwrap_err().to_string();
        assert!(err.starts_with("ONEORG:"), "got: {err}");
        let log = s.audit_page(&org, None, 50).await;
        assert!(log.iter().any(|r| r.action == "org.created"));

        // Invite + accept: bob joins; member.joined + invite rows audited.
        let bob2 = join(&s, &org, &alice, "bob@x.com").await;
        assert_eq!(bob2, bob);
        assert_eq!(s.org_member_ids(&org).await.len(), 2);
        assert!(s.audit_page(&org, None, 50).await.iter().any(|r| r.action == "member.joined"));
        // Double-accept converges (idempotent) — find the invite id again.
        let inv = s.org_invite_outbox(&org).await.into_iter().find(|i| i.email == "bob@x.com").unwrap();
        assert_eq!(s.org_invite_respond(&bob, &inv.id, true).await.unwrap(), ShareOutcome::Ok("accepted".into()));
        // Re-invite of a member is a no-op success with status accepted.
        let (_, st, _) = s.org_invite_create(&org, &alice, "bob@x.com", "member").await.unwrap();
        assert_eq!(st, "accepted");
        // Bob (already in an org) can't create or join another.
        assert!(s.org_create(&bob, "boborg").await.is_err());
        let carol = seed_user(&s, "carol@x.com").await;
        let org2 = {
            // carol makes her own org, invites bob — accept must ONEORG-409.
            let o2 = s.org_create(&carol, "carols").await.unwrap();
            s.org_set_seats(&o2, 3).await.unwrap();
            o2
        };
        let (iid, _, _) = s.org_invite_create(&org2, &carol, "bob@x.com", "member").await.unwrap();
        let err = s.org_invite_respond(&bob, &iid, true).await.unwrap_err().to_string();
        assert!(err.starts_with("ONEORG:"), "got: {err}");
    }

    #[tokio::test]
    async fn org_roles_transfer_and_removal() {
        let s = SqliteStore::in_memory().await;
        let alice = seed_user(&s, "alice@x.com").await;
        seed_user(&s, "bob@x.com").await;
        let org = seed_org(&s, &alice, 5).await;
        let bob = join(&s, &org, &alice, "bob@x.com").await;

        // Owner can't be removed or demoted in place — transfer first.
        let err = s.org_remove_member(&org, &alice).await.unwrap_err().to_string();
        assert!(err.starts_with("OWNER:"));
        let err = s.org_set_role(&org, &alice, "member").await.unwrap_err().to_string();
        assert!(err.starts_with("OWNER:"));

        // Transfer: exactly one owner at all times.
        s.org_set_role(&org, &bob, "owner").await.unwrap();
        let members = s.org_members(&org).await;
        assert_eq!(members.iter().filter(|m| m.role == "owner").count(), 1);
        assert_eq!(members.iter().find(|m| m.user_id == bob).unwrap().role, "owner");
        assert_eq!(members.iter().find(|m| m.user_id == alice).unwrap().role, "admin");

        // Now alice (admin) can be removed; membership reverts her org lookup.
        s.org_remove_member(&org, &alice).await.unwrap();
        assert!(s.org_for_user(&alice).await.is_none());
    }

    #[tokio::test]
    async fn org_seat_gate_blocks_over_capacity() {
        let s = SqliteStore::in_memory().await;
        let alice = seed_user(&s, "alice@x.com").await;
        seed_user(&s, "bob@x.com").await;
        seed_user(&s, "carol@x.com").await;
        let org = seed_org(&s, &alice, 2).await; // owner + 1
        join(&s, &org, &alice, "bob@x.com").await;

        // Third accept (over the 2 seats) → SEATS error, no member row.
        let (id, _, _) = s.org_invite_create(&org, &alice, "carol@x.com", "member").await.unwrap();
        let carol = s.user_id_by_email("carol@x.com").await.unwrap();
        let err = s.org_invite_respond(&carol, &id, true).await.unwrap_err().to_string();
        assert!(err.starts_with("SEATS:"), "got: {err}");
        assert_eq!(s.org_member_ids(&org).await.len(), 2);
        // Freeing a seat admits the next accept with zero billing traffic.
        let bob = s.user_id_by_email("bob@x.com").await.unwrap();
        s.org_remove_member(&org, &bob).await.unwrap();
        assert_eq!(s.org_invite_respond(&carol, &id, true).await.unwrap(), ShareOutcome::Ok("accepted".into()));
        // A dormant (0-seat) org refuses accepts outright.
        let dave = seed_user(&s, "dave@x.com").await;
        s.org_set_seats(&org, 0).await.unwrap();
        let (id2, _, _) = s.org_invite_create(&org, &alice, "dave@x.com", "member").await.unwrap();
        let err = s.org_invite_respond(&dave, &id2, true).await.unwrap_err().to_string();
        assert!(err.starts_with("SEATS:"), "got: {err}");
    }

    #[tokio::test]
    async fn org_pooled_limits_and_machine_gate() {
        let s = SqliteStore::in_memory().await;
        let alice = seed_user(&s, "alice@x.com").await;
        seed_user(&s, "bob@x.com").await;
        let org = seed_org(&s, &alice, 3).await;
        let bob = join(&s, &org, &alice, "bob@x.com").await;

        // 0064 acceptance 1: a member resolves the pooled team caps; the
        // per-member fields stay unmultiplied.
        let l = s.limits_for(&bob).await;
        assert_eq!((l.plan.as_str(), l.max_agents, l.max_concurrent_sessions), ("team", 30, 150));
        assert!(l.can_create_shares, "sharing is the team product");
        assert_eq!(l.summary_user_budget_usd, Some(2.00), "per-member, never multiplied");

        // Dormant org → personal plans, unchanged.
        s.org_set_seats(&org, 0).await.unwrap();
        assert_eq!(s.limits_for(&bob).await.plan, "free");
        s.org_set_seats(&org, 2).await.unwrap();

        // Pooled machine gate (0063 C2): tune the per-seat contribution to 1 so
        // the 2-seat pool caps at 2 machines org-wide.
        sqlx::query("UPDATE plan_limits SET max_agents = 1 WHERE plan = 'team'")
            .execute(&s.pool)
            .await
            .unwrap();
        let c1 = s.device_create("d1", "laptop").await.unwrap();
        s.device_approve(&alice, &c1.user_code_display).await.unwrap();
        let c2 = s.device_create("d2", "server").await.unwrap();
        s.device_approve(&alice, &c2.user_code_display).await.unwrap();
        // Pool full: bob's NEW label 402s even though bob owns zero machines…
        let c3 = s.device_create("d3", "bobbox").await.unwrap();
        let err = s.device_approve(&bob, &c3.user_code_display).await.unwrap_err().to_string();
        assert!(err.starts_with("LIMIT:") && err.contains("Team machine pool"), "got: {err}");
        // …while a re-enroll of an existing label still rotates fine.
        let c4 = s.device_create("d4", "laptop").await.unwrap();
        assert!(s.device_approve(&alice, &c4.user_code_display).await.is_ok());

        // `user plan team` footgun is closed (0063 A1).
        let err = s.set_plan("bob@x.com", "team").await.unwrap_err().to_string();
        assert!(err.contains("org plan"), "got: {err}");
    }

    // ── Proposal 0065 Part A: materialized team shares ────────────────────────

    #[tokio::test]
    async fn team_shares_materialize_prune_and_optout() {
        let s = SqliteStore::in_memory().await;
        let alice = seed_user(&s, "alice@x.com").await;
        seed_user(&s, "bob@x.com").await;
        let (_t, a_laptop) = s.upsert_agent(&alice, "laptop").await.unwrap();
        let org = seed_org(&s, &alice, 3).await;
        let bob = join(&s, &org, &alice, "bob@x.com").await;

        // Join materialized both directions: bob sees alice's machine…
        let bob_rows = s.shares_to_me(&bob).await;
        assert_eq!(bob_rows.len(), 1);
        assert_eq!((bob_rows[0].kind.as_str(), bob_rows[0].agent_id.as_str()), ("team", a_laptop.as_str()));
        assert_eq!(bob_rows[0].org_id.as_deref(), Some(org.as_str()));
        // …and a machine bob enrolls later appears for alice with no action
        // (the device_approve hook).
        let code = s.device_create("d1", "bobbox").await.unwrap();
        s.device_approve(&bob, &code.user_code_display).await.unwrap();
        let alice_rows = s.shares_to_me(&alice).await;
        assert_eq!(alice_rows.len(), 1, "alice sees bob's box via the new-machine hook");
        // Idempotent: re-materializing adds nothing.
        s.team_shares_materialize(&org).await;
        assert_eq!(s.shares_to_me(&bob).await.len(), 1);

        // Team rows are excluded from the personal outbox and not revocable.
        assert!(s.shares_by_owner(&alice).await.is_empty(), "team rows never show in the outbox");
        let row_id = s.shares_to_me(&bob).await[0].id.clone();
        assert!(s.share_is_team(&row_id).await);
        assert!(!s.revoke_share(&alice, &row_id).await, "team rows are not individually revocable");
        assert!(!s.leave_grant(&bob, &row_id).await, "and not individually leavable");

        // Opt-out prunes within the action; a personal share on the same agent
        // survives (org_id discriminates).
        let personal = s.share_agent(&alice, &a_laptop, &bob, false).await.unwrap();
        assert!(s.set_team_visible(&alice, &a_laptop, false).await);
        assert!(!s.set_team_visible(&bob, &a_laptop, false).await, "owner-scoped");
        s.team_shares_prune(&org).await;
        let kinds: Vec<String> = s.shares_to_me(&bob).await.into_iter().map(|r| r.kind).collect();
        assert_eq!(kinds, vec!["agent".to_string()], "team row pruned, personal share survives");
        // Flag back on → row returns via materialize.
        assert!(s.set_team_visible(&alice, &a_laptop, true).await);
        s.team_shares_materialize(&org).await;
        assert_eq!(s.shares_to_me(&bob).await.len(), 2);
        let _ = personal;

        // Member removal prunes exactly that member's org rows, both directions.
        s.org_remove_member(&org, &bob).await.unwrap();
        let kinds: Vec<String> = s.shares_to_me(&bob).await.into_iter().map(|r| r.kind).collect();
        assert_eq!(kinds, vec!["agent".to_string()], "only the personal share survives removal");
        assert!(s.shares_to_me(&alice).await.is_empty(), "alice's sight of bob's box gone too");

        // Org deletion cascades everything (0063 A acceptance).
        sqlx::query("DELETE FROM orgs WHERE id = ?1").bind(&org).execute(&s.pool).await.unwrap();
        let n: i64 = sqlx::query("SELECT count(*) AS n FROM shares WHERE kind = 'team'")
            .fetch_one(&s.pool).await.unwrap().try_get("n").unwrap();
        assert_eq!(n, 0);
        let n: i64 = sqlx::query("SELECT count(*) AS n FROM audit_log")
            .fetch_one(&s.pool).await.unwrap().try_get("n").unwrap();
        assert_eq!(n, 0, "audit dies with the org (CASCADE)");
    }

    // ── Proposal 0073: the delivery receipt ───────────────────────────────────

    /// One org invite's receipt across its whole life: absent until a send is
    /// attempted, claimed-then-stamped by an attempt, cleared by a re-invite, and
    /// **unwritable by a superseded or revoked attempt** — the guard the spawned
    /// send in `org::invite_create` rides on (0073 B1/B2).
    #[tokio::test]
    async fn org_invite_delivery_receipt_is_token_guarded() {
        let s = SqliteStore::in_memory().await;
        let alice = seed_user(&s, "alice@x.com").await;
        let org = seed_org(&s, &alice, 3).await;
        let row = |id: String| {
            let s = &s;
            let org = org.clone();
            async move { s.org_invite_outbox(&org).await.into_iter().find(|r| r.id == id).unwrap() }
        };

        // A fresh invite carries no receipt: NULL means "no send was attempted",
        // which is the permanent answer on a hub with no mailer.
        let (id, st, tok1) = s.org_invite_create(&org, &alice, "ghost@x.com", "member").await.unwrap();
        assert_eq!(st, "pending");
        let r = row(id.clone()).await;
        assert_eq!((r.delivery.as_deref(), r.emailed_at), (None, None));

        // Claim → 'sending' (the pre-attempt stamp), then the outcome + a time.
        assert!(s.org_invite_mark_sending(&id, &tok1).await);
        assert_eq!(row(id.clone()).await.delivery.as_deref(), Some("sending"));
        s.org_invite_delivery_record(&id, &tok1, "sent").await;
        let r = row(id.clone()).await;
        assert_eq!(r.delivery.as_deref(), Some("sent"));
        assert!(r.emailed_at.is_some());

        // A re-invite keeps the row, mints a FRESH token, and clears the receipt.
        let (id2, st2, tok2) = s.org_invite_create(&org, &alice, "GHOST@x.com", "member").await.unwrap();
        assert_eq!((id2.as_str(), st2.as_str()), (id.as_str(), "pending"));
        assert_ne!(tok2, tok1);
        let r = row(id.clone()).await;
        assert_eq!((r.delivery.as_deref(), r.emailed_at), (None, None), "a re-invite starts clean");

        // Attempt one is now superseded: it can neither claim nor stamp.
        assert!(!s.org_invite_mark_sending(&id, &tok1).await);
        s.org_invite_delivery_record(&id, &tok1, "failed").await;
        assert_eq!(row(id.clone()).await.delivery, None, "the superseded attempt writes nothing");

        // A revoke inside the send window kills the live token's claim too.
        assert!(s.org_invite_mark_sending(&id, &tok2).await);
        s.org_invite_revoke(&org, &id).await;
        assert!(!s.org_invite_mark_sending(&id, &tok2).await, "revoked ⇒ no send");
        // That leaves the row wearing the pre-attempt stamp with no task behind it
        // — exactly what a restart mid-send leaves. The sweep fail-stamps it once
        // it is stale (the hub has no graceful shutdown).
        assert_eq!(row(id.clone()).await.delivery.as_deref(), Some("sending"));
        s.invite_delivery_sweep().await;
        assert_eq!(row(id.clone()).await.delivery.as_deref(), Some("sending"), "a fresh attempt is never stolen");
        let _ = sqlx::query("UPDATE org_invites SET created_at = ?1 WHERE id = ?2")
            .bind(now_secs() as i64 - DELIVERY_STUCK_AFTER - 60)
            .bind(&id)
            .execute(&s.pool)
            .await;
        s.invite_delivery_sweep().await;
        let r = row(id.clone()).await;
        assert_eq!(r.delivery.as_deref(), Some("failed"), "a lost attempt becomes visible");
        assert!(r.emailed_at.is_some());
    }

    /// The `email_invites` twin. Liveness there is the row + its token: a revoke
    /// DELETEs it and a re-offer mints a fresh token on it.
    #[tokio::test]
    async fn email_invite_delivery_receipt_is_token_guarded() {
        let s = SqliteStore::in_memory().await;
        let alice = seed_user(&s, "alice@x.com").await;
        let (_t, agent) = s.upsert_agent(&alice, "laptop").await.unwrap();
        let live = |id: String| {
            let s = &s;
            let alice = alice.clone();
            async move { s.email_invite_outbox(&alice).await.into_iter().find(|r| r.id == id) }
        };

        let (id, tok1) =
            s.email_invite_create(&alice, "ghost@x.com", "agent", &agent, None, false, false).await.unwrap();
        assert_eq!(live(id.clone()).await.unwrap().delivery, None);

        assert!(s.email_invite_mark_sending(&id, &tok1).await);
        s.email_invite_delivery_record(&id, &tok1, "rejected").await;
        let r = live(id.clone()).await.unwrap();
        assert_eq!(r.delivery.as_deref(), Some("rejected"));
        assert!(r.emailed_at.is_some());

        // Re-offer: same row, fresh token, cleared receipt; attempt one is dead.
        let (id2, tok2) =
            s.email_invite_create(&alice, "ghost@x.com", "agent", &agent, None, false, false).await.unwrap();
        assert_eq!(id2, id);
        assert_eq!(live(id.clone()).await.unwrap().delivery, None);
        assert!(!s.email_invite_mark_sending(&id, &tok1).await);
        s.email_invite_delivery_record(&id, &tok1, "sent").await;
        assert_eq!(live(id.clone()).await.unwrap().delivery, None, "the superseded attempt writes nothing");

        // A revoke removes the row outright, so the claim fails there too.
        assert!(s.email_invite_revoke(&alice, &id).await);
        assert!(!s.email_invite_mark_sending(&id, &tok2).await);
        assert!(live(id).await.is_none());
    }

    // ── Proposal 0064: org-targeted billing writes ────────────────────────────

    #[tokio::test]
    async fn org_billing_state_machine() {
        let s = SqliteStore::in_memory().await;
        let alice = seed_user(&s, "alice@x.com").await;
        let org = s.org_create(&alice, "acme").await.unwrap();

        // checkout.session.completed (re-fetch: active team, quantity 3).
        let apply = SubApply {
            target: SubTarget::Org(org.clone()),
            plan: Some("team".into()),
            status: "active".into(),
            subscription_id: Some("sub_t".into()),
            customer_id: Some("cus_o".into()),
            period_end: Some(1_900_000_000),
            seat_count: Some(3),
        };
        assert_eq!(s.billing_process_event("evt_t1", "h", 1, Some(apply)).await.unwrap(), EventOutcome::Applied);
        let row = s.org_get(&org).await.unwrap();
        assert_eq!((row.plan.as_str(), row.plan_status.as_deref(), row.seat_count), ("team", Some("active"), 3));
        assert_eq!(s.org_by_billing_customer("cus_o").await.as_deref(), Some(org.as_str()));
        // A member's limits show the pool with no Stripe call on the read path.
        assert_eq!(s.limits_for(&alice).await.max_agents, 30);
        // No `users` writes: alice's personal plan untouched (SQL assert).
        let plan: String = sqlx::query("SELECT plan FROM users WHERE id = ?1")
            .bind(&alice).fetch_one(&s.pool).await.unwrap().try_get("plan").unwrap();
        assert_eq!(plan, "free");

        // Portal quantity bump 3→5 flows in via the updated re-fetch.
        let bump = SubApply {
            target: SubTarget::Org(org.clone()),
            plan: Some("team".into()),
            status: "active".into(),
            subscription_id: Some("sub_t".into()),
            customer_id: Some("cus_o".into()),
            period_end: Some(1_900_000_000),
            seat_count: Some(5),
        };
        s.billing_process_event("evt_t2", "h", 2, Some(bump)).await.unwrap();
        assert_eq!(s.org_get(&org).await.unwrap().seat_count, 5);

        // Terminal: seats zeroed, sub id cleared, customer kept, membership
        // intact, member falls back to the personal plan — zero users writes.
        let del = SubApply {
            target: SubTarget::Org(org.clone()),
            plan: None,
            status: "canceled".into(),
            subscription_id: None,
            customer_id: Some("cus_o".into()),
            period_end: None,
            seat_count: Some(0),
        };
        s.billing_process_event("evt_t3", "h", 3, Some(del)).await.unwrap();
        let row = s.org_get(&org).await.unwrap();
        assert_eq!((row.plan_status.as_deref(), row.seat_count, row.billing_subscription_id), (Some("canceled"), 0, None));
        assert_eq!(row.billing_customer_id.as_deref(), Some("cus_o"), "kept for resubscribe");
        assert_eq!(s.org_member_ids(&org).await.len(), 1, "nobody evicted");
        assert_eq!(s.limits_for(&alice).await.plan, "free", "dormant org confers nothing");
        // Reconcile feed carries the org (with member_count riding along).
        let rows = s.org_billing_rows_for_reconcile().await;
        assert!(rows.iter().any(|r| r.org_id == org && r.member_count == 1));
        // Founder cohort: plan/prior_plan beta.
        assert!(!s.user_founder_cohort(&alice).await);
        s.set_plan("alice@x.com", "beta").await.unwrap();
        assert!(s.user_founder_cohort(&alice).await);
    }

    #[tokio::test]
    async fn upsert_rotates_token_in_place() {
        let s = SqliteStore::in_memory().await;
        let alice = s.create_user("alice@x.com", "password12345").await.unwrap();
        let (tok1, agent1) = s.upsert_agent(&alice, "laptop").await.unwrap();
        let (tok2, agent2) = s.upsert_agent(&alice, "laptop").await.unwrap();
        assert_eq!(agent1, agent2, "same (user, machine) keeps its agent id");
        assert_ne!(tok1, tok2, "token rotated");
        // Old token is instantly dead; new one works.
        assert_eq!(s.resolve_agent("laptop", Some(&tok1)).await, None);
        assert_eq!(s.resolve_agent("laptop", Some(&tok2)).await, Some((alice, agent1)));
    }
}
