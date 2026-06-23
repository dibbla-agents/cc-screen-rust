-- Proposal 0001 Phase 2 — the RFC-8628 device authorization grant (§4, §6–8).
-- A headless host posts for a code, prints it, and polls; the user approves it
-- from a browser that's already logged in, binding the pending row to their
-- tenant and minting the agent's uplink token.

CREATE TABLE IF NOT EXISTS device_enrollments (
    device_code    TEXT    PRIMARY KEY,        -- high-entropy; the host polls with this
    user_code      TEXT    NOT NULL UNIQUE,    -- short human code, NORMALIZED (no dash, upper)
    device_id      TEXT    NOT NULL,           -- stable host identity
    machine_id     TEXT    NOT NULL,           -- requested label
    user_id        TEXT,                       -- null until approved → binds the tenant
    agent_id       TEXT,                       -- set on approval
    uplink_token   TEXT,                       -- plaintext parked for ONE pickup, then deleted
    status         TEXT    NOT NULL,           -- pending | approved | denied | expired
    interval       INTEGER NOT NULL,           -- poll throttle (seconds)
    last_polled_at INTEGER,
    expires_at     INTEGER NOT NULL            -- unix epoch seconds (~10 min out)
);

CREATE INDEX IF NOT EXISTS idx_device_user_code ON device_enrollments (user_code);
