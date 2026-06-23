//! Multi-tenant store (proposal 0001), compiled only under the `multi-tenant`
//! feature. Phase 0 scaffold: this module + the optional `sqlx` dependency exist
//! purely to prove the SaaS superset builds and that everything here compiles
//! *out* cleanly under default features.
//!
//! The store is **pluggable** (per the chosen direction, deviating from the
//! proposal's Postgres-only assumption): Phase 1 puts the persistence operations
//! behind a trait — SQLite is the first backend, a Postgres backend can be added
//! later as a second impl without touching callers — and adds the `users` table,
//! argon2 verification, and a `SqliteTokens` impl of [`crate::state::AgentTokens`].
//! Phase 2 adds the `device_enrollments` handlers.

/// The first backend's connection pool. SQLite (file-backed, zero-ops for dev and
/// small single-node installs); a Postgres pool becomes an alternate backend
/// behind the same store trait in a later phase. Named here so Phase 1 can
/// reference a stable type from the scaffold.
pub type Db = sqlx::SqlitePool;
