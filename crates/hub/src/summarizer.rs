//! The hub's session summarizer (proposal 0022).
//!
//! The hub is the single keyholder (`CCHUB_ANTHROPIC_API_KEY`) and spend boundary.
//! On a [`AgentMsg::SummaryRequest`] it gates — feature on? key present? budget
//! left? — and, if allowed, calls Haiku via the shared [`cc_screen_summary`] crate
//! and returns a [`Summary`]. A declined request returns `None` and the agent
//! keeps showing `preview`. Refusals are logged, never errors that break the
//! uplink.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

pub use cc_screen_summary::Summary;

/// Estimated cost per summary call, in micro-dollars. Haiku 4.5 is $1/1M input,
/// $5/1M output; a ~5K-input + ~200-output call is ≈ $0.006 (proposal 0022 §11).
/// Used only to enforce the budget gate — we don't read back exact usage.
const EST_CALL_MICROS: u64 = 6_000;

/// What to do with a summary request.
#[derive(Debug)]
pub enum Outcome {
    /// A summary was produced.
    Ok(Summary),
    /// The gate declined (feature off / no key / over budget) — show preview.
    Declined,
    /// The call was attempted but failed (network / non-2xx / parse) — show preview.
    Failed,
}

/// The hub's summarizer: config + a shared HTTP client + the running spend tally.
pub struct Summarizer {
    enabled: bool,
    api_key: Option<String>,
    model: String,
    /// Lifetime spend cap (USD) since process start; `None` = uncapped.
    budget_usd: Option<f64>,
    spent_micros: AtomicU64,
    /// Per-user spend cap (USD) since process start (proposal 0001 §10.6.2). In
    /// multi-tenant the shared key is a per-tenant abuse surface, so each user gets
    /// their own ceiling on top of the fleet-wide `budget_usd`. `None` = no
    /// per-user cap (single-tenant, or unset).
    user_budget_usd: Option<f64>,
    per_user_micros: Mutex<HashMap<String, u64>>,
    client: reqwest::Client,
}

impl Summarizer {
    /// Build from parsed config. `enabled` defaults (in the config layer) to "on
    /// iff a key is set"; with no key it can never spend regardless.
    pub fn new(
        enabled: bool,
        api_key: Option<String>,
        model: String,
        budget_usd: Option<f64>,
        user_budget_usd: Option<f64>,
    ) -> Self {
        Self {
            enabled,
            api_key,
            model,
            budget_usd,
            spent_micros: AtomicU64::new(0),
            user_budget_usd,
            per_user_micros: Mutex::new(HashMap::new()),
            client: reqwest::Client::new(),
        }
    }

    /// A disabled summarizer (no key, off) — declines everything. Used by tests
    /// and as the default when summaries aren't configured.
    pub fn disabled() -> Self {
        Self::new(false, None, cc_screen_summary::DEFAULT_MODEL.to_string(), None, None)
    }

    /// Whether the feature is live (on AND a key is present).
    pub fn active(&self) -> bool {
        self.enabled && self.api_key.is_some()
    }

    /// Estimated USD spent so far this process.
    pub fn spent_usd(&self) -> f64 {
        self.spent_micros.load(Ordering::Relaxed) as f64 / 1_000_000.0
    }

    /// Reserve budget for one call: returns false (and reserves nothing) if a
    /// budget is set and this call would exceed it. Atomic so concurrent requests
    /// can't both slip past the cap.
    fn try_reserve(&self) -> bool {
        let Some(budget) = self.budget_usd else { return true };
        let cap_micros = (budget * 1_000_000.0) as u64;
        loop {
            let cur = self.spent_micros.load(Ordering::Relaxed);
            if cur + EST_CALL_MICROS > cap_micros {
                return false;
            }
            if self
                .spent_micros
                .compare_exchange(cur, cur + EST_CALL_MICROS, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                return true;
            }
        }
    }

    /// Per-user pre-check: would another call fit `user`'s ceiling? Commits the
    /// reservation when it fits. A tiny concurrent overshoot is acceptable for an
    /// abuse cap. Always true when no per-user cap is set or `user` is None.
    fn try_reserve_user(&self, user: Option<&str>) -> bool {
        let (Some(budget), Some(user)) = (self.user_budget_usd, user) else { return true };
        let cap_micros = (budget * 1_000_000.0) as u64;
        let mut m = self.per_user_micros.lock().unwrap();
        let spent = m.entry(user.to_string()).or_insert(0);
        if *spent + EST_CALL_MICROS > cap_micros {
            return false;
        }
        *spent += EST_CALL_MICROS;
        true
    }

    /// Estimated USD a single user has spent this process (proposal 0001 §10.6.2).
    pub fn user_spent_usd(&self, user: &str) -> f64 {
        self.per_user_micros.lock().unwrap().get(user).copied().unwrap_or(0) as f64 / 1_000_000.0
    }

    /// Gate + (if allowed) call Haiku. Never panics; failures are logged and
    /// mapped to [`Outcome::Failed`] so the uplink reader keeps running.
    pub async fn summarize(&self, inputs: &[String], tail: &str) -> Outcome {
        self.summarize_for(None, inputs, tail).await
    }

    /// Like [`summarize`], but also charges the per-tenant ceiling (§10.6.2) when
    /// `owner` is `Some` and a per-user budget is configured. Both the fleet-wide
    /// and the per-user cap must have room. Single-tenant passes `None` → only the
    /// fleet budget applies, exactly as before.
    pub async fn summarize_for(&self, owner: Option<&str>, inputs: &[String], tail: &str) -> Outcome {
        let Some(key) = self.api_key.as_deref() else { return Outcome::Declined };
        if !self.enabled {
            return Outcome::Declined;
        }
        // Per-user ceiling first (so a maxed-out tenant never touches the fleet
        // tally), then the fleet-wide budget.
        if !self.try_reserve_user(owner) {
            tracing::warn!("summary: user {:?} over per-user budget; declining", owner);
            return Outcome::Declined;
        }
        if !self.try_reserve() {
            tracing::warn!("summary: over fleet budget (spent ≈ ${:.2}); declining", self.spent_usd());
            return Outcome::Declined;
        }
        match cc_screen_summary::summarize(&self.client, key, &self.model, inputs, tail).await {
            Ok(s) => Outcome::Ok(s),
            Err(e) => {
                tracing::warn!("summary: call failed: {e}");
                Outcome::Failed
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_and_keyless_decline() {
        let s = Summarizer::disabled();
        assert!(!s.active());
        // No key → enabling alone doesn't make it active.
        let s = Summarizer::new(true, None, "claude-haiku-4-5".into(), None, None);
        assert!(!s.active());
        // Key + on → active.
        let s = Summarizer::new(true, Some("k".into()), "claude-haiku-4-5".into(), None, None);
        assert!(s.active());
        // Key but off → inactive.
        let s = Summarizer::new(false, Some("k".into()), "claude-haiku-4-5".into(), None, None);
        assert!(!s.active());
    }

    #[test]
    fn budget_gate_stops_spending() {
        // Budget of one call's worth: the first reserve succeeds, the next fails.
        let micros = EST_CALL_MICROS as f64 / 1_000_000.0;
        let s = Summarizer::new(true, Some("k".into()), "m".into(), Some(micros), None);
        assert!(s.try_reserve(), "first call fits the budget");
        assert!(!s.try_reserve(), "second call is over budget");
        // No budget → always reserves.
        let s = Summarizer::new(true, Some("k".into()), "m".into(), None, None);
        for _ in 0..1000 {
            assert!(s.try_reserve());
        }
    }

    #[test]
    fn per_user_gate_isolates_tenants() {
        // One call's worth per user; the fleet budget is uncapped.
        let micros = EST_CALL_MICROS as f64 / 1_000_000.0;
        let s = Summarizer::new(true, Some("k".into()), "m".into(), None, Some(micros));
        // alice spends her allowance; her second call is refused…
        assert!(s.try_reserve_user(Some("alice")));
        assert!(!s.try_reserve_user(Some("alice")), "alice is now capped");
        // …but bob is unaffected (per-tenant isolation).
        assert!(s.try_reserve_user(Some("bob")));
        // No owner / no per-user budget → always reserves.
        assert!(s.try_reserve_user(None));
        let s2 = Summarizer::new(true, Some("k".into()), "m".into(), None, None);
        assert!(s2.try_reserve_user(Some("alice")));
        assert!(s2.try_reserve_user(Some("alice")));
    }

    #[tokio::test]
    async fn no_key_summarize_declines_without_calling() {
        let s = Summarizer::new(true, None, "m".into(), None, None);
        assert!(matches!(s.summarize(&["q".into()], "tail").await, Outcome::Declined));
    }
}
