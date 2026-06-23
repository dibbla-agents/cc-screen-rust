-- Proposal 0001 Phase 1 — identity + the tenancy boundary.
-- SQLite-first; types kept portable (TEXT ids, INTEGER epoch-seconds) so a
-- Postgres backend can mirror this schema later. Emails are stored lowercased by
-- the application layer (SQLite TEXT UNIQUE is case-sensitive), giving the
-- citext-unique behaviour the proposal's §4 schema describes.

CREATE TABLE IF NOT EXISTS users (
    id              TEXT    PRIMARY KEY,         -- opaque random id (base64url)
    email           TEXT    NOT NULL UNIQUE,     -- lowercased by the app layer
    password_hash   TEXT,                        -- argon2id; NULL for OAuth-only (§3.3)
    google_sub      TEXT    UNIQUE,              -- stable Google subject; match here (§3.3)
    session_version INTEGER NOT NULL DEFAULT 0,  -- bump = invalidate all cookies
    plan            TEXT    NOT NULL DEFAULT 'free',
    created_at      INTEGER NOT NULL             -- unix epoch seconds
    -- invariant (enforced in code): password_hash OR google_sub is present
);

CREATE TABLE IF NOT EXISTS agents (
    id            TEXT    PRIMARY KEY,           -- the durable AgentId (base64url)
    user_id       TEXT    NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    device_id     TEXT,                          -- stable host identity (Phase 2)
    machine_id    TEXT    NOT NULL,              -- the user's label: "laptop"
    token_hash    TEXT    NOT NULL,              -- sha256 of the uplink token; never plaintext
    status        TEXT    NOT NULL DEFAULT 'offline',
    last_seen_at  INTEGER,
    created_at    INTEGER NOT NULL,
    UNIQUE (user_id, machine_id)
);

-- The uplink resolves an inbound (machine_id, token) to its owning row; the token
-- hash is the selective half of that lookup.
CREATE INDEX IF NOT EXISTS idx_agents_token_hash ON agents (token_hash);
