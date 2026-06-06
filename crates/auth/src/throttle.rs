//! Login throttling — blunts online password guessing on the documented
//! reverse-proxy-to-internet hub (a human-chosen password is a guessable surface;
//! the old fixed 250 ms per-request sleep is parallelizable and doesn't compound).
//!
//! Per-source (keyed by `X-Forwarded-For` behind the documented proxy, else a
//! single global bucket for direct tailnet) consecutive failures escalate a
//! lockout window with exponential backoff; the first couple of failures only get
//! the caller's fixed delay, so a fat-fingered password isn't punished. A correct
//! login clears the source. Axum-free: it works on plain strings + `Instant`.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use http::HeaderMap;

/// No lockout for the first `LOCK_AFTER - 1` failures (just the fixed delay).
const LOCK_AFTER: u32 = 3;
/// Cap the lockout window.
const MAX_LOCK_SECS: u64 = 30;
/// Bound the per-source table so a flood of distinct sources can't grow it without
/// limit; stale (unlocked) entries are pruned past this size.
const MAX_TRACKED: usize = 4096;

#[derive(Default)]
pub struct LoginThrottle {
    sources: Mutex<HashMap<String, Attempt>>,
}

struct Attempt {
    fails: u32,
    locked_until: Option<Instant>,
}

impl LoginThrottle {
    pub fn new() -> LoginThrottle {
        LoginThrottle::default()
    }

    /// The remaining lockout for `source`, if it's currently locked out. Callers
    /// reject the attempt (e.g. `429`) without even checking the password.
    pub fn locked_for(&self, source: &str, now: Instant) -> Option<Duration> {
        let g = self.sources.lock().unwrap();
        g.get(source)
            .and_then(|a| a.locked_until)
            .filter(|u| *u > now)
            .map(|u| u - now)
    }

    /// Record a failed attempt for `source`; returns the new lockout window
    /// (`ZERO` for the first few failures).
    pub fn record_failure(&self, source: &str, now: Instant) -> Duration {
        let mut g = self.sources.lock().unwrap();
        if g.len() >= MAX_TRACKED {
            g.retain(|_, a| a.locked_until.map(|u| u > now).unwrap_or(false));
        }
        let a = g.entry(source.to_string()).or_insert(Attempt { fails: 0, locked_until: None });
        a.fails = a.fails.saturating_add(1);
        let lock = backoff_for(a.fails);
        a.locked_until = (!lock.is_zero()).then_some(now + lock);
        lock
    }

    /// Clear a source after a correct login.
    pub fn record_success(&self, source: &str) {
        self.sources.lock().unwrap().remove(source);
    }
}

/// Pure backoff: no lockout for the first `LOCK_AFTER - 1` failures, then 1s, 2s,
/// 4s, … doubling, capped at `MAX_LOCK_SECS`.
pub fn backoff_for(fails: u32) -> Duration {
    if fails < LOCK_AFTER {
        return Duration::ZERO;
    }
    let steps = fails - LOCK_AFTER;
    let secs = 1u64.checked_shl(steps).unwrap_or(u64::MAX).min(MAX_LOCK_SECS);
    Duration::from_secs(secs)
}

/// The throttle key for a request: the first `X-Forwarded-For` hop (set by the
/// documented TLS reverse proxy) if present, else a single `"global"` bucket
/// (direct tailnet has no per-source signal without it).
pub fn source_key(headers: &HeaderMap) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| "global".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps() {
        assert_eq!(backoff_for(0), Duration::ZERO);
        assert_eq!(backoff_for(1), Duration::ZERO);
        assert_eq!(backoff_for(2), Duration::ZERO);
        assert_eq!(backoff_for(3), Duration::from_secs(1));
        assert_eq!(backoff_for(4), Duration::from_secs(2));
        assert_eq!(backoff_for(5), Duration::from_secs(4));
        assert_eq!(backoff_for(50), Duration::from_secs(MAX_LOCK_SECS));
        assert_eq!(backoff_for(u32::MAX), Duration::from_secs(MAX_LOCK_SECS));
    }

    #[test]
    fn lockout_engages_after_repeated_failures_and_clears_on_success() {
        let t = LoginThrottle::new();
        let t0 = Instant::now();
        assert!(t.locked_for("1.2.3.4", t0).is_none(), "starts unlocked");
        // First two failures: no lockout yet.
        assert!(t.record_failure("1.2.3.4", t0).is_zero());
        assert!(t.record_failure("1.2.3.4", t0).is_zero());
        assert!(t.locked_for("1.2.3.4", t0).is_none());
        // Third failure → locked for ~1s.
        let lock = t.record_failure("1.2.3.4", t0);
        assert_eq!(lock, Duration::from_secs(1));
        assert!(t.locked_for("1.2.3.4", t0).is_some(), "now locked");
        // A different source is unaffected.
        assert!(t.locked_for("9.9.9.9", t0).is_none());
        // After the window passes, it unlocks.
        assert!(t.locked_for("1.2.3.4", t0 + Duration::from_secs(2)).is_none());
        // Success clears the counter entirely.
        t.record_success("1.2.3.4");
        assert!(t.record_failure("1.2.3.4", t0).is_zero(), "counter reset after success");
    }

    #[test]
    fn source_key_prefers_forwarded_for() {
        let mut h = HeaderMap::new();
        assert_eq!(source_key(&h), "global");
        h.insert("x-forwarded-for", "203.0.113.7, 10.0.0.1".parse().unwrap());
        assert_eq!(source_key(&h), "203.0.113.7");
    }
}
