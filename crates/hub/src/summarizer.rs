//! The hub's session summarizer (proposal 0022).
//!
//! The hub is the single keyholder (`CCHUB_ANTHROPIC_API_KEY`) and spend boundary.
//! On a [`AgentMsg::SummaryRequest`] it gates — feature on? key present? budget
//! left? — and, if allowed, calls Haiku via the shared [`cc_screen_summary`] crate
//! and returns a [`Summary`]. A declined request returns `None` and the agent
//! keeps showing `preview`. Refusals are logged, never errors that break the
//! uplink.

use std::sync::atomic::{AtomicU64, Ordering};

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
    client: reqwest::Client,
}

impl Summarizer {
    /// Build from parsed config. `enabled` defaults (in the config layer) to "on
    /// iff a key is set"; with no key it can never spend regardless.
    pub fn new(enabled: bool, api_key: Option<String>, model: String, budget_usd: Option<f64>) -> Self {
        Self {
            enabled,
            api_key,
            model,
            budget_usd,
            spent_micros: AtomicU64::new(0),
            client: reqwest::Client::new(),
        }
    }

    /// A disabled summarizer (no key, off) — declines everything. Used by tests
    /// and as the default when summaries aren't configured.
    pub fn disabled() -> Self {
        Self::new(false, None, cc_screen_summary::DEFAULT_MODEL.to_string(), None)
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

    /// Gate + (if allowed) call Haiku. Never panics; failures are logged and
    /// mapped to [`Outcome::Failed`] so the uplink reader keeps running.
    pub async fn summarize(&self, inputs: &[String], tail: &str) -> Outcome {
        let Some(key) = self.api_key.as_deref() else { return Outcome::Declined };
        if !self.enabled {
            return Outcome::Declined;
        }
        if !self.try_reserve() {
            tracing::warn!("summary: over budget (spent ≈ ${:.2}); declining", self.spent_usd());
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
        let s = Summarizer::new(true, None, "claude-haiku-4-5".into(), None);
        assert!(!s.active());
        // Key + on → active.
        let s = Summarizer::new(true, Some("k".into()), "claude-haiku-4-5".into(), None);
        assert!(s.active());
        // Key but off → inactive.
        let s = Summarizer::new(false, Some("k".into()), "claude-haiku-4-5".into(), None);
        assert!(!s.active());
    }

    #[test]
    fn budget_gate_stops_spending() {
        // Budget of one call's worth: the first reserve succeeds, the next fails.
        let micros = EST_CALL_MICROS as f64 / 1_000_000.0;
        let s = Summarizer::new(true, Some("k".into()), "m".into(), Some(micros));
        assert!(s.try_reserve(), "first call fits the budget");
        assert!(!s.try_reserve(), "second call is over budget");
        // No budget → always reserves.
        let s = Summarizer::new(true, Some("k".into()), "m".into(), None);
        for _ in 0..1000 {
            assert!(s.try_reserve());
        }
    }

    #[tokio::test]
    async fn no_key_summarize_declines_without_calling() {
        let s = Summarizer::new(true, None, "m".into(), None);
        assert!(matches!(s.summarize(&["q".into()], "tail").await, Outcome::Declined));
    }
}
