-- Proposal 0058 Part B — Stripe billing mirror.
-- Billing truth lives in Stripe; these columns are the local mirror the
-- entitlement checks read (never the API on a request path). NULLs = never
-- subscribed. The relation is 1:1 by construction (single-seat B2C; Team is a
-- non-goal), so the columns live on `users`, not a separate subscriptions table.
ALTER TABLE users ADD COLUMN billing_customer_id     TEXT;     -- cus_…
ALTER TABLE users ADD COLUMN billing_subscription_id TEXT;     -- sub_…
ALTER TABLE users ADD COLUMN plan_status             TEXT;     -- active|past_due|canceled
ALTER TABLE users ADD COLUMN current_period_end      INTEGER;  -- unix epoch seconds
-- The plan to restore when a subscription dies: beta users land back on 'beta',
-- an unlimited friend lands back on 'unlimited' — never blindly 'free'. Stamped
-- once (COALESCE) on first paid activation, never overwritten by later churn.
ALTER TABLE users ADD COLUMN prior_plan              TEXT;

CREATE INDEX IF NOT EXISTS idx_users_billing_customer ON users (billing_customer_id);

-- Exactly-once webhook processing: first INSERT wins, replays no-op.
CREATE TABLE IF NOT EXISTS billing_events (
    id           TEXT    PRIMARY KEY,   -- Stripe event id (evt_…)
    received_at  INTEGER NOT NULL,
    payload_hash TEXT    NOT NULL       -- sha256 hex of the raw body (audit)
);
