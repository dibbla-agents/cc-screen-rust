// `cc-screen-rust doctor` — the preflight for the coding-assistant CLIs this
// agent drives (proposal 0046). cc-screen doesn't *contain* claude/codex/gemini/
// kimi, it launches them by name — so this is the authoritative "which of them
// exist on this machine" check. Authoritative because it probes the exact
// session PATH (`config::load().env_path`, the PATH `engine.rs` spawns with) and
// the exact tool registry sessions launch from — the shell installers call this
// instead of keeping their own list, so installer and runtime can't drift.
//
//   doctor             report-only, exit 0 (the install scripts' non-interactive path)
//   doctor --strict    exit 1 if any assistant is missing (CI)
//   doctor --install   on a TTY, offer to run each missing CLI's installer
//   doctor --update    update the installed CLIs, reporting from → to (0049).
//                      NOTE `cc-screen-rust update` updates the AGENT itself; this
//                      updates the assistants it drives, and restarts no sessions.
//
// A missing assistant is never fatal to an agent install: a machine with only
// `claude` is a perfectly good agent for claude sessions (the runtime guard in
// `create_core` keeps the others from failing opaquely).

use std::io::IsTerminal;

use crate::tools::{self, Tool};

/// One probed row: the tool, its probe binary, and whether it's present.
struct Row {
    tool: Tool,
    bin: String,
    present: bool,
}

fn probe_rows(tools: &[Tool], env_path: &str) -> Vec<Row> {
    tools
        .iter()
        .filter_map(|t| {
            let bin = tools::probe_binary(t)?.to_string(); // shell tool → no probe
            let present = tools::binary_on_path(&bin, env_path);
            Some(Row { tool: t.clone(), bin, present })
        })
        .collect()
}

fn label(t: &Tool) -> &str {
    tools::assistant_for(t).map(|a| a.label).unwrap_or(&t.prefix)
}

fn print_report(rows: &[Row]) {
    println!("Coding assistants on this machine (session PATH):");
    for r in rows {
        if r.present {
            println!("  ✓ {:<10} ({})", r.bin, label(&r.tool));
        } else {
            match tools::install_hint(&r.tool) {
                Some(hint) => println!("  ✗ {:<10} not found — install: {hint}", r.bin),
                None => println!("  ✗ {:<10} not found", r.bin),
            }
        }
    }
}

/// Best-effort report for `cc-screen-rust install` (service setup): print the
/// table, never fail the install over it — same posture as the clipboard shim.
pub fn install_report() {
    let cfg = crate::config::load();
    let tools = tools::load_tools(cfg.tools_path);
    let rows = probe_rows(&tools, &cfg.env_path);
    println!();
    print_report(&rows);
    if rows.iter().any(|r| !r.present) {
        println!("  (missing ones are optional — sessions for the installed CLIs work now;");
        println!("   run `cc-screen-rust doctor --install` to add the others.)");
    }
}

/// The `doctor` subcommand. Returns the process exit code.
pub fn run(args: &[String]) -> i32 {
    let strict = args.iter().any(|a| a == "--strict");
    let install = args.iter().any(|a| a == "--install");
    let update = args.iter().any(|a| a == "--update");

    let cfg = crate::config::load();
    let tools = tools::load_tools(cfg.tools_path);
    let mut rows = probe_rows(&tools, &cfg.env_path);
    print_report(&rows);

    // `--update` (proposal 0049): update the installed CLIs and report from → to.
    // Deliberately does NOT restart sessions — a CLI invocation is a different
    // process from the running agent and has no business reaching into its
    // registry. The web action (POST /api/assistants/update) is what also
    // restarts them. Note the name split: `cc-screen-rust update` updates the
    // AGENT; `doctor --update` updates the assistants it drives.
    let mut update_failed = false;
    if update {
        if run_updates(&rows, &cfg.env_path) {
            update_failed = true;
        }
    }

    if install {
        if !std::io::stdin().is_terminal() {
            // A piped `curl | sh` can't prompt (and reads would eat the script);
            // degrade to the report the caller already got.
            eprintln!("doctor: --install needs an interactive terminal; reported only.");
        } else {
            offer_installs(&mut rows, &cfg.env_path);
        }
    }

    if strict && (rows.iter().any(|r| !r.present) || update_failed) {
        return 1;
    }
    0
}

/// Run the update for every present assistant and print a version-carrying
/// report row each. Returns whether any update failed (only `--strict` acts on
/// it — a failed update is reported, never fatal to a plain run).
fn run_updates(rows: &[Row], env_path: &str) -> bool {
    use crate::assistants::UpdateOutcome;
    let mut failed = false;
    println!();
    println!("Updating the installed assistants (one at a time):");
    for r in rows {
        match crate::assistants::update_tool(&r.tool, env_path) {
            UpdateOutcome::Updated { from, to } => {
                println!("  ✓ {:<10} {:<14} {from} → {to}", r.bin, label(&r.tool))
            }
            UpdateOutcome::Current { version } => {
                println!("  ✓ {:<10} {:<14} {version} (already current)", r.bin, label(&r.tool))
            }
            UpdateOutcome::Failed { error, .. } => {
                failed = true;
                println!("  ✗ {:<10} {:<14} {error}", r.bin, label(&r.tool));
            }
            UpdateOutcome::Skipped { reason } => {
                let hint = tools::install_hint(&r.tool)
                    .map(|c| format!(" — install: {c}"))
                    .unwrap_or_default();
                println!("  – {:<10} {:<14} {reason}{hint}", r.bin, label(&r.tool));
            }
        }
    }
    println!("  (this updates the binaries only; the web UI's \"Update coding assistants\"");
    println!("   action also restarts sessions, resuming each conversation.)");
    failed
}

/// Prompt per missing assistant, run its installer via `/bin/sh -c` with the
/// session PATH, and re-probe. The user sees the exact command before it runs;
/// declining (or a failed install) just moves on — never aborts.
fn offer_installs(rows: &mut [Row], env_path: &str) {
    use std::io::Write;
    for r in rows.iter_mut().filter(|r| !r.present) {
        let Some(hint) = tools::install_hint(&r.tool) else {
            println!("→ {}: no known install command (declare one with cc_tool_install)", r.bin);
            continue;
        };
        print!("Install {} with `{hint}`? [y/N] ", label(&r.tool));
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err() {
            return;
        }
        if !matches!(line.trim(), "y" | "Y" | "yes") {
            continue;
        }
        let status = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(&hint)
            .env("PATH", env_path)
            .status();
        r.present = tools::binary_on_path(&r.bin, env_path);
        match (status, r.present) {
            (Ok(s), true) if s.success() => println!("  ✓ {} installed", r.bin),
            (Ok(s), false) if s.success() => println!(
                "  ✗ installer finished but `{}` still isn't on the session PATH — you may need a new shell or a PATH entry",
                r.bin
            ),
            (Ok(s), _) => println!("  ✗ installer exited with {s}; `{}` {}", r.bin, if r.present { "is now present anyway" } else { "still missing" }),
            (Err(e), _) => println!("  ✗ couldn't run the installer: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(prefix: &str, tmpl: &str) -> Tool {
        Tool {
            cmd: format!("{prefix}-cmd"),
            prefix: prefix.to_string(),
            tmpl: tmpl.to_string(),
            extra_flag: None,
            extra_max: 0,
            resume_suffix: None,
            resume_keep_extra: false,
            yolo_flag: None,
            install_hint: None,
            update_cmd: None,
        }
    }

    #[test]
    fn probe_rows_skip_the_shell_and_flag_missing() {
        let dir = std::env::temp_dir().join(format!("ccdoc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let present = dir.join("present-cli");
        std::fs::write(&present, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&present, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let tools = vec![
            tool("present", "present-cli"),
            tool("missing", "missing-cli --flag"),
            tool("shell", "${SHELL:-/bin/bash} -l"),
        ];
        let rows = probe_rows(&tools, dir.to_str().unwrap());
        // The shell tool contributes no row; the others probe by template head.
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|r| r.bin == "present-cli" && r.present));
        assert!(rows.iter().any(|r| r.bin == "missing-cli" && !r.present));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
