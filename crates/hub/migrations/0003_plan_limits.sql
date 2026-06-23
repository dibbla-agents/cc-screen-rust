-- Proposal 0001 Phase 4 (minus billing — this is an internal service): the plan
-- limits the relay enforces. `users.plan` already exists (default 'free'); an
-- admin sets it with `cc-screen-hub user plan <email> <plan>`. No Stripe — plans
-- are assigned by hand, but the *enforcement* (max machines, max concurrent
-- sessions) is real, replacing the hardcoded caps the §8.3 gate referenced.

CREATE TABLE IF NOT EXISTS plan_limits (
    plan                    TEXT    PRIMARY KEY,
    max_agents              INTEGER NOT NULL,
    max_concurrent_sessions INTEGER NOT NULL
);

-- Seed sensible internal defaults. 'free' is generous enough not to bite normal
-- internal use; bump a user to 'unlimited' when needed. INSERT OR IGNORE so this
-- migration is idempotent and never clobbers hand-tuned rows.
INSERT OR IGNORE INTO plan_limits (plan, max_agents, max_concurrent_sessions) VALUES
    ('free',       10,      50),
    ('pro',        100,     500),
    ('unlimited',  1000000, 1000000);
