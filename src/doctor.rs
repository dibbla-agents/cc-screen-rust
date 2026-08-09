// `cc-screen-rust doctor` — the preflight for the coding-assistant CLIs this
// agent drives (proposal 0046). cc-screen doesn't *contain* claude/codex/gemini/
// kimi, it launches them by name — so this is the authoritative "which of them
// exist on this machine" check. Authoritative because it probes the exact
// session PATH (`config::load().env_path`, the PATH `engine.rs` spawns with) and
// the exact tool registry sessions launch from — the shell installers call this
// instead of keeping their own list, so installer and runtime can't drift.
//
//   doctor                   report-only, exit 0 (the install scripts' non-interactive path)
//   doctor --strict          exit 1 if any assistant is missing (CI)
//   doctor --install         on a TTY, offer to run each missing CLI's installer
//   doctor --install --yes   install every missing CLI, no prompts (0050) — the
//                            explicit "I already consented" switch, which is what
//                            every *piped* installer needs: a `curl … | sh` has no
//                            TTY by construction, so --install alone degraded to a
//                            report and a machine enrolled the SaaS way never
//                            installed an assistant.
//   doctor --install --yes --only claude,codex    …narrowed to those
//   doctor --update    update the installed CLIs, reporting from → to (0049).
//                      NOTE `cc-screen-rust update` updates the AGENT itself; this
//                      updates the assistants it drives, and restarts no sessions.
//
// Installing goes through `provision::install_tool` — the one runner the web
// action uses too — so the CLI path gets user-scope prefixes, prerequisites and
// the landing zone, and there is exactly one install implementation. It restarts
// no sessions: a CLI invocation is a different process from the running agent.
//
// A missing assistant is never fatal to an agent install: a machine with only
// `claude` is a perfectly good agent for claude sessions (the runtime guard in
// `create_core` keeps the others from failing opaquely).

use std::io::IsTerminal;

use crate::tools::{self, Tool};

/// One probed row: the tool, its probe binary, whether it's present, and — when
/// it is — the file the probe resolved it to (proposal 0051), which makes
/// "*which* claude?" answerable from the report.
struct Row {
    tool: Tool,
    bin: String,
    present: bool,
    resolved: Option<std::path::PathBuf>,
}

fn probe_rows(tools: &[Tool], env_path: &str) -> Vec<Row> {
    tools
        .iter()
        .filter_map(|t| {
            let bin = tools::probe_binary(t)?.to_string(); // shell tool → no probe
            let resolved = tools::resolve_on_path(&bin, env_path);
            Some(Row { tool: t.clone(), bin, present: resolved.is_some(), resolved })
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
            match &r.resolved {
                Some(p) => println!("  ✓ {:<10} ({})  {}", r.bin, label(&r.tool), p.display()),
                None => println!("  ✓ {:<10} ({})", r.bin, label(&r.tool)),
            }
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
        println!("   run `cc-screen-rust doctor --install --yes` to add the others. It installs");
        println!("   them for the current user under ~/.local — no sudo, nothing system-wide.)");
    }
}

/// The `doctor` subcommand. Returns the process exit code.
pub fn run(args: &[String]) -> i32 {
    let strict = args.iter().any(|a| a == "--strict");
    let install = args.iter().any(|a| a == "--install");
    let update = args.iter().any(|a| a == "--update");
    let yes = args.iter().any(|a| a == "--yes" || a == "-y");
    let only = only_list(args);

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

    let mut install_failed = false;
    if install {
        // `--only a,b` narrows the set; an unknown name simply matches nothing.
        let selected: Vec<&mut Row> = rows
            .iter_mut()
            .filter(|r| !r.present)
            .filter(|r| only.is_empty() || only.iter().any(|o| o == &r.tool.prefix || o == &r.tool.cmd))
            .collect();
        if yes {
            install_failed = run_installs(selected, &cfg.env_path, &cfg.home);
        } else if !std::io::stdin().is_terminal() {
            // A piped `curl | sh` can't prompt (and reads would eat the script).
            // Degrade to the report the caller already got — and name the switch
            // that makes this work unattended (0050 G1).
            eprintln!(
                "doctor: --install needs an interactive terminal; reported only. \
                 Use `--install --yes` to install without prompting."
            );
        } else {
            install_failed = offer_installs(selected, &cfg.env_path, &cfg.home);
        }
    }

    if strict && (rows.iter().any(|r| !r.present) || update_failed || install_failed) {
        return 1;
    }
    0
}

/// The `--only a,b` / `--only a --only b` selection, normalised. Empty = all.
fn only_list(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut want = false;
    for a in args {
        if want {
            out.extend(a.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string));
            want = false;
        } else if a == "--only" {
            want = true;
        } else if let Some(v) = a.strip_prefix("--only=") {
            out.extend(v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string));
        }
    }
    out
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

/// Prompt per missing assistant, then install through the one runner. The user
/// sees the exact command (and what it needs) before it runs; declining, or a
/// failed install, just moves on — never aborts. Returns whether any failed.
fn offer_installs(rows: Vec<&mut Row>, env_path: &str, home: &std::path::Path) -> bool {
    use std::io::Write;
    let mut failed = false;
    let mut accepted: Vec<&mut Row> = Vec::new();
    for r in rows {
        let Some(cmd) = tools::install_command(&r.tool) else {
            let docs = tools::docs_url(&r.tool)
                .map(|d| format!("see {d}"))
                .unwrap_or_else(|| "declare one with cc_tool_install".into());
            println!("→ {}: no install command for this platform — {docs}", r.bin);
            continue;
        };
        // Name the prerequisites in the prompt: consent should state the blast
        // radius, and a Node bootstrap is ~120 MB the user should see coming.
        let needs: Vec<String> = tools::prereqs_for(&r.tool)
            .into_iter()
            .filter(|p| !tools::binary_on_path(p.bin, env_path))
            .map(|p| format!("{} ({})", p.label, tools::prereq_size_hint(p)))
            .collect();
        if !needs.is_empty() {
            println!("  {} also needs: {}", label(&r.tool), needs.join(", "));
        }
        print!("Install {} with `{cmd}`? [y/N] ", label(&r.tool));
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err() {
            break;
        }
        if matches!(line.trim(), "y" | "Y" | "yes") {
            accepted.push(r);
        }
    }
    if !accepted.is_empty() {
        failed = run_installs(accepted, env_path, home);
    }
    failed
}

/// Install each selected row through `provision::install_tool` and print one row
/// per assistant. Returns whether any failed (only `--strict` acts on it — a
/// failed assistant install is never fatal to a machine install, [0046]'s
/// posture).
fn run_installs(rows: Vec<&mut Row>, env_path: &str, home: &std::path::Path) -> bool {
    use crate::provision::InstallOutcome;
    if rows.is_empty() {
        return false;
    }
    let mut failed = false;
    println!();
    println!("Installing the missing assistants (for this user, under ~/.local — no sudo):");
    for r in rows {
        match crate::provision::install_tool(&r.tool, env_path, home) {
            InstallOutcome::Installed { version, via } => {
                r.present = true;
                r.resolved = tools::resolve_on_path(&r.bin, env_path);
                let v = if version.is_empty() { String::new() } else { format!(" {version}") };
                println!("  ✓ {:<10} {:<14} installed{v} ({via})", r.bin, label(&r.tool));
            }
            InstallOutcome::AlreadyPresent { version } => {
                r.present = true;
                println!("  – {:<10} {:<14} already installed ({version})", r.bin, label(&r.tool));
            }
            InstallOutcome::Failed { error } => {
                failed = true;
                println!("  ✗ {:<10} {:<14} {error}", r.bin, label(&r.tool));
            }
            InstallOutcome::Unsupported { reason } => {
                println!("  – {:<10} {:<14} {reason}", r.bin, label(&r.tool));
            }
        }
    }
    println!("  (this installs the binaries only; the web UI's \"Update coding assistants\"");
    println!("   action also restores the sessions a missing CLI was blocking.)");
    failed
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
            image_paste: crate::tools::ImagePasteStrategy::ClipboardProbe,
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
