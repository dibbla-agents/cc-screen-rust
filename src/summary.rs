//! Agent-side capture for the session-status summary (proposal 0022).
//!
//! The agent is the only place the terminal emulator and the keystroke stream
//! live, so input extraction + redaction happen here. The actual Anthropic call
//! is the hub's job (it holds the key + the spend gate); this module just turns
//! raw bytes into clean, redacted material for a `SummaryRequest`.
//!
//! Two pure, unit-tested helpers:
//! - [`normalize_input`] reconstructs the operator's recent typed *submissions*
//!   from the raw input-ring bytes (segment on Enter, apply backspace, strip
//!   escape/control sequences, unwrap bracketed paste).
//! - [`redact`] masks obvious secrets before anything leaves the agent.

pub use cc_screen_summary::Summary;

use cc_screen_protocol::{PASTE_END, PASTE_START};

/// How many recent submissions (+ the in-progress line) to keep from the ring.
pub const KEEP_SUBMISSIONS: usize = 4;

/// Standalone-only self-summarizer (proposal 0022 §0): a pure no-hub agent with
/// `CCWEB_ANTHROPIC_API_KEY` set summarizes its own sessions, reusing the exact
/// agent extract/redact/cache path the hub flow uses — only the Anthropic call
/// moves on-box. The hub remains the canonical home; this never runs when a hub
/// is configured. A changed session (hash gate) is summarized at most once per
/// tick; an idle fleet makes no calls.
pub async fn standalone_summarizer(
    state: crate::engine::AppState,
    api_key: String,
    model: String,
    tail_lines: usize,
    interval_secs: u64,
) {
    let client = reqwest::Client::new();
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(interval_secs.max(5)));
    loop {
        tick.tick().await;
        for sess in state.list() {
            let (hash, inputs, tail) = sess.summary_extract(tail_lines);
            if !sess.summary_candidate(hash) {
                continue;
            }
            sess.mark_summary_requested(hash);
            match cc_screen_summary::summarize(&client, &api_key, &model, &inputs, &tail).await {
                Ok(s) => {
                    sess.store_summary(hash, s.headline, s.detail);
                }
                Err(e) => tracing::warn!("standalone summary failed for {}: {e}", sess.name),
            }
        }
    }
}

/// Reconstruct the operator's recent typed submissions from raw input bytes.
///
/// The raw ring is the exact byte stream that went to the PTY: typed keys, named
/// keys (arrows/function = escape sequences), control combos, Enter (`\r`/`\n`),
/// backspace (`\x7f`/`\x08`), and bracketed-paste-wrapped pastes. We:
/// - segment on Enter (each finished line is one submission; a trailing
///   un-terminated line is the *in-progress* one),
/// - apply backspace within the current line,
/// - strip escape/control sequences so arrows/Ctrl-combos don't pollute the text,
/// - unwrap bracketed paste (drop the markers, keep the content verbatim,
///   including any internal newlines — a paste is one submission).
///
/// Returns the last [`KEEP_SUBMISSIONS`] non-empty segments, oldest first.
pub fn normalize_input(raw: &[u8]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let n = raw.len();
    let mut i = 0;

    let flush = |cur: &mut String, out: &mut Vec<String>| {
        let t = cur.trim().to_string();
        if !t.is_empty() {
            out.push(t);
        }
        cur.clear();
    };

    while i < n {
        // Bracketed paste: keep the inner bytes as data (markers + internal
        // newlines don't segment), so a multi-line paste is a single submission.
        if raw[i..].starts_with(PASTE_START) {
            i += PASTE_START.len();
            let start = i;
            while i < n && !raw[i..].starts_with(PASTE_END) {
                i += 1;
            }
            cur.push_str(&String::from_utf8_lossy(&raw[start..i]));
            if i < n {
                i += PASTE_END.len(); // consume the end marker
            }
            continue;
        }
        let b = raw[i];
        match b {
            0x1b => {
                // An escape sequence (arrow/function key, Ctrl-combo CSI, …) — drop
                // it. CSI (`ESC [`) and SS3 (`ESC O`) run to a final byte; a lone
                // ESC or `ESC <char>` drops a single trailing byte.
                i += 1;
                if i < n && raw[i] == b'[' {
                    i += 1;
                    while i < n && !(0x40..=0x7e).contains(&raw[i]) {
                        i += 1;
                    }
                    if i < n {
                        i += 1; // the final byte
                    }
                } else if i < n {
                    i += 1; // SS3 selector or the single char after a lone ESC
                    if raw.get(i - 1) == Some(&b'O') && i < n {
                        i += 1; // SS3 has one more byte (e.g. ESC O A)
                    }
                }
            }
            b'\r' | b'\n' => {
                flush(&mut cur, &mut out);
                // Treat CRLF as one boundary.
                if b == b'\r' && raw.get(i + 1) == Some(&b'\n') {
                    i += 1;
                }
                i += 1;
            }
            0x7f | 0x08 => {
                cur.pop(); // backspace removes one (whole) char
                i += 1;
            }
            _ if b < 0x20 => {
                i += 1; // other control byte (Ctrl-C, Tab, …) — drop
            }
            _ => {
                // Decode one UTF-8 char so backspace + length stay char-accurate.
                let len = utf8_len(b);
                let end = (i + len).min(n);
                match std::str::from_utf8(&raw[i..end]) {
                    Ok(s) => cur.push_str(s),
                    Err(_) => cur.push('\u{fffd}'),
                }
                i = end;
            }
        }
    }
    // The trailing, un-Entered line is the in-progress submission.
    flush(&mut cur, &mut out);

    if out.len() > KEEP_SUBMISSIONS {
        out.split_off(out.len() - KEEP_SUBMISSIONS)
    } else {
        out
    }
}

/// UTF-8 sequence length implied by a leading byte.
fn utf8_len(b: u8) -> usize {
    match b {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1, // continuation/invalid byte → consume one (lossy)
    }
}

/// Best-effort secret masking over a single line/string. Documented as
/// best-effort: it catches the obvious shapes (`KEY=…`, `Authorization: Bearer …`,
/// `sk-…`, AWS `AKIA…`, JWT-shaped `a.b.c`, long hex/base64 blobs) before the
/// extract leaves the agent. Replaces the secret with `«redacted»`.
pub fn redact(text: &str) -> String {
    text.lines().map(redact_line).collect::<Vec<_>>().join("\n")
}

const MASK: &str = "«redacted»";

fn redact_line(line: &str) -> String {
    // `Authorization: Bearer <token>` (or `Authorization: <token>`).
    if let Some(pos) = find_ci(line, "authorization:") {
        let (head, _tail) = line.split_at(pos + "authorization:".len());
        return format!("{head} {MASK}");
    }
    // `NAME=value` where NAME smells secret → mask the value.
    if let Some(eq) = line.find('=') {
        let key = line[..eq].trim_end();
        let key_tail = key.rsplit(|c: char| c.is_whitespace()).next().unwrap_or(key);
        if key_is_secret(key_tail) && !line[eq + 1..].trim().is_empty() {
            return format!("{}={MASK}", &line[..eq]);
        }
    }
    // Token-shaped substrings anywhere on the line.
    line.split_inclusive(char::is_whitespace)
        .map(|tok| {
            let trimmed = tok.trim_end();
            if trimmed.len() == tok.len() {
                mask_token(tok)
            } else {
                format!("{}{}", mask_token(trimmed), &tok[trimmed.len()..])
            }
        })
        .collect()
}

/// Case-insensitive ASCII substring search → byte offset of the first match.
fn find_ci(haystack: &str, needle: &str) -> Option<usize> {
    let hl = haystack.to_ascii_lowercase();
    hl.find(&needle.to_ascii_lowercase())
}

fn key_is_secret(key: &str) -> bool {
    let k = key.to_ascii_uppercase();
    const NEEDLES: [&str; 6] = ["KEY", "TOKEN", "PASSWORD", "SECRET", "PASSWD", "APIKEY"];
    NEEDLES.iter().any(|n| k.contains(n))
}

/// Mask a single whitespace-delimited token if it looks like a credential.
fn mask_token(tok: &str) -> String {
    if looks_secret(tok) {
        MASK.to_string()
    } else {
        tok.to_string()
    }
}

fn looks_secret(tok: &str) -> bool {
    let t = tok.trim_matches(|c: char| matches!(c, '"' | '\'' | ',' | ';' | '(' | ')'));
    if t.len() < 16 {
        // Short tokens: only the unmistakable provider prefixes.
        return t.starts_with("sk-") || t.starts_with("AKIA");
    }
    // Provider-prefixed keys.
    if t.starts_with("sk-") || t.starts_with("AKIA") || t.starts_with("ghp_") || t.starts_with("xox") {
        return true;
    }
    // JWT-shaped: three base64url segments separated by dots.
    let segs: Vec<&str> = t.split('.').collect();
    if segs.len() == 3 && segs.iter().all(|s| s.len() >= 4 && is_b64ish(s)) {
        return true;
    }
    // Long high-entropy blob (hex/base64-ish, no spaces).
    t.len() >= 32 && is_b64ish(t) && has_mixed_classes(t)
}

fn is_b64ish(s: &str) -> bool {
    s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '+' | '/' | '='))
}

fn has_mixed_classes(s: &str) -> bool {
    let has_digit = s.chars().any(|c| c.is_ascii_digit());
    let has_alpha = s.chars().any(|c| c.is_ascii_alphabetic());
    has_digit && has_alpha
}

/// A cheap, order-stable content hash over the summary inputs (the typed
/// submissions + the terminal tail). Drives the change gate: equal hash ⇒ nothing
/// changed ⇒ no request. Uses `DefaultHasher` (not cryptographic — only needs to
/// detect change).
pub fn content_hash(inputs: &[String], tail: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    inputs.hash(&mut h);
    tail.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cc_screen_protocol::wrap_bracketed_paste;

    #[test]
    fn segments_on_enter_in_order() {
        let raw = b"fix the auth bug\ry\r";
        assert_eq!(normalize_input(raw), vec!["fix the auth bug", "y"]);
    }

    #[test]
    fn backspace_reconstructs_final_text() {
        // "helllo" + two backspaces + "o" → "hello"
        let raw = b"helllo\x7f\x7fo\r";
        assert_eq!(normalize_input(raw), vec!["hello"]);
    }

    #[test]
    fn strips_escape_and_control_sequences() {
        // Arrow keys (CSI), an SS3 arrow, and a Ctrl-C, interleaved with text.
        let raw = b"ab\x1b[Ade\x1bOBf\x03g\r";
        assert_eq!(normalize_input(raw), vec!["abdefg"]);
    }

    #[test]
    fn unwraps_multiline_paste_as_one_submission() {
        let pasted = wrap_bracketed_paste("line one\nline two", true);
        let got = normalize_input(&pasted);
        assert_eq!(got, vec!["line one\nline two"]);
    }

    #[test]
    fn in_progress_line_is_kept() {
        let raw = b"done one\rtyping this"; // no trailing Enter on the last
        assert_eq!(normalize_input(raw), vec!["done one", "typing this"]);
    }

    #[test]
    fn keeps_only_the_last_few_submissions() {
        let raw = b"one\rtwo\rthree\rfour\rfive\rsix\r";
        let got = normalize_input(raw);
        assert_eq!(got.len(), KEEP_SUBMISSIONS);
        assert_eq!(got.last().unwrap(), "six");
        assert!(!got.contains(&"one".to_string()));
    }

    #[test]
    fn redacts_obvious_secrets() {
        let r = redact("export OPENAI_API_KEY=sk-abc123def456ghi789jklmno");
        assert!(r.contains("«redacted»"), "key value masked: {r}");
        assert!(!r.contains("sk-abc123"), "raw secret leaked: {r}");

        let r = redact("Authorization: Bearer eyJhbGciOiJIUzI1Ni) very-secret");
        assert!(r.starts_with("Authorization:"));
        assert!(r.contains("«redacted»"));
        assert!(!r.contains("eyJhbGci"));

        // A JWT-shaped token anywhere on the line.
        let jwt = "header.payloadpayload.signaturesig";
        let r = redact(&format!("token is {jwt} ok"));
        assert!(r.contains("«redacted»"), "jwt masked: {r}");
    }

    #[test]
    fn leaves_ordinary_text_alone() {
        let plain = "fix the auth bug and run the tests please";
        assert_eq!(redact(plain), plain);
        let path = "edited src/engine.rs and src/handlers.rs today";
        assert_eq!(redact(path), path);
    }

    #[test]
    fn hash_changes_with_content_and_is_stable() {
        let a = content_hash(&["q".into()], "out");
        let b = content_hash(&["q".into()], "out");
        assert_eq!(a, b, "same content → same hash");
        assert_ne!(a, content_hash(&["q".into()], "out2"), "tail change → new hash");
        assert_ne!(a, content_hash(&["q2".into()], "out"), "input change → new hash");
    }
}
