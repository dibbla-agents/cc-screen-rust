//! Small formatting helpers shared by the UI.

use std::time::{SystemTime, UNIX_EPOCH};

/// Truncate to `max` display chars, appending `…` when cut.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Compact "time ago" from a unix-seconds timestamp (the session's last
/// activity). Returns "-" for an unknown (zero) or future-ish timestamp.
pub fn ago(unix_secs: i64) -> String {
    if unix_secs <= 0 {
        return "-".into();
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
    let d = now - unix_secs;
    if d < 0 {
        return "now".into();
    }
    match d {
        0..=59 => format!("{d}s"),
        60..=3599 => format!("{}m", d / 60),
        3600..=86399 => format!("{}h", d / 3600),
        _ => format!("{}d", d / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_marks_cut() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 5), "hell…");
    }

    #[test]
    fn ago_buckets() {
        assert_eq!(ago(0), "-");
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
        assert_eq!(ago(now), "0s");
        assert_eq!(ago(now - 90), "1m");
        assert_eq!(ago(now - 7200), "2h");
    }
}
