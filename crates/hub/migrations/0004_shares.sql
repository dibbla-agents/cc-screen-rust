-- Proposal 0039 — agent & session sharing grants.
-- A row means: `grantee_user_id` may see/use a scope on `agent_id` that they do
-- not own. The owner is always agents.user_id; we denormalise nothing — the
-- owner is resolved by joining agents.

CREATE TABLE IF NOT EXISTS shares (
    id              TEXT    PRIMARY KEY,        -- opaque random id (base64url)
    agent_id        TEXT    NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    grantee_user_id TEXT    NOT NULL REFERENCES users(id) ON DELETE CASCADE,

    -- 'agent'   → grantee may use the whole agent (list/create/attach).
    -- 'session' → grantee may view only the named session (session NOT NULL).
    kind            TEXT    NOT NULL CHECK (kind IN ('agent','session')),

    -- The session NAME ("claude-myproj") for a session share; NULL for an agent
    -- share. Session names are the agent-side ids (engine.rs Session.name); they
    -- are stable per machine and what the relay already routes by.
    session         TEXT,

    -- Owner-peek (this proposal's override): for kind='agent' only, may the OWNER
    -- see the sessions the grantee creates on this agent? Default off. Ignored for
    -- session shares.
    owner_peek      INTEGER NOT NULL DEFAULT 0,

    created_at      INTEGER NOT NULL,

    -- A grant is defined by (who can see, what, which slice). Re-sharing the same
    -- thing is idempotent; the unique key makes revoke a single deterministic row.
    -- session is part of the key; SQLite treats NULLs as distinct, so the agent
    -- share (session IS NULL) and session shares coexist — acceptable, the app
    -- layer upserts agent shares by (agent_id, grantee, kind).
    UNIQUE (agent_id, grantee_user_id, kind, session),

    CHECK ((kind = 'session') = (session IS NOT NULL))  -- session iff session-kind
);

-- The hot path: "what may this grantee see?" — resolve_scoped runs this per
-- request, so index the grantee.
CREATE INDEX IF NOT EXISTS idx_shares_grantee ON shares (grantee_user_id);
-- The revoke / owner-management path: "what has this agent shared out?"
CREATE INDEX IF NOT EXISTS idx_shares_agent ON shares (agent_id);
