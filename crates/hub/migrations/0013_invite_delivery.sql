-- Proposal 0073 — per-invite delivery state for the transactional mailer.
-- Nullable by construction: NULL means "no send was attempted", which is the
-- correct and permanent answer for every row created before the mailer existed
-- and for every hub that never configures CCHUB_SMTP_URL.
ALTER TABLE org_invites   ADD COLUMN emailed_at INTEGER;
ALTER TABLE org_invites   ADD COLUMN delivery   TEXT;  -- sending|sent|rejected|failed
ALTER TABLE email_invites ADD COLUMN emailed_at INTEGER;
ALTER TABLE email_invites ADD COLUMN delivery   TEXT;
