-- Proposal 0060 — per-user terminal-client credentials (`ccs activate`).
-- `client_tokens` mirrors `agents` deliberately: hash at rest, shown once,
-- individually revocable, cascade-deleted with the user. The `kind` column lets
-- one `device_enrollments` table serve both the machine flow ('agent') and the
-- terminal sign-in flow ('client').
-- (0007/0008 are reserved by proposal 0058's billing branch; sqlx orders by
-- version and does not require contiguity.)

CREATE TABLE IF NOT EXISTS client_tokens (
    id           TEXT    PRIMARY KEY,
    user_id      TEXT    NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    label        TEXT    NOT NULL,          -- e.g. "erik@orchid", shown on the account page
    token_hash   TEXT    NOT NULL UNIQUE,   -- sha256, never plaintext (0042 Stream E)
    created_at   INTEGER NOT NULL,
    last_used_at INTEGER
);

CREATE INDEX IF NOT EXISTS idx_client_tokens_user ON client_tokens (user_id);

ALTER TABLE device_enrollments ADD COLUMN kind TEXT NOT NULL DEFAULT 'agent';
