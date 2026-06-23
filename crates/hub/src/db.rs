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

    /// Hand-provision a user (Phase 1 has no public signup). Returns the new
    /// `user_id`. Errors on a duplicate email or a too-short password.
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
            .ok()??;
        let id: String = row.try_get("id").ok()?;
        let hash: Option<String> = row.try_get("password_hash").ok()?;
        verify_password(password, &hash?).then_some(id)
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

    async fn create_user(&self, email: &str, password: &str) -> anyhow::Result<String> {
        let email = normalize_email(email);
        anyhow::ensure!(!email.is_empty(), "email is required");
        anyhow::ensure!(password.len() >= 8, "password must be at least 8 characters");
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
}

fn normalize_email(email: &str) -> String {
    email.trim().to_lowercase()
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
        s.create_user("a@b.com", "longenough").await.unwrap();
        assert!(s.create_user("a@b.com", "longenough").await.is_err(), "duplicate email");
        assert!(s.create_user("c@d.com", "short").await.is_err(), "short password");
    }

    // The §4.1 keystone's data half: a token resolves to its OWNER's agent and
    // never to another tenant's, even when both tenants reuse the same machine_id.
    #[tokio::test]
    async fn agent_token_resolves_to_owning_tenant_only() {
        let s = SqliteStore::in_memory().await;
        let alice = s.create_user("alice@x.com", "password1").await.unwrap();
        let bob = s.create_user("bob@x.com", "password2").await.unwrap();
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
        let pw_id = s.create_user("link@example.com", "password1").await.unwrap();
        assert_eq!(s.upsert_google_user("sub-link", "link@example.com").await.unwrap(), pw_id);
        // Subsequent sign-in matches on the subject.
        assert_eq!(s.upsert_google_user("sub-link", "link@example.com").await.unwrap(), pw_id);
    }

    #[tokio::test]
    async fn upsert_rotates_token_in_place() {
        let s = SqliteStore::in_memory().await;
        let alice = s.create_user("alice@x.com", "password1").await.unwrap();
        let (tok1, agent1) = s.upsert_agent(&alice, "laptop").await.unwrap();
        let (tok2, agent2) = s.upsert_agent(&alice, "laptop").await.unwrap();
        assert_eq!(agent1, agent2, "same (user, machine) keeps its agent id");
        assert_ne!(tok1, tok2, "token rotated");
        // Old token is instantly dead; new one works.
        assert_eq!(s.resolve_agent("laptop", Some(&tok1)).await, None);
        assert_eq!(s.resolve_agent("laptop", Some(&tok2)).await, Some((alice, agent1)));
    }
}
