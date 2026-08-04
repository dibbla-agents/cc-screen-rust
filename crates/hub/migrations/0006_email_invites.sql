-- Proposal 0056 Part C — invites to an email with no account yet ([0040]'s
-- declared "later extension"). Converts into a share_invites row (and from
-- there the normal accept → 0039 grant) when an account with this email
-- appears. Never grants anything by itself.
--
-- Rows are also minted (pre-converted) for KNOWN-user invites so the share
-- create response carries an /invite/<token> link in both arms — that response
-- uniformity is what closes the account-existence oracle ([0042]).
CREATE TABLE IF NOT EXISTS email_invites (
    id              TEXT    PRIMARY KEY,          -- cc_screen_auth::generate_token()
    token           TEXT    NOT NULL UNIQUE,      -- capability in the invite link
    inviter_user_id TEXT    NOT NULL REFERENCES users(id)  ON DELETE CASCADE,
    email           TEXT    NOT NULL,             -- lowercased at insert
    resource_kind   TEXT    NOT NULL CHECK (resource_kind IN ('agent','session')),
    agent_id        TEXT    NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    session_name    TEXT,
    owner_peek      INTEGER NOT NULL DEFAULT 0,
    created_at      INTEGER NOT NULL,
    expires_at      INTEGER NOT NULL,             -- +14 days, matching share_invites
    converted_at    INTEGER,                      -- set when attached to a new account
    UNIQUE (email, resource_kind, agent_id, session_name),
    CHECK ((resource_kind = 'session') = (session_name IS NOT NULL))
);
CREATE INDEX IF NOT EXISTS idx_email_invites_email   ON email_invites (email);
CREATE INDEX IF NOT EXISTS idx_email_invites_inviter ON email_invites (inviter_user_id);
