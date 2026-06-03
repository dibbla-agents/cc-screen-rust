// `cc-screen-rust install` / `uninstall` — the binary wires up its own
// long-running service, so the dist-shipped binary needs no separate install
// script (the tailscale/caddy pattern). Linux → systemd --user; macOS → launchd
// LaunchAgent. Tailnet-only by design, same as the server itself (see config.rs).
//
// The unit/plist *builders* are pure functions (testable); the install drivers
// compute the inputs (binary path, bind addr, PATH) and shell out to
// systemctl / launchctl.

use std::collections::BTreeMap;
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
    /// Web-login password (CCWEB_PASSWORD). Setting it turns on the auth gate.
    password: Option<String>,
    /// API token (CCWEB_API_TOKEN). Auto-generated when --password is set
    /// without one, so the TUI has a credential.
    token: Option<String>,
    /// Hub URL to also dial out to and register with (CCWEB_HUB_URL). Turns this
    /// machine into a "slave" reachable through the hub.
    hub: Option<String>,
    /// The per-agent UPLINK token the hub authorizes this machine with
    /// (CCWEB_HUB_TOKEN) — distinct from the client `--token`.
    hub_token: Option<String>,
    /// This machine's name in the hub's list (CCWEB_MACHINE_ID; default hostname).
    machine_id: Option<String>,
    /// With --hub, bind no inbound socket — reachable ONLY through the hub
    /// (CCWEB_HUB_ONLY).
    hub_only: bool,
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn print_help() {
    println!(
        r#"cc-screen-rust install — run this machine's agent as an auto-starting service.

An "agent" owns the AI-CLI sessions on THIS computer. Run it stand-alone (open it
directly on your tailnet) or as a "slave" that dials out to a central hub so all
your machines show up under one address.

USAGE
  cc-screen-rust install [options]      set up + (re)start the service
  cc-screen-rust update                 fetch the latest release + restart the service
  cc-screen-rust uninstall              tear the service back down
  cc-screen-rust --help                 runtime usage (flags/env when run directly)

STAND-ALONE (default)
  cc-screen-rust install                serve on the tailnet IP, port 8839
  --port N            port to serve on (default 8839)
  --bind ADDR         bind address (default: the tailnet IP, else 127.0.0.1)
  --no-restore        don't auto-resume recorded sessions at startup

SLAVE MODE (also dial out to a hub — see `cc-screen-hub install`)
  --hub URL           the hub to register with, e.g. https://hub.example:8840
  --hub-token TOK     this machine's per-agent uplink token (the hub authorizes
                      it; ask whoever runs the hub). Distinct from --token below.
  --machine-id NAME   how this box appears in the hub's list (default: hostname)
  --hub-only          bind NO local port — reachable ONLY through the hub
                      (the strictest posture for a YOLO box). Without it the agent
                      ALSO keeps serving directly on the tailnet (dual-mode).

AUTH (opt-in; protects against OTHER people on your tailnet, not the public net)
  --password PW       turn on the gate; PW unlocks the web UI (2-week cookie).
                      Auto-generates a CCWEB_API_TOKEN (printed once) if you don't
                      pass --token, for the `ccs` TUI.
  --token TOK         set the CLIENT API token explicitly (for the TUI / scripts).
                      NOTE: this is the client gate, NOT the hub uplink token.

EXAMPLES
  # one machine, just for me, on my tailnet:
  cc-screen-rust install

  # a fleet box that reports into my hub and isn't reachable on its own:
  cc-screen-rust install --hub https://hub.example:8840 \
      --hub-token $TOKEN --machine-id laptop --hub-only

All settings are written to ~/.config/cc-screen-rust/web.env (CCWEB_*) and are
editable there; re-running install preserves what you don't override. Linux uses a
systemd --user unit; macOS a launchd LaunchAgent."#
    );
}

fn parse_opts(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        port: 8839,
        bind: None,
        no_restore: false,
        password: None,
        token: None,
        hub: None,
        hub_token: None,
        machine_id: None,
        hub_only: false,
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
        } else if a == "--password" {
            i += 1;
            o.password = Some(args.get(i).ok_or("--password needs a value")?.clone());
        } else if let Some(v) = a.strip_prefix("--password=") {
            o.password = Some(v.to_string());
        } else if a == "--token" {
            i += 1;
            o.token = Some(args.get(i).ok_or("--token needs a value")?.clone());
        } else if let Some(v) = a.strip_prefix("--token=") {
            o.token = Some(v.to_string());
        } else if a == "--hub" {
            i += 1;
            o.hub = Some(args.get(i).ok_or("--hub needs a value")?.clone());
        } else if let Some(v) = a.strip_prefix("--hub=") {
            o.hub = Some(v.to_string());
        } else if a == "--hub-token" {
            i += 1;
            o.hub_token = Some(args.get(i).ok_or("--hub-token needs a value")?.clone());
        } else if let Some(v) = a.strip_prefix("--hub-token=") {
            o.hub_token = Some(v.to_string());
        } else if a == "--machine-id" {
            i += 1;
            o.machine_id = Some(args.get(i).ok_or("--machine-id needs a value")?.clone());
        } else if let Some(v) = a.strip_prefix("--machine-id=") {
            o.machine_id = Some(v.to_string());
        } else if a == "--hub-only" {
            o.hub_only = true;
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

// ── web.env (key=value) read/merge/write ────────────────────────────────────
// Re-running `install` must not clobber a password/token the user set on a
// previous run, so we merge rather than overwrite: read the existing keys, apply
// only what this invocation changes, and write the union back.

fn read_env_file(path: &Path) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    if let Ok(s) = std::fs::read_to_string(path) {
        for line in s.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                map.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
    }
    map
}

fn write_env_file(path: &Path, env: &BTreeMap<String, String>) -> Result<(), String> {
    let body: String = env.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
    std::fs::write(path, body).map_err(|e| format!("writing {}: {e}", path.display()))
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
    env: &BTreeMap<String, String>,
    no_restore: bool,
    out_log: &str,
    err_log: &str,
) -> String {
    let mut prog_args = format!("    <string>{}</string>\n", xml_escape(bin));
    if no_restore {
        prog_args.push_str("    <string>--no-restore</string>\n");
    }
    // launchd has no EnvironmentFile equivalent, so the env (CCWEB_ADDR plus any
    // CCWEB_PASSWORD / CCWEB_API_TOKEN) is inlined into the plist alongside PATH.
    let mut env_xml = format!("    <key>PATH</key>\n    <string>{}</string>\n", xml_escape(path));
    for (k, v) in env {
        env_xml.push_str(&format!(
            "    <key>{}</key>\n    <string>{}</string>\n",
            xml_escape(k),
            xml_escape(v)
        ));
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
{env_xml}  </dict>
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
        env_xml = env_xml,
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

    // Merge into any existing web.env so a plain re-install keeps prior secrets.
    let mut env = read_env_file(&env_file);
    env.insert("CCWEB_ADDR".to_string(), addr.clone());
    if let Some(pw) = &opts.password {
        env.insert("CCWEB_PASSWORD".to_string(), pw.clone());
    }
    // Token precedence: explicit --token > existing > freshly minted (only when
    // enabling a password, so the TUI has a credential). Track what to print.
    let printed_token = if let Some(tok) = &opts.token {
        env.insert("CCWEB_API_TOKEN".to_string(), tok.clone());
        Some((tok.clone(), false))
    } else if opts.password.is_some() && !env.contains_key("CCWEB_API_TOKEN") {
        let t = crate::auth::generate_token();
        env.insert("CCWEB_API_TOKEN".to_string(), t.clone());
        Some((t, true))
    } else {
        None
    };
    // Slave mode: dial out to a hub. Stored in web.env (EnvironmentFile), so the
    // service picks them up — re-install preserves them.
    if let Some(h) = &opts.hub {
        env.insert("CCWEB_HUB_URL".to_string(), h.clone());
    }
    if let Some(t) = &opts.hub_token {
        env.insert("CCWEB_HUB_TOKEN".to_string(), t.clone());
    }
    if let Some(m) = &opts.machine_id {
        env.insert("CCWEB_MACHINE_ID".to_string(), m.clone());
    }
    if opts.hub_only {
        env.insert("CCWEB_HUB_ONLY".to_string(), "1".to_string());
    }
    write_env_file(&env_file, &env)?;

    if cfg!(target_os = "linux") {
        install_systemd(&bin, &env_file, opts.no_restore)?;
    } else if cfg!(target_os = "macos") {
        install_launchd(&bin, &config_dir, &env, opts.no_restore)?;
    } else {
        return Err(format!(
            "no service manager for this OS; run it yourself:\n  {bin} --addr {addr}"
        ));
    }

    println!();
    let hub_only = env.contains_key("CCWEB_HUB_ONLY");
    if hub_only {
        println!("cc-screen-rust running in --hub-only mode (no local port; reachable via the hub)");
    } else {
        println!("cc-screen-rust is serving on http://{addr}");
    }
    if let Some(hub) = env.get("CCWEB_HUB_URL") {
        let m = env.get("CCWEB_MACHINE_ID").cloned().unwrap_or_else(|| "<hostname>".into());
        println!("🛰  Uplinking to hub {hub} as machine '{m}' — it'll appear in the hub's session list.");
        if !env.contains_key("CCWEB_HUB_TOKEN") {
            println!("   (no --hub-token set: only works if the hub runs an open uplink.)");
        }
    }
    if opts.password.is_some() || env.contains_key("CCWEB_PASSWORD") || env.contains_key("CCWEB_API_TOKEN") {
        println!("🔒 Auth is ON — the web UI asks for a password/token.");
    }
    if let Some((tok, generated)) = printed_token {
        println!();
        println!("API token{}: {tok}", if generated { " (auto-generated)" } else { "" });
        println!("  → for the `ccs` TUI, put it in ~/.config/cc-screen-tui/config.toml:");
        println!("      api_token = \"{tok}\"");
        println!("  (or pass `ccs --token {tok}` / set CCWEB_API_TOKEN). Save it now — it's secret.");
    }
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
    env: &BTreeMap<String, String>,
    no_restore: bool,
) -> Result<(), String> {
    let agents = home().join("Library").join("LaunchAgents");
    std::fs::create_dir_all(&agents).map_err(|e| format!("mkdir {}: {e}", agents.display()))?;
    let plist = launchd_plist(
        bin,
        &svc_path(),
        env,
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

// ── update (re-run the hosted installer, then restart the service) ──────────

/// Fetch + install the latest released binary (the same `curl | sh` installer the
/// docs site serves), then restart the service so it takes over. Used by
/// `cc-screen-rust update`.
pub fn update() -> Result<(), String> {
    let url = format!("{}/install-cc-screen.sh", cc_screen_protocol::RELEASE_BASE_URL);
    println!("→ downloading the latest cc-screen-rust from {url}");
    let cmd = format!("curl --proto '=https' --tlsv1.2 -LsSf {url} | sh");
    let status = Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .status()
        .map_err(|e| format!("failed to run the installer: {e}"))?;
    if !status.success() {
        return Err("installer failed (is curl available, and the site reachable?)".into());
    }
    // Restart the service so the running process picks up the new binary
    // (best-effort: a manual/foreground run just gets the new binary on disk).
    restart_service();
    println!("✓ updated. If you run it as a service it's now on the new binary; a");
    println!("  foreground run will pick it up next start.");
    Ok(())
}

/// Best-effort restart of the installed service (no-op if it isn't installed).
fn restart_service() {
    if cfg!(target_os = "linux") {
        let _ = run("systemctl", &["--user", "restart", SYSTEMD_UNIT], true);
    } else if cfg!(target_os = "macos") {
        let kick = format!("launchctl kickstart -k gui/$(id -u)/{LAUNCHD_LABEL}");
        let _ = run("sh", &["-c", &kick], true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parse_defaults() {
        let o = parse_opts(&[]).unwrap();
        assert_eq!(o.port, 8839);
        assert!(o.bind.is_none());
        assert!(!o.no_restore);
        assert!(o.password.is_none());
        assert!(o.token.is_none());
    }

    #[test]
    fn parse_password_and_token_both_forms() {
        let o = parse_opts(&s(&["--password", "pw", "--token=tok"])).unwrap();
        assert_eq!(o.password.as_deref(), Some("pw"));
        assert_eq!(o.token.as_deref(), Some("tok"));
        let o = parse_opts(&s(&["--password=p w", "--token", "t"])).unwrap();
        assert_eq!(o.password.as_deref(), Some("p w"));
        assert_eq!(o.token.as_deref(), Some("t"));
        assert!(parse_opts(&s(&["--password"])).is_err());
    }

    #[test]
    fn parse_slave_hub_flags() {
        let o = parse_opts(&s(&[
            "--hub", "https://hub:8840", "--hub-token=T", "--machine-id", "box1", "--hub-only",
        ]))
        .unwrap();
        assert_eq!(o.hub.as_deref(), Some("https://hub:8840"));
        assert_eq!(o.hub_token.as_deref(), Some("T"));
        assert_eq!(o.machine_id.as_deref(), Some("box1"));
        assert!(o.hub_only);
        // The client --token is separate from the uplink --hub-token.
        let o = parse_opts(&s(&["--token", "client", "--hub-token", "uplink"])).unwrap();
        assert_eq!(o.token.as_deref(), Some("client"));
        assert_eq!(o.hub_token.as_deref(), Some("uplink"));
    }

    #[test]
    fn env_file_merge_preserves_existing() {
        let dir = std::env::temp_dir().join(format!("ccr-env-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("web.env");
        std::fs::write(&path, "CCWEB_ADDR=old:1\nCCWEB_API_TOKEN=keepme\n").unwrap();
        let mut env = read_env_file(&path);
        // A plain re-install only updates the addr; the token survives.
        env.insert("CCWEB_ADDR".into(), "new:2".into());
        write_env_file(&path, &env).unwrap();
        let back = read_env_file(&path);
        assert_eq!(back.get("CCWEB_ADDR").map(String::as_str), Some("new:2"));
        assert_eq!(back.get("CCWEB_API_TOKEN").map(String::as_str), Some("keepme"));
        let _ = std::fs::remove_dir_all(&dir);
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
        let mut env = BTreeMap::new();
        env.insert("CCWEB_ADDR".to_string(), "100.1.2.3:8839".to_string());
        let p = launchd_plist("/usr/bin/cc-screen-rust", "/p:/q", &env, false, "/o", "/e");
        assert!(p.contains("<string>com.dibbla.cc-screen-rust</string>"));
        assert!(p.contains("<string>/usr/bin/cc-screen-rust</string>"));
        assert!(p.contains("<key>CCWEB_ADDR</key>"));
        assert!(p.contains("<string>100.1.2.3:8839</string>"));
        assert!(p.contains("<key>PATH</key>"));
        assert!(p.contains("<key>RunAtLoad</key>"));
        assert!(!p.contains("--no-restore"));

        // Password/token, when set, land in the plist env (xml-escaped).
        env.insert("CCWEB_PASSWORD".to_string(), "p&w".to_string());
        env.insert("CCWEB_API_TOKEN".to_string(), "tok123".to_string());
        let p2 = launchd_plist("/b", "/p", &env, true, "/o", "/e");
        assert!(p2.contains("<string>--no-restore</string>"));
        assert!(p2.contains("<key>CCWEB_PASSWORD</key>"));
        assert!(p2.contains("<string>p&amp;w</string>"));
        assert!(p2.contains("<key>CCWEB_API_TOKEN</key>"));
        assert!(p2.contains("<string>tok123</string>"));
    }

    #[test]
    fn xml_escape_escapes() {
        assert_eq!(xml_escape("a&b<c>\"'"), "a&amp;b&lt;c&gt;&quot;&apos;");
    }
}
