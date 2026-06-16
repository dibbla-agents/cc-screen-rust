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

/// The last two segments of an absolute path as a `parent/leaf` breadcrumb
/// (proposal 0025), falling back to `name` when there's no usable cwd. A far
/// better disambiguator than the bare leaf for sessions auto-named after the dir
/// basename (`…/projectA/frontend` vs `…/projectB/frontend`).
///   "/home/erik/development/cc-screen-rust" → "development/cc-screen-rust"
///   "/home/erik"                            → "home/erik"
///   "/home"                                 → "home"
///   "" / "/"                                → `name`
pub fn dir_crumb(cwd: &str, name: &str) -> String {
    let segs: Vec<&str> = cwd.split('/').filter(|s| !s.is_empty()).collect();
    match segs.as_slice() {
        [.., parent, leaf] => format!("{parent}/{leaf}"),
        [leaf] => leaf.to_string(),
        [] => name.to_string(),
    }
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
    fn dir_crumb_segments() {
        assert_eq!(dir_crumb("/home/erik/development/cc-screen-rust", "x"), "development/cc-screen-rust");
        assert_eq!(dir_crumb("/home/erik", "x"), "home/erik");
        assert_eq!(dir_crumb("/home", "x"), "home");
        assert_eq!(dir_crumb("/", "fallback"), "fallback");
        assert_eq!(dir_crumb("", "fallback"), "fallback");
        // Trailing slash is ignored (empty segments filtered).
        assert_eq!(dir_crumb("/a/b/c/", "x"), "b/c");
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
