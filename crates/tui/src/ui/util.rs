//! Small formatting helpers shared by the UI.

use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::{
    style::{Color, Modifier, Style},
    text::Span,
};

/// Map a curated session-mark palette token (proposal 0029,
/// `cc_screen_protocol::SESSION_COLOR_TOKENS`) to a concrete terminal colour, so
/// the switcher row accent + grid border match the web PWA's rendered shade.
///
/// The web owns the shade as `hsl(<hue> 60% 58%)` (`frontend/src/util.ts`
/// `SESSION_COLORS` → `sessionAccent().border`); these RGB values are that HSL
/// converted, so a session marked `teal` on the phone reads teal here too.
///
/// **Display-only, forward-compatible:** an unknown token (a newer palette entry
/// this build doesn't know) or `None` maps to `None` — the caller then falls back
/// to today's default styling, never a broken colour. Setting colours stays
/// web-only.
pub fn color_token_to_color(token: Option<&str>) -> Option<Color> {
    let (r, g, b) = match token? {
        "rose" => (212, 84, 105),
        "magenta" => (212, 84, 169),
        "violet" => (148, 84, 212),
        "indigo" => (84, 105, 212),
        "teal" => (84, 212, 201),
        "lime" => (137, 212, 84),
        "orange" => (212, 137, 84),
        "slate" => (84, 148, 212),
        _ => return None,
    };
    Some(Color::Rgb(r, g, b))
}

/// Truncate to `max` display chars, appending `…` when cut.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Truncate to `max` display *columns* (unicode-width), appending `…` when
/// cut. Unlike `truncate` above this stays aligned for double-width (CJK)
/// text, where char count and rendered columns diverge — used by the grid
/// action menu's fixed columns (0062b) so a wide name can't push the row past
/// the panel border.
pub fn truncate_w(s: &str, max: usize) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    if s.width() <= max {
        return s.to_string();
    }
    let budget = max.saturating_sub(1); // reserve one column for the ellipsis
    let mut out = String::new();
    let mut w = 0usize;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw > budget {
            break;
        }
        out.push(c);
        w += cw;
    }
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

/// A session's folder breadcrumb (proposal 0025) — `None` when the agent
/// couldn't read the cwd, so a caller never shows the slug twice (unlike
/// `dir_crumb`, which falls back to a name for surfaces that need *something*).
pub fn session_crumb(cwd: &str) -> Option<String> {
    let c = dir_crumb(cwd, "");
    (!c.is_empty()).then_some(c)
}

/// Keep the TAIL of `s` within `max` display columns, prefixing `…` when cut —
/// for a path, whose leaf is the part that identifies it.
pub fn truncate_w_left(s: &str, max: usize) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    if s.width() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let budget = max - 1; // one column for the ellipsis
    let mut tail: Vec<char> = Vec::new();
    let mut w = 0usize;
    for c in s.chars().rev() {
        let cw = c.width().unwrap_or(0);
        if w + cw > budget {
            break;
        }
        tail.push(c);
        w += cw;
    }
    let mut out = String::from("…");
    out.extend(tail.into_iter().rev());
    out
}

/// The name-first identity cell (web parity, proposal 0033): the session's
/// display name, then — dim, and first to give way — `· <where>` (a folder
/// breadcrumb, optionally machine-qualified). Exactly `width` display columns,
/// padded, so it can sit in a fixed column. The name always wins the space; the
/// breadcrumb is only drawn when a useful sliver of it fits, and is cut from
/// the LEFT so its leaf survives.
pub fn name_with_crumb(
    name: &str,
    crumb: Option<&str>,
    width: usize,
    name_style: Style,
    crumb_style: Style,
) -> Vec<Span<'static>> {
    use unicode_width::UnicodeWidthStr;
    const SEP: &str = " · ";
    const MIN_CRUMB: usize = 6;
    let name = truncate_w(name, width);
    let mut used = name.width();
    let mut spans = vec![Span::styled(name, name_style)];
    if let Some(c) = crumb.filter(|c| !c.is_empty()) {
        let room = width.saturating_sub(used + SEP.width());
        if room >= MIN_CRUMB.min(c.width()) && room > 0 {
            let c = truncate_w_left(c, room);
            used += SEP.width() + c.width();
            spans.push(Span::styled(SEP, crumb_style.add_modifier(Modifier::DIM)));
            spans.push(Span::styled(c, crumb_style));
        }
    }
    if used < width {
        spans.push(Span::raw(" ".repeat(width - used)));
    }
    spans
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
    fn truncate_w_counts_display_columns() {
        // ASCII: identical to `truncate`.
        assert_eq!(truncate_w("hello", 10), "hello");
        assert_eq!(truncate_w("hello world", 5), "hell…");
        // CJK chars are 2 columns wide: "画面共有" is 4 chars but 8 columns, so
        // a 4-column budget holds one char plus the ellipsis.
        assert_eq!(truncate_w("画面共有", 8), "画面共有");
        assert_eq!(truncate_w("画面共有", 4), "画…");
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
    fn session_crumb_is_none_without_a_cwd() {
        assert_eq!(session_crumb("/home/erik/dev/api").as_deref(), Some("dev/api"));
        assert_eq!(session_crumb(""), None);
        assert_eq!(session_crumb("/"), None);
    }

    #[test]
    fn truncate_w_left_keeps_the_leaf() {
        assert_eq!(truncate_w_left("development/api", 20), "development/api");
        assert_eq!(truncate_w_left("development/api", 8), "…ent/api");
        assert_eq!(truncate_w_left("画面/共有", 5), "…共有");
        assert_eq!(truncate_w_left("abc", 0), "");
    }

    fn cell(spans: &[Span]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn name_with_crumb_is_name_first_and_fixed_width() {
        use unicode_width::UnicodeWidthStr;
        let st = Style::default();
        // Room for both: name, dim separator, full crumb, padded to width.
        let c = cell(&name_with_crumb("api", Some("dev/api"), 20, st, st));
        assert_eq!(c, "api · dev/api       ");
        // Tight: the crumb gives way from the left, the name stays whole.
        let c = cell(&name_with_crumb("refactor", Some("development/api"), 20, st, st));
        assert_eq!(c, "refactor · …ment/api");
        // No sliver worth drawing → name alone.
        let c = cell(&name_with_crumb("a-long-session-name", Some("dev/api"), 22, st, st));
        assert_eq!(c, "a-long-session-name   ");
        // Name longer than the cell → truncated name, no crumb.
        let c = cell(&name_with_crumb("a-very-long-name", Some("x/y"), 8, st, st));
        assert_eq!(c, "a-very-…");
        // No cwd → name alone, still padded.
        let c = cell(&name_with_crumb("shell", None, 8, st, st));
        assert_eq!(c.width(), 8);
        assert_eq!(c.trim_end(), "shell");
    }

    #[test]
    fn color_token_maps_known_unknown_and_none() {
        // A known token → its concrete RGB (the web `teal` shade).
        assert_eq!(color_token_to_color(Some("teal")), Some(Color::Rgb(84, 212, 201)));
        assert_eq!(color_token_to_color(Some("rose")), Some(Color::Rgb(212, 84, 105)));
        // Every curated token resolves (kept in lockstep with the protocol set).
        for tok in cc_screen_protocol::SESSION_COLOR_TOKENS {
            assert!(color_token_to_color(Some(tok)).is_some(), "token {tok} should map");
        }
        // An unknown token (forward-compat) and an absent mark → no accent.
        assert_eq!(color_token_to_color(Some("chartreuse")), None);
        assert_eq!(color_token_to_color(None), None);
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
