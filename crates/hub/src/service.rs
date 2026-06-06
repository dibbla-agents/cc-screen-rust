// `cc-screen-hub install` / `uninstall` — the hub wires up its own long-running
// service, mirroring the agent's `service.rs`. Linux → systemd --user; macOS →
// launchd. The unit/plist builders are pure functions (tested); the drivers shell
// out to systemctl / launchctl. Default port 8840 so it coexists with an agent on
// 8839 on the same box.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

const SYSTEMD_UNIT: &str = "cc-screen-hub.service";
const LAUNCHD_LABEL: &str = "com.dibbla.cc-screen-hub";

// Wait for the bound (tailnet) IP before starting, so a late-coming Tailscale
// doesn't crash-loop the bind. Loopback/wildcard skip the wait.
const SYSTEMD_EXEC_START_PRE: &str = r#"ExecStartPre=/bin/sh -c 'a="${CCWEB_ADDR}"; ip="${a%%:*}"; case "$ip" in ""|0.0.0.0|127.0.0.1|localhost) exit 0;; esac; for i in $(seq 1 60); do ip -o addr show 2>/dev/null | grep -Fqw "$ip" && exit 0; sleep 1; done; exit 0'"#;

struct Opts {
    port: u16,
    bind: Option<String>,
    password: Option<String>,
    token: Option<String>,
    /// `machine_id:token,…` for the per-agent uplink gate (CCHUB_AGENT_TOKENS).
    agents: Option<String>,
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

fn print_help() {
    println!(
        "cc-screen-hub install — set up the auto-starting hub service\n\
         \n\
         usage:\n\
         \u{20}\u{20}cc-screen-hub install [--port N] [--bind ADDR] [--password PW] [--token TOK] [--agents SPEC]\n\
         \u{20}\u{20}cc-screen-hub update      fetch the latest release + restart the service\n\
         \u{20}\u{20}cc-screen-hub uninstall\n\
         \n\
         --port N        port to serve on (default 8840)\n\
         --bind ADDR     bind address (default: the tailnet IP, else 127.0.0.1)\n\
         --password PW   turn on the client auth gate (mints a 2-week web session)\n\
         --token TOK     client API token (for the `ccs` TUI / scripts)\n\
         --agents SPEC   per-agent uplink tokens, `machine:token,machine2:token2`\n\
         \n\
         Off-tailnet: bind the tailnet IP (default) and front the hub with a TLS\n\
         reverse proxy; require --agents tokens so only known machines can register.\n\
         All keys are editable in ~/.config/cc-screen-hub/web.env.\n\
         \n\
         Linux installs a systemd --user unit; macOS a launchd LaunchAgent."
    );
}

fn parse_opts(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts { port: 8840, bind: None, password: None, token: None, agents: None };
    let mut i = 0;
    let take = |i: &mut usize, what: &str| -> Result<String, String> {
        *i += 1;
        args.get(*i).cloned().ok_or_else(|| format!("{what} needs a value"))
    };
    while i < args.len() {
        let a = args[i].as_str();
        if a == "-p" || a == "--port" {
            o.port = take(&mut i, "--port")?.parse().map_err(|_| "invalid --port".to_string())?;
        } else if let Some(v) = a.strip_prefix("--port=") {
            o.port = v.parse().map_err(|_| format!("invalid --port: {v}"))?;
        } else if a == "-b" || a == "--bind" {
            o.bind = Some(take(&mut i, "--bind")?);
        } else if let Some(v) = a.strip_prefix("--bind=") {
            o.bind = Some(v.to_string());
        } else if a == "--password" {
            o.password = Some(take(&mut i, "--password")?);
        } else if let Some(v) = a.strip_prefix("--password=") {
            o.password = Some(v.to_string());
        } else if a == "--token" {
            o.token = Some(take(&mut i, "--token")?);
        } else if let Some(v) = a.strip_prefix("--token=") {
            o.token = Some(v.to_string());
        } else if a == "--agents" {
            o.agents = Some(take(&mut i, "--agents")?);
        } else if let Some(v) = a.strip_prefix("--agents=") {
            o.agents = Some(v.to_string());
        } else {
            return Err(format!("unknown option: {a}"));
        }
        i += 1;
    }
    Ok(o)
}

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
    let status =
        Command::new(cmd).args(args).status().map_err(|e| format!("failed to run `{cmd}`: {e}"))?;
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
    // web.env carries the client password / token + per-agent uplink tokens —
    // write it private (0600), fixing the mode on any pre-existing file.
    cc_screen_auth::write_private_file(path, body.as_bytes())
        .map_err(|e| format!("writing {}: {e}", path.display()))
}

// ── pure builders (testable) ───────────────────────────────────────────────

fn systemd_unit(bin: &str, env_file: &str, path: &str) -> String {
    let mut u = String::new();
    u.push_str("[Unit]\n");
    u.push_str("Description=cc-screen-hub — aggregator for cc-screen-rust agents\n");
    u.push_str("After=network-online.target\n");
    u.push_str("StartLimitIntervalSec=0\n\n");
    u.push_str("[Service]\n");
    u.push_str(&format!("Environment=PATH={path}\n"));
    u.push_str(&format!("EnvironmentFile={env_file}\n"));
    u.push_str(SYSTEMD_EXEC_START_PRE);
    u.push('\n');
    u.push_str(&format!("ExecStart={bin}\n"));
    u.push_str("Restart=always\n");
    u.push_str("RestartSec=2\n\n");
    u.push_str("[Install]\n");
    u.push_str("WantedBy=default.target\n");
    u
}

fn launchd_plist(bin: &str, path: &str, env: &BTreeMap<String, String>, out_log: &str, err_log: &str) -> String {
    let prog_args = format!("    <string>{}</string>\n", xml_escape(bin));
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

    let config_dir = home().join(".config").join("cc-screen-hub");
    std::fs::create_dir_all(&config_dir).map_err(|e| format!("mkdir {}: {e}", config_dir.display()))?;
    let env_file = config_dir.join("web.env");

    // Merge into any existing web.env so a re-install keeps prior secrets.
    let mut env = read_env_file(&env_file);
    env.insert("CCWEB_ADDR".to_string(), addr.clone());
    if let Some(pw) = &opts.password {
        env.insert("CCWEB_PASSWORD".to_string(), pw.clone());
    }
    if let Some(spec) = &opts.agents {
        env.insert("CCHUB_AGENT_TOKENS".to_string(), spec.clone());
    }
    let printed_token = if let Some(tok) = &opts.token {
        env.insert("CCWEB_API_TOKEN".to_string(), tok.clone());
        Some((tok.clone(), false))
    } else if opts.password.is_some() && !env.contains_key("CCWEB_API_TOKEN") {
        let t = cc_screen_auth::generate_token();
        env.insert("CCWEB_API_TOKEN".to_string(), t.clone());
        Some((t, true))
    } else {
        None
    };
    write_env_file(&env_file, &env)?;

    if cfg!(target_os = "linux") {
        install_systemd(&bin, &env_file)?;
    } else if cfg!(target_os = "macos") {
        install_launchd(&bin, &config_dir, &env)?;
    } else {
        return Err(format!("no service manager for this OS; run it yourself:\n  {bin} --addr {addr}"));
    }

    println!();
    println!("cc-screen-hub is serving on http://{addr}");
    if env.contains_key("CCWEB_PASSWORD") || env.contains_key("CCWEB_API_TOKEN") {
        println!("🔒 Client auth is ON.");
    }
    if env.contains_key("CCHUB_AGENT_TOKENS") {
        println!("🔑 Per-agent uplink tokens configured — only listed machines may register.");
    } else {
        println!("⚠ Open uplink (no --agents tokens): any agent may register. Tailnet-only.");
    }
    if let Some((tok, generated)) = printed_token {
        println!("\nClient API token{}: {tok}", if generated { " (auto-generated)" } else { "" });
        println!("  → `ccs --server http://{addr} --token {tok}`. Save it now — it's secret.");
    }
    Ok(())
}

fn install_systemd(bin: &str, env_file: &Path) -> Result<(), String> {
    let unit_dir = home().join(".config").join("systemd").join("user");
    std::fs::create_dir_all(&unit_dir).map_err(|e| format!("mkdir {}: {e}", unit_dir.display()))?;
    let unit = systemd_unit(bin, &env_file.to_string_lossy(), &svc_path());
    let unit_path = unit_dir.join(SYSTEMD_UNIT);
    std::fs::write(&unit_path, unit).map_err(|e| format!("writing {}: {e}", unit_path.display()))?;
    run("systemctl", &["--user", "daemon-reload"], false)?;
    run("systemctl", &["--user", "enable", SYSTEMD_UNIT], true)?;
    run("systemctl", &["--user", "restart", SYSTEMD_UNIT], false)?;
    if let Ok(user) = std::env::var("USER") {
        let _ = run("loginctl", &["enable-linger", &user], true);
    }
    println!("→ systemd --user service '{SYSTEMD_UNIT}' running");
    Ok(())
}

fn install_launchd(bin: &str, config_dir: &Path, env: &BTreeMap<String, String>) -> Result<(), String> {
    let agents = home().join("Library").join("LaunchAgents");
    std::fs::create_dir_all(&agents).map_err(|e| format!("mkdir {}: {e}", agents.display()))?;
    let plist = launchd_plist(
        bin,
        &svc_path(),
        env,
        &config_dir.join("stdout.log").to_string_lossy(),
        &config_dir.join("stderr.log").to_string_lossy(),
    );
    let plist_path = agents.join(format!("{LAUNCHD_LABEL}.plist"));
    let plist_str = plist_path.to_string_lossy().to_string();
    // The plist inlines secret env, so write it private (0600).
    cc_screen_auth::write_private_file(&plist_path, plist.as_bytes())
        .map_err(|e| format!("writing {}: {e}", plist_path.display()))?;
    let _ = run("launchctl", &["unload", "-w", &plist_str], true);
    run("launchctl", &["load", "-w", &plist_str], false)?;
    println!("→ launchd LaunchAgent '{LAUNCHD_LABEL}' loaded");
    Ok(())
}

pub fn uninstall() -> Result<(), String> {
    if cfg!(target_os = "linux") {
        let _ = run("systemctl", &["--user", "disable", "--now", SYSTEMD_UNIT], true);
        let unit_path = home().join(".config").join("systemd").join("user").join(SYSTEMD_UNIT);
        let _ = std::fs::remove_file(&unit_path);
        let _ = run("systemctl", &["--user", "daemon-reload"], true);
        println!("→ removed systemd --user service '{SYSTEMD_UNIT}'");
    } else if cfg!(target_os = "macos") {
        let plist_path =
            home().join("Library").join("LaunchAgents").join(format!("{LAUNCHD_LABEL}.plist"));
        let _ = run("launchctl", &["unload", "-w", &plist_path.to_string_lossy()], true);
        let _ = std::fs::remove_file(&plist_path);
        println!("→ removed launchd LaunchAgent '{LAUNCHD_LABEL}'");
    } else {
        return Err("no service manager for this OS".into());
    }
    Ok(())
}

// ── update (re-run the hosted installer, then restart the service) ──────────

/// Fetch + install the latest released hub binary (the same `curl | sh` installer
/// the docs site serves), then restart the service. Used by `cc-screen-hub update`.
pub fn update() -> Result<(), String> {
    let url = format!("{}/install-cc-screen-hub.sh", cc_screen_protocol::RELEASE_BASE_URL);
    println!("→ downloading the latest cc-screen-hub from {url}");
    let cmd = format!("curl --proto '=https' --tlsv1.2 -LsSf {url} | sh");
    let status = Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .status()
        .map_err(|e| format!("failed to run the installer: {e}"))?;
    if !status.success() {
        return Err("installer failed (is curl available, and the site reachable?)".into());
    }
    restart_service();
    println!("✓ updated. The hub service is now on the new binary.");
    Ok(())
}

/// Best-effort restart of the installed hub service (no-op if not installed).
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
    fn parse_defaults_to_8840() {
        let o = parse_opts(&[]).unwrap();
        assert_eq!(o.port, 8840);
        assert!(o.bind.is_none() && o.agents.is_none());
    }

    #[test]
    fn parse_agents_and_creds_both_forms() {
        let o = parse_opts(&s(&["--agents", "a:t1,b:t2", "--password=pw", "--token", "tok"])).unwrap();
        assert_eq!(o.agents.as_deref(), Some("a:t1,b:t2"));
        assert_eq!(o.password.as_deref(), Some("pw"));
        assert_eq!(o.token.as_deref(), Some("tok"));
        assert!(parse_opts(&s(&["--port"])).is_err());
        assert!(parse_opts(&s(&["--nope"])).is_err());
    }

    #[test]
    fn systemd_unit_has_essentials() {
        let u = systemd_unit("/home/u/.local/bin/cc-screen-hub", "/c/web.env", "/p:/q");
        assert!(u.contains("ExecStart=/home/u/.local/bin/cc-screen-hub\n"));
        assert!(u.contains("EnvironmentFile=/c/web.env\n"));
        assert!(u.contains("Environment=PATH=/p:/q\n"));
        assert!(u.contains("WantedBy=default.target"));
        assert!(u.contains("ExecStartPre=/bin/sh -c"));
    }

    #[test]
    fn launchd_plist_has_essentials() {
        let mut env = BTreeMap::new();
        env.insert("CCWEB_ADDR".to_string(), "100.1.2.3:8840".to_string());
        env.insert("CCHUB_AGENT_TOKENS".to_string(), "a:t&1".to_string());
        let p = launchd_plist("/usr/bin/cc-screen-hub", "/p:/q", &env, "/o", "/e");
        assert!(p.contains("<string>com.dibbla.cc-screen-hub</string>"));
        assert!(p.contains("<key>CCWEB_ADDR</key>"));
        assert!(p.contains("<string>100.1.2.3:8840</string>"));
        // xml-escaped value.
        assert!(p.contains("<string>a:t&amp;1</string>"));
    }
}
