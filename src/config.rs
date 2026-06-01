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
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

fn arg_addr() -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--addr" && i + 1 < args.len() {
            return Some(args[i + 1].clone());
        }
        if let Some(v) = args[i].strip_prefix("--addr=") {
            return Some(v.to_string());
        }
        i += 1;
    }
    None
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
    let home = home_dir();
    let config_dir = home.join(".config").join("cc-screen-rust");
    let _ = std::fs::create_dir_all(&config_dir);
    let env_path = build_env_path(&home);
    let addr = arg_addr()
        .or_else(|| std::env::var("CCWEB_ADDR").ok())
        .unwrap_or_else(|| "127.0.0.1:8839".to_string());
    let tools_path = resolve_tools_path(&home, &config_dir);
    let no_restore = std::env::args().any(|a| a == "--no-restore");
    Config { home, config_dir, env_path, addr, tools_path, no_restore }
}
