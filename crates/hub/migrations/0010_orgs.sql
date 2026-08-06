-- Proposal 0063 — orgs, membership, org invites, audit log, machine opt-out.
-- Ownership of machines NEVER moves: agents keep their single user_id owner
-- (0001_init.sql); an org grants visibility and pooled limits, nothing else.
-- Billing lives elsewhere by design: seat_count, the Stripe mirror columns,
-- and the 'team' plan_limits seed arrive in 0064's 0011_team_billing.sql —
-- this migration is the billing-free org core.

CREATE TABLE IF NOT EXISTS orgs (
    id         TEXT    PRIMARY KEY,   -- opaque random id (base64url)
    name       TEXT    NOT NULL,
    plan       TEXT    NOT NULL DEFAULT 'team',  -- plan_limits row (per-seat; seeded by 0064)
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS org_members (
    org_id     TEXT    NOT NULL REFERENCES orgs(id)  ON DELETE CASCADE,
    user_id    TEXT    NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role       TEXT    NOT NULL CHECK (role IN ('owner','admin','member')),
    created_at INTEGER NOT NULL,
    PRIMARY KEY (org_id, user_id),
    -- v1: at most ONE org per user — keeps limits inheritance (0063 C1) and the
    -- consent model unambiguous. Lifting this later is a constraint drop plus
    -- a resolution policy, not a schema rework.
    UNIQUE (user_id)
);

-- Org invites: the 0040/0005 state machine + the 0006 token-link, verbatim
-- pattern. Both arms (existing account / not yet) mint a token so the create
-- response is uniform — the 0042 account-existence-oracle fix carries over.
CREATE TABLE IF NOT EXISTS org_invites (
    id              TEXT    PRIMARY KEY,
    token           TEXT    NOT NULL UNIQUE,       -- capability in the invite link
    org_id          TEXT    NOT NULL REFERENCES orgs(id)  ON DELETE CASCADE,
    inviter_user_id TEXT    NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    email           TEXT    NOT NULL,              -- lowercased at insert
    role            TEXT    NOT NULL DEFAULT 'member' CHECK (role IN ('admin','member')),
    status          TEXT    NOT NULL,              -- pending|accepted|declined|revoked|expired
    created_at      INTEGER NOT NULL,
    responded_at    INTEGER,
    expires_at      INTEGER NOT NULL,              -- +14 days, matching share_invites
    UNIQUE (org_id, email)                         -- re-invite refreshes the row (0040 §4)
);
CREATE INDEX IF NOT EXISTS idx_org_invites_email ON org_invites (email);

-- Append-only audit trail (0063 Part D). INTEGER PRIMARY KEY deliberately breaks
-- the TEXT-id convention: rowid gives a monotonic, gap-tolerant cursor for
-- paging and an unambiguous total order — exactly what an audit log wants.
CREATE TABLE IF NOT EXISTS audit_log (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    org_id        TEXT    NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    at            INTEGER NOT NULL,
    actor_user_id TEXT,               -- NULL for system events (sweep-expiry)
    action        TEXT    NOT NULL,   -- dotted vocabulary, see org.rs
    target        TEXT,               -- e.g. a user_id, agent_id, invite id
    detail        TEXT                -- small JSON blob; never terminal content
);
CREATE INDEX IF NOT EXISTS idx_audit_org ON audit_log (org_id, id);

-- The consent model's opt-out (0063 §"The consent decision"): may this machine
-- be visible to the owner's org? Owner-toggled, audited, read by 0065's
-- visibility materialization. Default ON — joining implies visibility.
ALTER TABLE agents ADD COLUMN team_visible INTEGER NOT NULL DEFAULT 1;
