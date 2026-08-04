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

    // ── RFC-8628 device flow (proposal 0001 §6–8) ──────────────────────────────
    /// Mint + store a pending enrollment for a headless host (`/api/device/code`).
    async fn device_create(&self, device_id: &str, machine_id: &str) -> anyhow::Result<DeviceCode>;

    /// A host's poll (`/api/device/token`). Handles lazy expiry, `slow_down`
    /// throttling, and single-use delivery of the approved token.
    async fn device_poll(&self, device_code: &str) -> DevicePoll;

    /// A logged-in browser approves a pending code (`/api/device/approve`), binding
    /// it to `user_id`, minting the agent's token, and parking the plaintext for the
    /// host's next poll. Returns the bound `machine_id`. Errors if the code is
    /// unknown/expired/already used.
    async fn device_approve(&self, user_id: &str, user_code: &str) -> anyhow::Result<String>;

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
}

/// A plan's enforced caps (proposal 0001 Phase 4). Resolved from `plan_limits`
/// joined on `users.plan`; falls back to [`PlanLimits::default`] if the plan row
/// is missing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanLimits {
    /// The plan's name (`users.plan`), surfaced by `/api/me` (proposal 0056 B1).
    pub plan: String,
    pub max_agents: i64,
    pub max_concurrent_sessions: i64,
}
impl Default for PlanLimits {
    fn default() -> Self {
        // Mirrors the seeded 'free' row, so an unknown/missing plan is merely
        // conservative, never unbounded.
        PlanLimits { plan: "free".into(), max_agents: 10, max_concurrent_sessions: 50 }
    }
}

/// One of a user's registered agents, for the dashboard (proposal 0001 Phase 3).
/// Live online status is annotated by the hub from its registry, not stored here.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentRow {
    pub agent_id: String,
    pub machine_id: String,
    pub created_at: i64,
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

    #[cfg(test)]
    async fn in_memory() -> Self {
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
        anyhow::ensure!(!email.is_empty(), "email is required");
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
        let device_code = cc_screen_auth::generate_token();
        let display = gen_user_code();
        let stored = normalize_user_code(&display);
        let expires_at = now_secs() as i64 + DEVICE_CODE_TTL;
        sqlx::query(
            "INSERT INTO device_enrollments
               (device_code, user_code, device_id, machine_id, status, interval, expires_at)
             VALUES (?1, ?2, ?3, ?4, 'pending', ?5, ?6)",
        )
        .bind(&device_code)
        .bind(&stored)
        .bind(device_id)
        .bind(machine_id)
        .bind(DEVICE_POLL_INTERVAL)
        .bind(expires_at)
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

    async fn device_poll(&self, device_code: &str) -> DevicePoll {
        let now = now_secs() as i64;
        let Ok(Some(row)) = sqlx::query(
            "SELECT status, agent_id, uplink_token, expires_at, last_polled_at, interval
               FROM device_enrollments WHERE device_code = ?1",
        )
        .bind(device_code)
        .fetch_optional(&self.pool)
        .await
        else {
            return DevicePoll::Expired; // unknown ⇒ treat as expired
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
                // Single-use: hand the parked token over exactly once, then delete.
                let _ = sqlx::query("DELETE FROM device_enrollments WHERE device_code = ?1")
                    .bind(device_code)
                    .execute(&self.pool)
                    .await;
                match (token, agent_id) {
                    (Some(token), Some(agent_id)) => DevicePoll::Approved { token, agent_id },
                    _ => DevicePoll::Expired,
                }
            }
            _ => DevicePoll::Expired,
        }
    }

    async fn device_approve(&self, user_id: &str, user_code: &str) -> anyhow::Result<String> {
        let now = now_secs() as i64;
        let code = normalize_user_code(user_code);
        let row = sqlx::query(
            "SELECT device_code, machine_id FROM device_enrollments
              WHERE user_code = ?1 AND status = 'pending' AND expires_at > ?2",
        )
        .bind(&code)
        .bind(now)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| anyhow::anyhow!("unknown or expired code"))?;
        let device_code: String = row.try_get("device_code")?;
        let machine_id: String = row.try_get("machine_id")?;

        // Plan gate (§8.3): a genuinely NEW machine past the cap is refused; a
        // re-enroll of an existing label reuses its row and doesn't count. The
        // "LIMIT:" prefix lets the handler answer 402 (not 404).
        if !self.has_machine(user_id, &machine_id).await {
            let limits = self.limits_for(user_id).await;
            if self.agent_count(user_id).await >= limits.max_agents {
                anyhow::bail!(
                    "LIMIT:Machine limit reached for your plan ({}). Unlink one or ask for an upgrade.",
                    limits.max_agents
                );
            }
        }

        // Mint (or rotate) the agent + its token, then park the plaintext for the
        // host's next poll to claim exactly once.
        let (token, agent_id) = self.upsert_agent(user_id, &machine_id).await?;
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
        Ok(machine_id)
    }

    async fn list_agents(&self, user_id: &str) -> Vec<AgentRow> {
        let rows = sqlx::query("SELECT id, machine_id, created_at FROM agents WHERE user_id = ?1 ORDER BY created_at DESC")
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
        sqlx::query(
            "SELECT pl.plan, pl.max_agents, pl.max_concurrent_sessions
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

    async fn visibility_rows(&self, user_id: &str) -> Vec<ShareRow> {
        let rows = sqlx::query(
            "SELECT s.id, s.agent_id, a.user_id AS owner_user_id, s.grantee_user_id,
                    s.kind, s.session, s.owner_peek, s.created_at
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
        let rows = sqlx::query(
            "SELECT s.id, s.agent_id, a.user_id AS owner_user_id, s.grantee_user_id,
                    s.kind, s.session, s.owner_peek, s.created_at
               FROM shares s JOIN agents a ON a.id = s.agent_id
              WHERE a.user_id = ?1
              ORDER BY s.created_at DESC",
        )
        .bind(owner_user_id)
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();
        rows.iter().filter_map(share_row).collect()
    }

    async fn revoke_share(&self, owner_user_id: &str, share_id: &str) -> bool {
        sqlx::query(
            "DELETE FROM shares
              WHERE id = ?1
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
                    s.kind, s.session, s.owner_peek, s.created_at
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
        let Some(row) = sqlx::query(
            "SELECT agent_id, kind, session FROM shares WHERE id = ?1 AND grantee_user_id = ?2",
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
        anyhow::ensure!(!email.is_empty(), "email is required");
        let now = now_secs() as i64;
        let expires = now + SHARE_INVITE_TTL;
        let converted_at = converted.then_some(now);

        // Upsert by (email, kind, agent, session) explicitly — a NULL session for
        // an agent invite is DISTINCT under the UNIQUE index (same discipline as
        // share_invites). A re-offer keeps the row id but mints a FRESH token
        // (the old link dies) and refreshes the TTL.
        let existing: Option<String> = sqlx::query(
            "SELECT id FROM email_invites
              WHERE email = ?1 AND agent_id = ?2 AND resource_kind = ?3
                AND ((?4 IS NULL AND session_name IS NULL) OR session_name = ?4)",
        )
        .bind(&email)
        .bind(agent_id)
        .bind(kind)
        .bind(session)
        .fetch_optional(&self.pool)
        .await?
        .and_then(|r| r.try_get("id").ok());

        let token = cc_screen_auth::generate_token();
        if let Some(id) = existing {
            sqlx::query(
                "UPDATE email_invites
                    SET token = ?1, owner_peek = ?2, created_at = ?3, expires_at = ?4,
                        converted_at = ?5
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

        // Abuse bound (proposal 0056 C2): cap the inviter's LIVE (unconverted,
        // unexpired) email invites. Pre-converted known-user rows don't count.
        if !converted {
            let live: i64 = sqlx::query(
                "SELECT count(*) AS n FROM email_invites
                  WHERE inviter_user_id = ?1 AND converted_at IS NULL AND expires_at > ?2",
            )
            .bind(inviter)
            .bind(now)
            .fetch_one(&self.pool)
            .await
            .ok()
            .and_then(|r| r.try_get::<i64, _>("n").ok())
            .unwrap_or(0);
            anyhow::ensure!(
                live < EMAIL_INVITE_CAP,
                "CAP:too many pending invitations — cancel some from your outbox first"
            );
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
/// Public-signup password minimum, aligned with the CCWEB_PASSWORD warning bar
/// (proposal 0053 Part E).
pub const MIN_PASSWORD_LEN: usize = 12;

fn normalize_email(email: &str) -> String {
    email.trim().to_lowercase()
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
        assert_eq!(s.device_poll(&code.device_code).await, DevicePoll::Pending);
        assert_eq!(s.device_poll(&code.device_code).await, DevicePoll::SlowDown);
        // Unknown code ⇒ treated as expired.
        assert_eq!(s.device_poll("nope").await, DevicePoll::Expired);
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
        let machine = s.device_approve(&alice, &code.user_code_display.to_lowercase()).await.unwrap();
        assert_eq!(machine, "laptop");

        // First poll (last_polled NULL ⇒ not throttled) yields the token once.
        let (token, agent_id) = match s.device_poll(&code.device_code).await {
            DevicePoll::Approved { token, agent_id } => (token, agent_id),
            other => panic!("expected Approved, got {other:?}"),
        };
        assert_eq!(s.resolve_agent("laptop", Some(&token)).await, Some((alice, agent_id)));
        // Single-use: the row is gone, so a repeat poll is Expired.
        assert_eq!(s.device_poll(&code.device_code).await, DevicePoll::Expired);

        // A bad/unknown code can't be approved.
        assert!(s.device_approve("someone", "ZZZZ-ZZZZ").await.is_err());
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
