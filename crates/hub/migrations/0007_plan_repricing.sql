-- Proposal 0058 Part A — pricing & plan restructure (dark-safe schema half only).
-- This migration adds columns (with capability-preserving defaults) and the
-- grandfather `beta` row nobody is on yet. It does NOT reprice `free`/`pro` or
-- move the cohort — those value changes alter live behavior and are RUNBOOK
-- one-shots sequenced across the rollout (Part D): the cohort move
-- (UPDATE users SET plan='beta' WHERE plan='free') at T-60d, the guarded
-- free/pro reprices at T-0. Ordering is load-bearing (cohort move before the
-- free re-seed, or every beta user silently drops to 2/5), which is exactly why
-- they live in the ops runbook, not here.

ALTER TABLE plan_limits ADD COLUMN can_create_shares       INTEGER NOT NULL DEFAULT 1;
ALTER TABLE plan_limits ADD COLUMN summary_user_budget_usd REAL;  -- NULL = env fallback

-- Today's free caps (10/50), frozen forever for the beta cohort ("free during
-- beta" honored). INSERT OR IGNORE keeps this idempotent and never clobbers a
-- hand-tuned row.
INSERT OR IGNORE INTO plan_limits (plan, max_agents, max_concurrent_sessions,
                                   can_create_shares, summary_user_budget_usd)
VALUES ('beta', 10, 50, 1, 2.00);
