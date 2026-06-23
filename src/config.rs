// Runtime configuration: where state lives, what address to bind, and how to
// find the tool registry. This is the side-by-side sibling of the Go app, so it
// keeps its OWN config dir (~/.config/cc-screen-rust) and its own default port,
// but reuses the cc-screen tools.conf FORMAT (and the existing file if present)
// so the tool list never drifts from the shell / the Go build.

use std::path::PathBuf;

pub struct Config {
    pub home: PathBuf,
    pub config_dir: PathBuf,
    /// PATH for spawned sessions, guaranteed to include ~/.local/bin (the usual
    /// home for these CLIs). A systemd --user unit inherits a minimal PATH, so
    /// without this the tool would be "command not found" and the session would
    /// die instantly — exactly the Go build's main.go PATH fix.
    pub env_path: String,
    pub addr: String,
    /// Base URL a spawned session fetches its staged clipboard images from
    /// (exported as `CCWEB_CLIP_URL`; see clip.rs / the clipboard shim). Derived
    /// from the agent's *real* bind (the shim runs on the same host), NOT loopback.
    /// **Empty under `--hub-only`**: that mode binds no local socket, so there's
    /// nothing for the shim to hit — it then uses the standard Go/Mac clip chain.
    pub clip_url: String,
    /// Resolved tools.conf path, or None to fall back to the built-in defaults.
    pub tools_path: Option<PathBuf>,
    /// Skip auto-resume of recorded sessions at startup (--no-restore).
    pub no_restore: bool,
    /// Optional web-login password (CCWEB_PASSWORD). Set either this or
    /// `api_token` to turn on the opt-in auth gate; see auth.rs.
    pub password: Option<String>,
    /// Optional API token (CCWEB_API_TOKEN) the TUI / scripts present directly.
    pub api_token: Option<String>,
    /// If set, also dial this hub and register (the agent↔hub uplink). The agent
    /// still serves direct clients locally unless `hub_only`. `--hub`/CCWEB_HUB_URL.
    pub hub_url: Option<String>,
    /// The per-agent token presented on the uplink handshake (the hub authorizes
    /// the machine with it). Distinct from `api_token` (the client gate).
    /// `--token`/`--hub-token`/CCWEB_HUB_TOKEN.
    pub hub_token: Option<String>,
    /// This agent's stable identity in the hub's machine list. Defaults to the
    /// hostname; `--machine-id`/CCWEB_MACHINE_ID overrides.
    pub machine_id: String,
    /// With `--hub` set, bind NO inbound socket — reachable only through the hub
    /// (the YOLO box stops listening). `--hub-only`.
    pub hub_only: bool,
    /// Opt into RFC-8628 device enrollment against a multi-tenant hub (proposal
    /// 0001): when set and no token is otherwise available, run the device flow
    /// (print a code, wait for phone approval) and persist the minted token.
    /// `--enroll`/CCWEB_HUB_ENROLL. Off by default, so a tokenless open-uplink
    /// agent connects exactly as before; once enrolled, the persisted token
    /// auto-resumes with no flag.
    pub enroll: bool,
    /// Comma-separated extra allowed Origin/Host values (CCWEB_ALLOWED_ORIGINS) for
    /// the browser trust boundary — a reverse-proxy domain or non-tailnet hostname.
    /// Loopback, raw IPs, and `*.ts.net` are always accepted; see auth::origin.
    pub allowed_origins: Option<String>,
    /// Loud override (CCWEB_ALLOW_UNAUTHENTICATED_REMOTE): permit a routable bind
    /// with auth disabled. Off by default — the fail-closed guard refuses it.
    pub allow_unauthenticated_remote: bool,
    /// Session-summary extract size in terminal lines (CCWEB_SUMMARY_TAIL_LINES,
    /// default 200). The main cost dial — more lines = more input tokens.
    pub summary_tail_lines: usize,
    /// Session-summary candidacy tick in seconds (CCWEB_SUMMARY_INTERVAL_SECS,
    /// default 300). How often a changed session is re-summarized.
    pub summary_interval_secs: u64,
    /// Optional standalone-only Anthropic key (CCWEB_ANTHROPIC_API_KEY). The hub
    /// is the canonical keyholder; this only enables self-summarizing on a pure
    /// no-hub agent. Off unless set. See proposal 0022 §0.
    pub anthropic_api_key: Option<String>,
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

/// Read a `--flag value` or `--flag=value` CLI argument.
fn arg_value(flag: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    let eq = format!("{flag}=");
    let mut i = 0;
    while i < args.len() {
        if args[i] == flag && i + 1 < args.len() {
            return Some(args[i + 1].clone());
        }
        if let Some(v) = args[i].strip_prefix(&eq) {
            return Some(v.to_string());
        }
        i += 1;
    }
    None
}

/// Whether a bare `--flag` is present.
fn has_flag(flag: &str) -> bool {
    std::env::args().any(|a| a == flag)
}

/// Whether an env var is set to a truthy value (`1`/`true`/`yes`, any case).
fn env_truthy(key: &str) -> bool {
    std::env::var(key)
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

/// A stable identity for this machine in the hub's list: the hostname, falling
/// back to `$HOSTNAME` then `"agent"`. Override with `--machine-id`.
fn default_machine_id() -> String {
    if let Ok(s) = std::fs::read_to_string("/etc/hostname") {
        let s = s.trim();
        if !s.is_empty() {
            return s.to_string();
        }
    }
    std::env::var("HOSTNAME").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "agent".into())
}

fn build_env_path(home: &PathBuf) -> String {
    let local = home.join(".local").join("bin");
    let local = local.to_string_lossy().to_string();
    let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
    let sep = ":";
    if path.split(sep).any(|p| p == local) {
        path
    } else {
        format!("{local}{sep}{path}")
    }
}

/// The clip base URL a session fetches its staged image from — derived from the
/// agent's **actual bind address**, since the shim runs on the very same host and
/// reaches the agent there. NOT hardcoded loopback: an agent commonly binds its
/// tailnet IP (e.g. `100.x:8839`), and `127.0.0.1:8839` would then be a refused
/// socket. A wildcard bind isn't itself connectable, so map it to loopback.
fn clip_url_from_addr(addr: &str) -> String {
    let (host, port) = match addr.rsplit_once(':') {
        Some((h, p)) => (h, p),
        None => ("127.0.0.1", "8839"), // malformed (no port) → safe default
    };
    let port: u16 = port.parse().unwrap_or(8839);
    let host = match host {
        "" | "0.0.0.0" | "::" | "[::]" => "127.0.0.1",
        h => h,
    };
    format!("http://{host}:{port}")
}

fn resolve_tools_path(home: &PathBuf, config_dir: &PathBuf) -> Option<PathBuf> {
    // Mirror cc-screen.sh / the Go toolsConfigPath, but also accept our own dir.
    if let Some(p) = std::env::var_os("CCSCREEN_CONFIG") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    let cfg_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    let shared = cfg_home.join("cc-screen").join("tools.conf");
    if shared.exists() {
        return Some(shared);
    }
    let own = config_dir.join("tools.conf");
    if own.exists() {
        return Some(own);
    }
    None
}

pub fn load() -> Config {
    let real_home = home_dir();
    // Confinement / browse root. Defaults to $HOME. An isolated-state agent
    // whose $HOME is a symlink farm (e.g. the studio agent) can point this at
    // the real home with CCWEB_HOME so live session cwds — which /proc reports
    // canonically, outside the symlinked $HOME — still resolve under the guard.
    // The config/state dir stays under the *real* $HOME, so each agent keeps
    // its own session store even when several share one CCWEB_HOME.
    let home = std::env::var_os("CCWEB_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| real_home.clone());
    let config_dir = real_home.join(".config").join("cc-screen-rust");
    let _ = std::fs::create_dir_all(&config_dir);
    let env_path = build_env_path(&home);
    let addr = arg_value("--addr")
        .or_else(|| std::env::var("CCWEB_ADDR").ok())
        .unwrap_or_else(|| "127.0.0.1:8839".to_string());
    let tools_path = resolve_tools_path(&home, &config_dir);
    let no_restore = has_flag("--no-restore");
    let password = std::env::var("CCWEB_PASSWORD").ok();
    let api_token = std::env::var("CCWEB_API_TOKEN").ok();
    let hub_url = arg_value("--hub").or_else(|| std::env::var("CCWEB_HUB_URL").ok());
    // The per-agent UPLINK token (distinct from the client `CCWEB_API_TOKEN`):
    // `--hub-token` / CCWEB_HUB_TOKEN. We deliberately don't accept a bare
    // `--token` here, so it can't be confused with the client API token.
    let hub_token = arg_value("--hub-token").or_else(|| std::env::var("CCWEB_HUB_TOKEN").ok());
    let machine_id = arg_value("--machine-id")
        .or_else(|| std::env::var("CCWEB_MACHINE_ID").ok())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(default_machine_id);
    let hub_only = has_flag("--hub-only") || env_truthy("CCWEB_HUB_ONLY");
    let enroll = has_flag("--enroll") || env_truthy("CCWEB_HUB_ENROLL");
    // No local bind in hub-only mode → no clip endpoint to reach → leave it empty
    // so the shim skips straight to the standard Go/Mac clipboard chain.
    let clip_url = if hub_only { String::new() } else { clip_url_from_addr(&addr) };
    let allowed_origins = std::env::var("CCWEB_ALLOWED_ORIGINS").ok().filter(|s| !s.trim().is_empty());
    let allow_unauthenticated_remote = env_truthy("CCWEB_ALLOW_UNAUTHENTICATED_REMOTE");
    let summary_tail_lines = std::env::var("CCWEB_SUMMARY_TAIL_LINES")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(200);
    let summary_interval_secs = std::env::var("CCWEB_SUMMARY_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(300);
    let anthropic_api_key = std::env::var("CCWEB_ANTHROPIC_API_KEY").ok().filter(|s| !s.trim().is_empty());
    Config {
        home,
        config_dir,
        env_path,
        addr,
        clip_url,
        tools_path,
        no_restore,
        password,
        api_token,
        hub_url,
        hub_token,
        machine_id,
        hub_only,
        enroll,
        allowed_origins,
        allow_unauthenticated_remote,
        summary_tail_lines,
        summary_interval_secs,
        anthropic_api_key,
    }
}

#[cfg(test)]
mod tests {
    use super::clip_url_from_addr;

    #[test]
    fn clip_url_uses_the_real_bind_address() {
        // A tailnet-IP bind must be kept verbatim — the shim runs on the same host
        // and reaches the agent there; loopback would be a refused socket.
        assert_eq!(clip_url_from_addr("100.106.14.17:8839"), "http://100.106.14.17:8839");
        assert_eq!(clip_url_from_addr("127.0.0.1:8839"), "http://127.0.0.1:8839");
        assert_eq!(clip_url_from_addr("[::1]:8842"), "http://[::1]:8842");
        // A wildcard bind isn't itself connectable → loopback.
        assert_eq!(clip_url_from_addr("0.0.0.0:9001"), "http://127.0.0.1:9001");
        assert_eq!(clip_url_from_addr("[::]:8839"), "http://127.0.0.1:8839");
        // Malformed (no port) → safe loopback default, never a panic.
        assert_eq!(clip_url_from_addr("garbage"), "http://127.0.0.1:8839");
    }
}
