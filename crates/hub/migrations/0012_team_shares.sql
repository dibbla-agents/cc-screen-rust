-- Proposal 0065 Part A — the third share kind, 'team', plus the org_id
-- discriminator. A team grant is a real `shares` row so the whole
-- load→fold→predicate pipeline (visibility_rows → Visibility::from_rows →
-- may_see_session) and every cascade applies unchanged.
--
-- SQLite has no ALTER TABLE … DROP CONSTRAINT, so widening the kind CHECK is
-- the standard rebuild: create, copy, drop, rename, recreate the indexes.
-- The `(kind = 'session') = (session IS NOT NULL)` clause survives verbatim —
-- a team row has session IS NULL, like an agent row.

CREATE TABLE shares_new (
    id              TEXT    PRIMARY KEY,
    agent_id        TEXT    NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    grantee_user_id TEXT    NOT NULL REFERENCES users(id) ON DELETE CASCADE,

    -- 'agent'   → grantee may use the whole agent (list/create/attach).
    -- 'session' → grantee may view only the named session (session NOT NULL).
    -- 'team'    → grantee may VIEW the machine + all its sessions (0065 A2) —
    --             no create, no admin, no re-share. Materialized from org
    --             membership; org_id names the org that implies it.
    kind            TEXT    NOT NULL CHECK (kind IN ('agent','session','team')),

    session         TEXT,
    owner_peek      INTEGER NOT NULL DEFAULT 0,   -- 0 and ignored for team rows
    created_at      INTEGER NOT NULL,

    -- NULL for personal (0039) grants; the implying org for team rows, so
    -- leave-cleanup is exact (a personal share on the same agent survives
    -- leaving the org) and org deletion erases the materialization by CASCADE.
    org_id          TEXT    REFERENCES orgs(id) ON DELETE CASCADE,

    UNIQUE (agent_id, grantee_user_id, kind, session),
    CHECK ((kind = 'session') = (session IS NOT NULL))
);

INSERT INTO shares_new (id, agent_id, grantee_user_id, kind, session, owner_peek, created_at, org_id)
SELECT id, agent_id, grantee_user_id, kind, session, owner_peek, created_at, NULL FROM shares;

DROP TABLE shares;
ALTER TABLE shares_new RENAME TO shares;

CREATE INDEX IF NOT EXISTS idx_shares_grantee ON shares (grantee_user_id);
CREATE INDEX IF NOT EXISTS idx_shares_agent   ON shares (agent_id);
-- Team-row idempotency: the four-column UNIQUE key treats NULL session as
-- distinct, so team rows need their own key — this lets the materializer be a
-- plain INSERT OR IGNORE (0065 A1).
CREATE UNIQUE INDEX IF NOT EXISTS idx_shares_team ON shares (agent_id, grantee_user_id)
    WHERE kind = 'team';
