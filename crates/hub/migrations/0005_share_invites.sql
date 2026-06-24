-- Proposal 0040 — share invites. An owner invites a grantee to one of their
-- agents or sessions; the share takes effect only when the grantee accepts, at
-- which point the handler materialises a 0039 `shares` grant row (which the
-- visibility predicate reads). This table owns the *lifecycle* that produces that
-- grant: pending → accepted | declined | revoked | expired.
--
-- FK cascade (sqlx runs with foreign_keys=ON) reaps invites when the agent is
-- unlinked or a user is deleted — the same way the 0039 `shares` grant cascades.
CREATE TABLE IF NOT EXISTS share_invites (
    id              TEXT    PRIMARY KEY,        -- random (cc_screen_auth::generate_token)
    inviter_user_id TEXT    NOT NULL REFERENCES users(id)  ON DELETE CASCADE,  -- the owner
    grantee_user_id TEXT    NOT NULL REFERENCES users(id)  ON DELETE CASCADE,  -- the recipient
    resource_kind   TEXT    NOT NULL CHECK (resource_kind IN ('agent','session')),
    agent_id        TEXT    NOT NULL REFERENCES agents(id) ON DELETE CASCADE,  -- the machine
    session_name    TEXT,                       -- NULL for an agent-wide share; the
                                                --   session NAME (not 0035 label) for a session share
    -- Owner-peek (0039 §4.3): for an agent share, may the OWNER keep sight of the
    -- sessions the grantee creates? Carried on the invite so the accepted grant
    -- already knows it. Ignored for session shares.
    owner_peek      INTEGER NOT NULL DEFAULT 0,
    status          TEXT    NOT NULL,           -- pending|accepted|declined|revoked|expired
    created_at      INTEGER NOT NULL,           -- unix epoch seconds
    responded_at    INTEGER,                    -- accept/decline/revoke time
    expires_at      INTEGER,                    -- pending TTL (+14 days); NULL = no expiry
    -- One invite per (grantee, resource); re-inviting upserts this row (§4). Note
    -- SQLite treats NULLs as distinct, so for an agent invite (session_name NULL)
    -- the app layer upserts by (grantee, agent_id, kind) explicitly.
    UNIQUE (grantee_user_id, resource_kind, agent_id, session_name),

    CHECK ((resource_kind = 'session') = (session_name IS NOT NULL))
);

CREATE INDEX IF NOT EXISTS idx_share_grantee_status ON share_invites (grantee_user_id, status);
CREATE INDEX IF NOT EXISTS idx_share_inviter        ON share_invites (inviter_user_id);
CREATE INDEX IF NOT EXISTS idx_share_resource       ON share_invites (agent_id, session_name);
