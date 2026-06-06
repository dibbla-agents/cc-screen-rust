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
    /// Comma-separated extra allowed Origin/Host values (CCWEB_ALLOWED_ORIGINS) for
    /// the browser trust boundary — a reverse-proxy domain or non-tailnet hostname.
    /// Loopback, raw IPs, and `*.ts.net` are always accepted; see auth::origin.
    pub allowed_origins: Option<String>,
    /// Loud override (CCWEB_ALLOW_UNAUTHENTICATED_REMOTE): permit a routable bind
    /// with auth disabled. Off by default — the fail-closed guard refuses it.
    pub allow_unauthenticated_remote: bool,
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
    let allowed_origins = std::env::var("CCWEB_ALLOWED_ORIGINS").ok().filter(|s| !s.trim().is_empty());
    let allow_unauthenticated_remote = env_truthy("CCWEB_ALLOW_UNAUTHENTICATED_REMOTE");
    Config {
        home,
        config_dir,
        env_path,
        addr,
        tools_path,
        no_restore,
        password,
        api_token,
        hub_url,
        hub_token,
        machine_id,
        hub_only,
        allowed_origins,
        allow_unauthenticated_remote,
    }
}
