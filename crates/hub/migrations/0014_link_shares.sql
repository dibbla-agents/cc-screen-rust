-- Proposal 0083 Part C — read-only link grants.
--
-- The owner mints `https://<hub>/s/<token>` for ONE file; anyone holding the
-- URL may read it, with no account, until the owner revokes it.
--
-- Why its own table rather than a fourth `shares.kind`:
--
--   `shares.grantee_user_id` is `NOT NULL REFERENCES users(id)` and every
--   consumer of that table — `visibility_rows` → `Visibility::from_rows` →
--   `may_see_session` / `may_browse_agent`, the inbox/received/accept paths,
--   the org reconcile — is written against a row that HAS a grantee. A link
--   grant has none (the bearer is the recipient), so putting it there would
--   mean widening the column to NULL and then teaching every one of those
--   readers to skip a row shape they have never seen. The proposal asks for
--   that skipping to be provable; a separate table makes it structural instead
--   of tested: `Visibility` cannot see these rows because it never reads this
--   table. The API surface stays exactly as the proposal specifies —
--   `POST /api/shares {kind:"link"}`, the outbox, `POST /api/shares/:id/revoke`
--   — which is what clients and [0041]'s UI actually contract on.
--
-- The token is stored ONLY as its SHA-256 ([0073] flagged that invite tokens
-- are plaintext at rest; a standing capability doesn't get that treatment). A
-- plain indexed lookup on the hash is safe: the token is 32 bytes of OsRng, so
-- it is neither guessable nor in need of a salt or a constant-time compare.
CREATE TABLE IF NOT EXISTS link_shares (
    id             TEXT    PRIMARY KEY,           -- opaque random id (base64url)

    -- The owning machine. ON DELETE CASCADE is the unlink revocation rule:
    -- `POST /api/agents/unlink` removes the agent row, and every link grant on
    -- it dies with it.
    agent_id       TEXT    NOT NULL REFERENCES agents(id) ON DELETE CASCADE,

    -- Denormalised owner. `shares` resolves the owner by joining `agents`; here
    -- it is stored, because every operation on a link grant (outbox, revoke,
    -- regenerate) is owner-scoped and must stay so even if the agent row is
    -- mid-rotation.
    owner_user_id  TEXT    NOT NULL REFERENCES users(id) ON DELETE CASCADE,

    -- SHA-256 (lowercase hex) of the bearer token. UNIQUE so `regenerate` is a
    -- single deterministic UPDATE and a collision is a hard error, not a
    -- silently shared grant.
    token_hash     TEXT    NOT NULL UNIQUE,

    -- The ABSOLUTE path on the agent, canonicalized agent-side at mint time
    -- ([0074]'s rule: the hub never canonicalizes paths — it has no filesystem).
    -- Grants are path-identity: a rename kills the link ([0076]'s
    -- never-re-pointed rule), and a file deleted and re-created at the same
    -- path serves under the old token until revoked.
    path           TEXT    NOT NULL,

    -- The basename at mint time, for the outbox label and the page header. A
    -- display copy; the path is the identity.
    name           TEXT    NOT NULL,

    created_at     INTEGER NOT NULL,              -- unix seconds UTC

    -- NULL = until revoked. v1 mints with no expiry on purpose: a bookmarkable
    -- share that silently dies is worse than one you must revoke.
    expires_at     INTEGER
);

-- The owner's management view (outbox / revoke).
CREATE INDEX IF NOT EXISTS idx_link_shares_owner ON link_shares (owner_user_id);
-- The per-agent sweep (an unlink cascades, but an owner listing one machine's
-- links reads this).
CREATE INDEX IF NOT EXISTS idx_link_shares_agent ON link_shares (agent_id);
