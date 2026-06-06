// The tool registry — a direct port of the Go build's tmux.go parsing, minus
// the tmux specifics. `cc_tool <cmd> <prefix> <launch-template>` is the single
// source of truth shared with the shell tool; {name} in the template is the
// session's short name. We additionally know each tool's extra-dir startup flag
// and its resume form (for the restore path), with built-in defaults for the
// four known CLIs.

use std::path::PathBuf;

use cc_screen_protocol::{ExtraDirs, ToolInfo};

#[derive(Clone)]
pub struct Tool {
    pub cmd: String,    // shell command, e.g. cc
    pub prefix: String, // session-name prefix, e.g. claude
    pub tmpl: String,   // launch template; {name} -> session short name
    pub extra_flag: Option<String>, // --add-dir / --include-directories
    pub extra_max: u32,             // 0 = unlimited
    pub resume_suffix: Option<String>, // e.g. "--continue", "resume --last"
    pub resume_keep_extra: bool,
    /// The approval-bypass flag, kept *separate* from `tmpl` so a session can
    /// launch without it (proposal 0005's per-session "skip permissions"
    /// switch). `None` = the tool has no such flag (the bare shell) **or** a
    /// pre-0005 user template that still inlines it — either way nothing is
    /// appended, and a `false` skip can't strip an inlined flag.
    pub yolo_flag: Option<String>,
}

impl Tool {
    fn new(cmd: &str, prefix: &str, tmpl: &str) -> Tool {
        Tool {
            cmd: cmd.into(),
            prefix: prefix.into(),
            tmpl: tmpl.into(),
            extra_flag: None,
            extra_max: 0,
            resume_suffix: None,
            resume_keep_extra: false,
            yolo_flag: None,
        }
    }
}

/// The wire DTO for a tool. Shared by `GET /api/tools` and the hub uplink's
/// `Register` so both advertise the registry identically.
pub fn tool_info(t: &Tool) -> ToolInfo {
    ToolInfo {
        cmd: t.cmd.clone(),
        prefix: t.prefix.clone(),
        extra_dirs: t
            .extra_flag
            .is_some()
            .then(|| ExtraDirs { max: (t.extra_max > 0).then_some(t.extra_max) }),
    }
}

/// Strip one layer of matching surrounding quotes, mirroring shell parsing.
fn unquote(s: &str) -> String {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() >= 2 {
        let q = b[0];
        if (q == b'"' || q == b'\'') && b[b.len() - 1] == q {
            return s[1..s.len() - 1].to_string();
        }
    }
    s.to_string()
}

/// Split a line into its leading whitespace-separated tokens, returning the
/// remainder (unsplit) after `n` tokens — so a launch template keeps its spaces.
fn split_head(line: &str, n: usize) -> (Vec<String>, String) {
    let mut toks = Vec::new();
    let mut rest = line.trim_start();
    for _ in 0..n {
        match rest.find(char::is_whitespace) {
            Some(i) => {
                toks.push(rest[..i].to_string());
                rest = rest[i..].trim_start();
            }
            None => {
                toks.push(rest.to_string());
                rest = "";
            }
        }
    }
    (toks, rest.to_string())
}

pub fn load_tools(path: Option<PathBuf>) -> Vec<Tool> {
    if let Some(p) = path {
        if let Ok(text) = std::fs::read_to_string(&p) {
            let parsed = parse(&text);
            if !parsed.is_empty() {
                return with_defaults(parsed);
            }
        }
    }
    with_defaults(defaults())
}

fn defaults() -> Vec<Tool> {
    // The approval-bypass flag is *not* baked into these templates — it lives in
    // `yolo_flag` (filled by `with_defaults`) so a session can opt out of YOLO
    // per 0005. Gemini's `--skip-trust` stays: it's a trust-store convenience,
    // not the dangerous approval bypass.
    vec![
        Tool::new("cc", "claude", "claude --rc 'claude-{name}'"),
        Tool::new("kc", "kimi", "kimi"),
        Tool::new("gc", "gemini", "gemini --skip-trust"),
        Tool::new("coc", "codex", "codex"),
        Tool::new("tt", "shell", "${SHELL:-/bin/bash} -l"),
    ]
}

fn parse(text: &str) -> Vec<Tool> {
    let mut out: Vec<Tool> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let mut extra: Vec<(String, String, u32)> = Vec::new();
    let mut resumes: Vec<(String, String)> = Vec::new();
    let mut yolos: Vec<(String, String)> = Vec::new();

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with("cc_tool_extra_dirs") {
            let (toks, _) = split_head(line, 4);
            if toks.len() >= 3 {
                let max = toks.get(3).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
                extra.push((toks[1].clone(), toks[2].clone(), max));
            }
            continue;
        }
        // cc_tool_yolo <cmd|prefix> <flag> — declare a tool's approval-bypass
        // flag separately so a session can launch without it (0005). The rest of
        // the line (after the key) is the flag, so a multi-token flag works.
        if line.starts_with("cc_tool_yolo") {
            let (toks, rest) = split_head(line, 2);
            if toks.len() >= 2 && !rest.is_empty() {
                yolos.push((toks[1].clone(), unquote(&rest)));
            }
            continue;
        }
        if line.starts_with("cc_tool_resume") {
            let (toks, rest) = split_head(line, 2);
            if toks.len() >= 2 && !rest.is_empty() {
                resumes.push((toks[1].clone(), unquote(&rest)));
            }
            continue;
        }
        if line.starts_with("cc_tool") {
            let (toks, rest) = split_head(line, 3);
            if toks.len() < 3 || rest.is_empty() {
                continue;
            }
            let (cmd, prefix) = (toks[1].clone(), toks[2].clone());
            if seen.contains(&prefix) {
                continue;
            }
            seen.push(prefix.clone());
            out.push(Tool::new(&cmd, &prefix, &unquote(&rest)));
        }
    }

    // Apply declared extra-dir / resume metadata to matching tools.
    for (key, flag, max) in extra {
        for t in out.iter_mut() {
            if t.cmd == key || t.prefix == key {
                t.extra_flag = Some(flag.clone());
                t.extra_max = max;
            }
        }
    }
    for (key, suffix) in resumes {
        for t in out.iter_mut() {
            if t.cmd == key || t.prefix == key {
                t.resume_suffix = Some(suffix.clone());
                t.resume_keep_extra = true;
            }
        }
    }
    for (key, flag) in yolos {
        for t in out.iter_mut() {
            if t.cmd == key || t.prefix == key {
                t.yolo_flag = Some(flag.clone());
            }
        }
    }
    out
}

/// Fill in the built-in extra-dir + resume support for any tool that didn't
/// declare its own (the four known CLIs).
fn with_defaults(mut tools: Vec<Tool>) -> Vec<Tool> {
    for t in tools.iter_mut() {
        if t.extra_flag.is_none() {
            match (t.prefix.as_str(), t.cmd.as_str()) {
                ("claude" | "kimi" | "codex", _) | (_, "cc" | "kc" | "coc") => {
                    t.extra_flag = Some("--add-dir".into());
                }
                ("gemini", _) | (_, "gc") => {
                    t.extra_flag = Some("--include-directories".into());
                    t.extra_max = 5;
                }
                _ => {}
            }
        }
        if t.resume_suffix.is_none() {
            match (t.prefix.as_str(), t.cmd.as_str()) {
                ("claude" | "kimi", _) | (_, "cc" | "kc") => {
                    t.resume_suffix = Some("--continue".into());
                    t.resume_keep_extra = true;
                }
                ("gemini", _) | (_, "gc") => {
                    t.resume_suffix = Some("--resume latest".into());
                    t.resume_keep_extra = true;
                }
                ("codex", _) | (_, "coc") => {
                    t.resume_suffix = Some("resume --last".into());
                    t.resume_keep_extra = false; // codex resume rejects --add-dir
                }
                _ => {}
            }
        }
        if t.yolo_flag.is_none() {
            let cand: Option<&str> = match (t.prefix.as_str(), t.cmd.as_str()) {
                ("claude", _) | (_, "cc") => Some("--dangerously-skip-permissions"),
                ("kimi", _) | (_, "kc") => Some("-y"),
                ("gemini", _) | (_, "gc") => Some("-y"),
                ("codex", _) | (_, "coc") => Some("--dangerously-bypass-approvals-and-sandbox"),
                _ => None,
            };
            // Adopt the built-in flag only if the template doesn't already inline
            // it. A pre-0005 user template that bakes the flag in keeps
            // yolo_flag = None, so skip_permissions=false leaves it YOLO (we don't
            // strip inlined flags) and skip=true won't double it. (0005 §tools.conf.)
            if let Some(flag) = cand {
                if !t.tmpl.split_whitespace().any(|tok| tok == flag) {
                    t.yolo_flag = Some(flag.into());
                }
            }
        }
    }
    tools
}

pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn append_extra_dirs(mut cmd: String, t: &Tool, extra_dirs: &[String]) -> String {
    if let Some(flag) = &t.extra_flag {
        for dir in extra_dirs {
            cmd.push(' ');
            cmd.push_str(&shell_quote(flag));
            cmd.push(' ');
            cmd.push_str(&shell_quote(dir));
        }
    }
    cmd
}

fn launch_command(t: &Tool, name: &str, extra_dirs: &[String]) -> String {
    let cmd = t.tmpl.replace("{name}", name);
    append_extra_dirs(cmd, t, extra_dirs)
}

fn resume_command(t: &Tool, name: &str, extra_dirs: &[String]) -> Option<String> {
    let suffix = t.resume_suffix.as_ref()?;
    let mut cmd = t.tmpl.replace("{name}", name);
    if !suffix.is_empty() {
        cmd.push(' ');
        cmd.push_str(suffix);
    }
    if t.resume_keep_extra {
        cmd = append_extra_dirs(cmd, t, extra_dirs);
    }
    Some(cmd)
}

/// Build the shell command line for a session. Unlike the Go build there is no
/// "; tmux kill-session" tail — we observe the child's exit directly. When
/// `resume` is set we run "(resume) || (launch)" so a tool with nothing to
/// continue (or a rejected resume flag) still falls back to a fresh session.
///
/// `skip_permissions` (0005) gates the tool's `yolo_flag`: when true (the
/// default) and the tool has a flag, it's appended to *both* the fresh and
/// resume forms (so resuming stays YOLO too); when false the flag is omitted and
/// the CLI launches with its normal approval prompts.
pub fn build_launch(
    t: &Tool,
    name: &str,
    extra_dirs: &[String],
    resume: bool,
    skip_permissions: bool,
) -> String {
    let yolo = if skip_permissions { t.yolo_flag.as_deref() } else { None };
    let with_yolo = |mut cmd: String| {
        if let Some(flag) = yolo {
            cmd.push(' ');
            cmd.push_str(flag);
        }
        cmd
    };
    let launch = with_yolo(launch_command(t, name, extra_dirs));
    if resume {
        if let Some(rc) = resume_command(t, name, extra_dirs).map(with_yolo) {
            if rc != launch {
                return format!("({rc}) || ({launch})");
            }
        }
    }
    launch
}

/// tmux-safe short name: collapse anything outside [A-Za-z0-9_-] to '-' and trim.
pub fn sanitize_name(s: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in s.trim().chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch);
            prev_dash = ch == '-';
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_defaults() {
        let t = load_tools(None);
        let claude = t.iter().find(|x| x.prefix == "claude").unwrap();
        assert_eq!(claude.resume_suffix.as_deref(), Some("--continue"));
        assert_eq!(claude.extra_flag.as_deref(), Some("--add-dir"));
        let gem = t.iter().find(|x| x.prefix == "gemini").unwrap();
        assert_eq!(gem.extra_max, 5);
        let codex = t.iter().find(|x| x.prefix == "codex").unwrap();
        assert_eq!(codex.resume_suffix.as_deref(), Some("resume --last"));
        assert!(!codex.resume_keep_extra); // codex resume rejects --add-dir
    }

    #[test]
    fn yolo_flag_split_out_of_templates() {
        // The dangerous flag lives in yolo_flag, never inline in the template.
        let t = load_tools(None);
        let want = [
            ("claude", "--dangerously-skip-permissions"),
            ("kimi", "-y"),
            ("gemini", "-y"),
            ("codex", "--dangerously-bypass-approvals-and-sandbox"),
        ];
        for (prefix, flag) in want {
            let tool = t.iter().find(|x| x.prefix == prefix).unwrap();
            assert_eq!(tool.yolo_flag.as_deref(), Some(flag), "{prefix} yolo_flag");
            assert!(!tool.tmpl.contains(flag), "{prefix} tmpl must not inline the flag");
        }
        // Gemini keeps --skip-trust (a trust-store convenience, not the bypass).
        let gem = t.iter().find(|x| x.prefix == "gemini").unwrap();
        assert!(gem.tmpl.contains("--skip-trust"));
        // The bare shell has no approval flag.
        let sh = t.iter().find(|x| x.prefix == "shell").unwrap();
        assert!(sh.yolo_flag.is_none());
    }

    #[test]
    fn build_launch_gates_yolo_flag() {
        let t = load_tools(None);
        let claude = t.iter().find(|x| x.prefix == "claude").unwrap();
        // skip on (today's default): the flag is present.
        let yolo = build_launch(claude, "proj", &[], false, true);
        assert!(yolo.contains("--dangerously-skip-permissions"), "{yolo}");
        // skip off: launches with normal approval prompts — no flag.
        let safe = build_launch(claude, "proj", &[], false, false);
        assert!(!safe.contains("--dangerously-skip-permissions"), "{safe}");
        // Resume keeps the YOLO flag on both the resume and fallback forms.
        let resumed = build_launch(claude, "proj", &[], true, true);
        assert_eq!(resumed.matches("--dangerously-skip-permissions").count(), 2, "{resumed}");
        // …and drops it from both when skip is off.
        let resumed_safe = build_launch(claude, "proj", &[], true, false);
        assert!(!resumed_safe.contains("--dangerously-skip-permissions"), "{resumed_safe}");
    }

    #[test]
    fn cc_tool_yolo_declared_and_inline_guard() {
        // Explicit cc_tool_yolo wins.
        let cfg = "cc_tool xx myc 'myc run'\ncc_tool_yolo myc --go-fast";
        let t = parse(cfg);
        let t = with_defaults(t);
        let tool = t.iter().find(|x| x.prefix == "myc").unwrap();
        assert_eq!(tool.yolo_flag.as_deref(), Some("--go-fast"));
        let line = build_launch(tool, "p", &[], false, true);
        assert!(line.ends_with("--go-fast"), "{line}");
        // A pre-0005 template that inlines the known flag keeps yolo_flag = None,
        // so skip=false can't strip it (stays YOLO) and skip=true won't double it.
        let inlined = with_defaults(parse(
            "cc_tool cc claude \"claude --dangerously-skip-permissions\"",
        ));
        let c = inlined.iter().find(|x| x.prefix == "claude").unwrap();
        assert!(c.yolo_flag.is_none(), "inlined flag must not adopt a default yolo_flag");
        let on = build_launch(c, "p", &[], false, true);
        assert_eq!(on.matches("--dangerously-skip-permissions").count(), 1, "{on}");
        let off = build_launch(c, "p", &[], false, false);
        assert!(off.contains("--dangerously-skip-permissions"), "can't strip inlined: {off}");
    }

    #[test]
    fn sanitize() {
        assert_eq!(sanitize_name(" my proj!! "), "my-proj");
        assert_eq!(sanitize_name("a/b"), "a-b");
        assert_eq!(sanitize_name("ok_name-1"), "ok_name-1");
    }

    #[test]
    fn launch_and_resume_fallback() {
        let t = with_defaults(vec![Tool::new("cc", "claude", "claude --rc 'claude-{name}'")])
            .pop()
            .unwrap();
        // skip_permissions=false here keeps these assertions focused on the
        // launch/resume shape (yolo gating is covered by build_launch_gates_yolo_flag).
        let fresh = build_launch(&t, "proj", &[], false, false);
        assert_eq!(fresh, "claude --rc 'claude-proj'");
        let resumed = build_launch(&t, "proj", &[], true, false);
        assert!(resumed.contains("--continue"));
        assert!(resumed.contains("||")); // (resume) || (fresh) fallback
        let with_extra = build_launch(&t, "proj", &["/home/u/lib".to_string()], false, false);
        assert!(with_extra.contains("--add-dir") && with_extra.contains("/home/u/lib"));
    }
}
