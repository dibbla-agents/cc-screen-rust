// `cc-screen-rust install` / `uninstall` — the binary wires up its own
// long-running service, so the dist-shipped binary needs no separate install
// script (the tailscale/caddy pattern). Linux → systemd --user; macOS → launchd
// LaunchAgent. Tailnet-only by design, same as the server itself (see config.rs).
//
// The unit/plist *builders* are pure functions (testable); the install drivers
// compute the inputs (binary path, bind addr, PATH) and shell out to
// systemctl / launchctl.

use std::path::{Path, PathBuf};
use std::process::Command;

const SYSTEMD_UNIT: &str = "cc-screen-rust.service";
const LAUNCHD_LABEL: &str = "com.dibbla.cc-screen-rust";

// Wait for the bound (tailnet) IP to exist before starting, so a boot where
// Tailscale comes up late doesn't crash-loop the bind. Loopback/wildcard skip
// the wait. Kept verbatim from install.sh; uses CCWEB_ADDR from the env file.
const SYSTEMD_EXEC_START_PRE: &str = r#"ExecStartPre=/bin/sh -c 'a="${CCWEB_ADDR}"; ip="${a%%:*}"; case "$ip" in ""|0.0.0.0|127.0.0.1|localhost) exit 0;; esac; for i in $(seq 1 60); do ip -o addr show 2>/dev/null | grep -Fqw "$ip" && exit 0; sleep 1; done; exit 0'"#;

struct Opts {
    port: u16,
    bind: Option<String>,
    no_restore: bool,
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn print_help() {
    println!(
        "cc-screen-rust install — set up the auto-starting service for this binary\n\
         \n\
         usage:\n\
         \u{20}\u{20}cc-screen-rust install [--port N] [--bind ADDR] [--no-restore]\n\
         \u{20}\u{20}cc-screen-rust uninstall\n\
         \n\
         --port N        port to serve on (default 8839)\n\
         --bind ADDR     bind address (default: the tailnet IP, else 127.0.0.1)\n\
         --no-restore    don't auto-resume recorded sessions at startup\n\
         \n\
         Linux installs a systemd --user unit; macOS a launchd LaunchAgent."
    );
}

fn parse_opts(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        port: 8839,
        bind: None,
        no_restore: false,
    };
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "-p" || a == "--port" {
            i += 1;
            let v = args.get(i).ok_or("--port needs a value")?;
            o.port = v.parse().map_err(|_| format!("invalid --port: {v}"))?;
        } else if let Some(v) = a.strip_prefix("--port=") {
            o.port = v.parse().map_err(|_| format!("invalid --port: {v}"))?;
        } else if a == "-b" || a == "--bind" {
            i += 1;
            o.bind = Some(args.get(i).ok_or("--bind needs a value")?.clone());
        } else if let Some(v) = a.strip_prefix("--bind=") {
            o.bind = Some(v.to_string());
        } else if a == "--no-restore" {
            o.no_restore = true;
        } else {
            return Err(format!("unknown option: {a}"));
        }
        i += 1;
    }
    Ok(o)
}

/// Prefer the tailnet IP (mirrors install.sh); fall back to loopback.
fn detect_bind() -> String {
    if let Ok(out) = Command::new("tailscale").args(["ip", "-4"]).output() {
        if out.status.success() {
            if let Some(line) = String::from_utf8_lossy(&out.stdout).lines().next() {
                let ip = line.trim();
                if !ip.is_empty() {
                    return ip.to_string();
                }
            }
        }
    }
    "127.0.0.1".to_string()
}

/// Bake ~/.local/bin onto PATH so the engine can find the agent CLIs (claude, …);
/// a systemd --user unit / launchd job otherwise inherits a minimal PATH.
fn svc_path() -> String {
    let local = home().join(".local").join("bin");
    let local = local.to_string_lossy().to_string();
    let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
    if path.split(':').any(|p| p == local) {
        path
    } else {
        format!("{local}:{path}")
    }
}

fn run(cmd: &str, args: &[&str], ignore_err: bool) -> Result<(), String> {
    let status = Command::new(cmd)
        .args(args)
        .status()
        .map_err(|e| format!("failed to run `{cmd}`: {e}"))?;
    if !status.success() && !ignore_err {
        return Err(format!("`{cmd} {}` failed ({status})", args.join(" ")));
    }
    Ok(())
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

// ── pure builders (testable) ───────────────────────────────────────────────

fn systemd_unit(bin: &str, env_file: &str, path: &str, no_restore: bool) -> String {
    let norestore = if no_restore { " --no-restore" } else { "" };
    let mut u = String::new();
    u.push_str("[Unit]\n");
    u.push_str("Description=cc-screen-rust — tmux-free phone UI for AI CLIs\n");
    u.push_str("After=network-online.target\n");
    u.push_str("StartLimitIntervalSec=0\n\n");
    u.push_str("[Service]\n");
    u.push_str(&format!("Environment=PATH={path}\n"));
    u.push_str(&format!("EnvironmentFile={env_file}\n"));
    u.push_str(SYSTEMD_EXEC_START_PRE);
    u.push('\n');
    u.push_str(&format!("ExecStart={bin}{norestore}\n"));
    u.push_str("Restart=always\n");
    u.push_str("RestartSec=2\n\n");
    u.push_str("[Install]\n");
    u.push_str("WantedBy=default.target\n");
    u
}

fn launchd_plist(
    bin: &str,
    path: &str,
    addr: &str,
    no_restore: bool,
    out_log: &str,
    err_log: &str,
) -> String {
    let mut prog_args = format!("    <string>{}</string>\n", xml_escape(bin));
    if no_restore {
        prog_args.push_str("    <string>--no-restore</string>\n");
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
{prog_args}  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>{path}</string>
    <key>CCWEB_ADDR</key>
    <string>{addr}</string>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>{out_log}</string>
  <key>StandardErrorPath</key>
  <string>{err_log}</string>
</dict>
</plist>
"#,
        label = LAUNCHD_LABEL,
        prog_args = prog_args,
        path = xml_escape(path),
        addr = xml_escape(addr),
        out_log = xml_escape(out_log),
        err_log = xml_escape(err_log),
    )
}

// ── install / uninstall drivers ────────────────────────────────────────────

pub fn install(args: &[String]) -> Result<(), String> {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        return Ok(());
    }
    let opts = parse_opts(args)?;
    let bind = opts.bind.clone().unwrap_or_else(detect_bind);
    let addr = format!("{bind}:{}", opts.port);

    let bin = std::env::current_exe().map_err(|e| format!("locating this binary: {e}"))?;
    let bin = bin.to_string_lossy().to_string();

    let config_dir = home().join(".config").join("cc-screen-rust");
    std::fs::create_dir_all(&config_dir)
        .map_err(|e| format!("mkdir {}: {e}", config_dir.display()))?;
    let env_file = config_dir.join("web.env");
    std::fs::write(&env_file, format!("CCWEB_ADDR={addr}\n"))
        .map_err(|e| format!("writing {}: {e}", env_file.display()))?;

    if cfg!(target_os = "linux") {
        install_systemd(&bin, &env_file, opts.no_restore)?;
    } else if cfg!(target_os = "macos") {
        install_launchd(&bin, &config_dir, &addr, opts.no_restore)?;
    } else {
        return Err(format!(
            "no service manager for this OS; run it yourself:\n  {bin} --addr {addr}"
        ));
    }

    println!();
    println!("cc-screen-rust is serving on http://{addr}");
    if bind.starts_with("100.") {
        println!("From a tailnet device, open  http://{addr}  and Add to Home Screen.");
    }
    Ok(())
}

fn install_systemd(bin: &str, env_file: &Path, no_restore: bool) -> Result<(), String> {
    let unit_dir = home().join(".config").join("systemd").join("user");
    std::fs::create_dir_all(&unit_dir).map_err(|e| format!("mkdir {}: {e}", unit_dir.display()))?;
    let unit = systemd_unit(bin, &env_file.to_string_lossy(), &svc_path(), no_restore);
    let unit_path = unit_dir.join(SYSTEMD_UNIT);
    std::fs::write(&unit_path, unit).map_err(|e| format!("writing {}: {e}", unit_path.display()))?;

    run("systemctl", &["--user", "daemon-reload"], false)?;
    run("systemctl", &["--user", "enable", SYSTEMD_UNIT], true)?;
    run("systemctl", &["--user", "restart", SYSTEMD_UNIT], false)?;
    if let Ok(user) = std::env::var("USER") {
        let _ = run("loginctl", &["enable-linger", &user], true);
    }
    println!("→ systemd --user service '{SYSTEMD_UNIT}' running (auto-restart, auto-resume)");
    Ok(())
}

fn install_launchd(
    bin: &str,
    config_dir: &Path,
    addr: &str,
    no_restore: bool,
) -> Result<(), String> {
    let agents = home().join("Library").join("LaunchAgents");
    std::fs::create_dir_all(&agents).map_err(|e| format!("mkdir {}: {e}", agents.display()))?;
    let plist = launchd_plist(
        bin,
        &svc_path(),
        addr,
        no_restore,
        &config_dir.join("stdout.log").to_string_lossy(),
        &config_dir.join("stderr.log").to_string_lossy(),
    );
    let plist_path = agents.join(format!("{LAUNCHD_LABEL}.plist"));
    let plist_str = plist_path.to_string_lossy().to_string();
    std::fs::write(&plist_path, plist)
        .map_err(|e| format!("writing {}: {e}", plist_path.display()))?;

    // load -w is deprecated but the most broadly compatible across macOS
    // versions; unload first so a re-install picks up the new plist.
    let _ = run("launchctl", &["unload", "-w", &plist_str], true);
    run("launchctl", &["load", "-w", &plist_str], false)?;
    println!("→ launchd LaunchAgent '{LAUNCHD_LABEL}' loaded (auto-restart, auto-resume)");
    Ok(())
}

pub fn uninstall() -> Result<(), String> {
    if cfg!(target_os = "linux") {
        let _ = run("systemctl", &["--user", "disable", "--now", SYSTEMD_UNIT], true);
        let unit_path = home()
            .join(".config")
            .join("systemd")
            .join("user")
            .join(SYSTEMD_UNIT);
        let _ = std::fs::remove_file(&unit_path);
        let _ = run("systemctl", &["--user", "daemon-reload"], true);
        println!("→ removed systemd --user service '{SYSTEMD_UNIT}'");
    } else if cfg!(target_os = "macos") {
        let plist_path = home()
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{LAUNCHD_LABEL}.plist"));
        let _ = run("launchctl", &["unload", "-w", &plist_path.to_string_lossy()], true);
        let _ = std::fs::remove_file(&plist_path);
        println!("→ removed launchd LaunchAgent '{LAUNCHD_LABEL}'");
    } else {
        return Err("no service manager for this OS".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_defaults() {
        let o = parse_opts(&[]).unwrap();
        assert_eq!(o.port, 8839);
        assert!(o.bind.is_none());
        assert!(!o.no_restore);
    }

    #[test]
    fn parse_flags_both_forms() {
        let args: Vec<String> = ["--port=9001", "--bind", "100.1.2.3", "--no-restore"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let o = parse_opts(&args).unwrap();
        assert_eq!(o.port, 9001);
        assert_eq!(o.bind.as_deref(), Some("100.1.2.3"));
        assert!(o.no_restore);

        let sep: Vec<String> = ["-p", "8000"].iter().map(|s| s.to_string()).collect();
        assert_eq!(parse_opts(&sep).unwrap().port, 8000);
    }

    #[test]
    fn parse_rejects_unknown_and_bad_port() {
        assert!(parse_opts(&["--nope".to_string()]).is_err());
        assert!(parse_opts(&["--port=abc".to_string()]).is_err());
    }

    #[test]
    fn systemd_unit_has_essentials() {
        let u = systemd_unit("/home/u/.local/bin/cc-screen-rust", "/c/web.env", "/p:/q", false);
        assert!(u.contains("ExecStart=/home/u/.local/bin/cc-screen-rust\n"));
        assert!(u.contains("EnvironmentFile=/c/web.env\n"));
        assert!(u.contains("Environment=PATH=/p:/q\n"));
        assert!(u.contains("WantedBy=default.target"));
        // ExecStartPre wait-for-IP guard is preserved.
        assert!(u.contains("ExecStartPre=/bin/sh -c"));
        // no --no-restore unless asked
        assert!(!u.contains("--no-restore"));

        let u2 = systemd_unit("/b", "/e", "/p", true);
        assert!(u2.contains("ExecStart=/b --no-restore\n"));
    }

    #[test]
    fn launchd_plist_has_essentials() {
        let p = launchd_plist("/usr/bin/cc-screen-rust", "/p:/q", "100.1.2.3:8839", false, "/o", "/e");
        assert!(p.contains("<string>com.dibbla.cc-screen-rust</string>"));
        assert!(p.contains("<string>/usr/bin/cc-screen-rust</string>"));
        assert!(p.contains("<key>CCWEB_ADDR</key>"));
        assert!(p.contains("<string>100.1.2.3:8839</string>"));
        assert!(p.contains("<key>RunAtLoad</key>"));
        assert!(!p.contains("--no-restore"));

        let p2 = launchd_plist("/b", "/p", "a:1", true, "/o", "/e");
        assert!(p2.contains("<string>--no-restore</string>"));
    }

    #[test]
    fn xml_escape_escapes() {
        assert_eq!(xml_escape("a&b<c>\"'"), "a&amp;b&lt;c&gt;&quot;&apos;");
    }
}
