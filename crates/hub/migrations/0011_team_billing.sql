-- Proposal 0064 — Team seat billing. Org billing mirror (0008_billing.sql's
-- pattern; truth lives in Stripe, entitlements read these columns only).
-- seat_count mirrors the subscription item's quantity and is written ONLY by
-- the webhook/reconcile (never by membership changes — Stripe is the seat
-- source of truth; membership is enforced AGAINST it, 0064 B5) — plus the
-- `org seats` admin CLI on self-hosted/no-Stripe hubs (0063 B4).
ALTER TABLE orgs ADD COLUMN seat_count              INTEGER NOT NULL DEFAULT 0;
ALTER TABLE orgs ADD COLUMN billing_customer_id     TEXT;     -- cus_…
ALTER TABLE orgs ADD COLUMN billing_subscription_id TEXT;     -- sub_…
ALTER TABLE orgs ADD COLUMN plan_status             TEXT;     -- active|past_due|canceled
ALTER TABLE orgs ADD COLUMN current_period_end      INTEGER;  -- unix epoch seconds

CREATE INDEX IF NOT EXISTS idx_orgs_billing_customer ON orgs (billing_customer_id);

-- Per-seat contribution (multiplied by orgs.seat_count in code — 0064 A1/A2).
-- Deliberately ≥ Pro per seat so "every seat gets everything Pro has" is true.
INSERT OR IGNORE INTO plan_limits (plan, max_agents, max_concurrent_sessions,
                                   can_create_shares, summary_user_budget_usd)
VALUES ('team', 10, 50, 1, 2.00);
