//! Persisted client config at `~/.config/cc-screen-tui/config.toml`.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Ready-session notification mode (proposal 0018). Gates the two TUI-native
/// surfaces: the foreground statusbar toast (§3) and the background terminal
/// bell + OSC 9 desktop notification (§4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NotifyMode {
    /// No ready-session notifications at all.
    Off,
    /// Foreground statusbar toast only; suppress the bell/OSC.
    Toast,
    /// Background bell + OSC 9 only; suppress the statusbar toast.
    Bell,
    /// Both surfaces (the default): toast when focused, bell + OSC 9 when not.
    All,
}

impl NotifyMode {
    /// Whether the foreground statusbar toast (§3) should show in this mode.
    pub fn wants_toast(self) -> bool {
        matches!(self, NotifyMode::Toast | NotifyMode::All)
    }
    /// Whether the background bell + OSC 9 (§4) should fire in this mode.
    pub fn wants_bell(self) -> bool {
        matches!(self, NotifyMode::Bell | NotifyMode::All)
    }
}

impl Default for NotifyMode {
    fn default() -> Self {
        NotifyMode::All
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Default server base URL.
    pub server: String,
    /// True when the on-disk config existed but didn't parse (proposal 0060 A2):
    /// `load()` warned on stderr and fell back to defaults; the next `save()`
    /// first renames the corrupt file to `config.toml.bad` so nothing
    /// user-written is ever overwritten unseen. Never serialized.
    #[serde(skip)]
    pub corrupt: bool,
    /// Prefix key for in-attach commands (tmux-style), e.g. "C-a". Used in M3.
    pub prefix: String,
    /// Recently-attached session names, most-recent first. Maintained in M4.
    pub recents: Vec<String>,
    /// API token for a password-protected server. Sent as `Authorization:
    /// Bearer <token>` on REST + the WS handshake; lets the headless client skip
    /// the web password. Overridable by `--token` or `CCS_API_TOKEN`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_token: Option<String>,
    /// Ready-session notifications (0018): `off` | `toast` | `bell` | `all`.
    /// Defaults to `all` — the statusbar toast is non-intrusive; the louder
    /// bell + OSC 9 only fire when the terminal is unfocused.
    #[serde(default)]
    pub notify: NotifyMode,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: "http://127.0.0.1:8839".into(),
            corrupt: false,
            prefix: "C-a".into(),
            recents: Vec::new(),
            api_token: None,
            notify: NotifyMode::default(),
        }
    }
}

/// Resolve the API token by precedence: `--token` > `CCS_API_TOKEN` >
/// `CCWEB_API_TOKEN` > credentials.toml (per host, proposal 0060 C3) > config
/// `api_token` (back-compat). Blank values count as unset.
pub fn resolve_token(
    cli: Option<String>,
    env_ccs: Option<String>,
    env_ccweb: Option<String>,
    credential: Option<String>,
    cfg: Option<String>,
) -> Option<String> {
    [cli, env_ccs, env_ccweb, credential, cfg]
        .into_iter()
        .flatten()
        .map(|s| s.trim().to_string())
        .find(|s| !s.is_empty())
}

pub fn config_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "cc-screen-tui")
        .map(|d| d.config_dir().join("config.toml"))
}

impl Config {
    /// Load config, falling back to defaults on any error — the client should
    /// always start. A file that exists but doesn't parse is NOT silently
    /// discarded (proposal 0060 A2): warn on stderr (we're pre-alt-screen, it
    /// will be seen) and mark the config `corrupt` so `save()` preserves the
    /// original as `config.toml.bad`.
    pub fn load() -> Self {
        let Some(path) = config_path() else {
            return Config::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(s) => match toml::from_str(&s) {
                Ok(cfg) => cfg,
                Err(e) => {
                    eprintln!(
                        "ccs: warning: couldn't parse {} ({}) — using defaults; the file will be kept as config.toml.bad",
                        path.display(),
                        e.message()
                    );
                    Config { corrupt: true, ..Config::default() }
                }
            },
            Err(_) => Config::default(),
        }
    }

    pub fn save(&mut self) -> Result<()> {
        let path = config_path().context("no config directory available")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // A2: never overwrite an unparsable user-written file unseen — move it
        // aside once, then write fresh.
        if self.corrupt && path.exists() {
            let _ = std::fs::rename(&path, path.with_extension("toml.bad"));
            self.corrupt = false;
        }
        std::fs::write(&path, toml::to_string_pretty(self)?)?;
        Ok(())
    }
}

// ── Credentials store (proposal 0060 C3) ──────────────────────────────────────
//
// Tokens live OUTSIDE config.toml (which users paste into issues and dotfile
// repos), in a sibling `credentials.toml` written 0600 in a 0700 dir — the same
// posture as the agent's enroll.json. Keyed by the server's host so several
// hubs coexist:
//
//   [servers."app.ccscreen.dev"]
//   token = "…"

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Credentials {
    #[serde(default)]
    pub servers: std::collections::BTreeMap<String, ServerCred>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerCred {
    pub token: String,
}

pub fn credentials_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "cc-screen-tui")
        .map(|d| d.config_dir().join("credentials.toml"))
}

/// The credentials key for a server URL: the authority (host[:port]),
/// lowercased, scheme/path stripped — so `https://app.ccscreen.dev/` and
/// `app.ccscreen.dev` land on the same entry.
pub fn host_key(server: &str) -> String {
    let s = server.trim();
    let s = s.split_once("://").map(|(_, rest)| rest).unwrap_or(s);
    let s = s.split(['/', '?', '#']).next().unwrap_or(s);
    s.trim_end_matches(':').to_ascii_lowercase()
}

pub fn load_credentials() -> Credentials {
    let Some(path) = credentials_path() else { return Credentials::default() };
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}

/// The stored token for `server` (by host key), if any.
pub fn credential_for(server: &str) -> Option<String> {
    load_credentials()
        .servers
        .get(&host_key(server))
        .map(|c| c.token.clone())
        .filter(|t| !t.trim().is_empty())
}

/// Store (or replace) the token for `server`. 0600 file in a 0700 dir.
pub fn store_credential(server: &str, token: &str) -> Result<()> {
    let mut creds = load_credentials();
    creds.servers.insert(host_key(server), ServerCred { token: token.to_string() });
    write_credentials(&creds)
}

/// Remove the token for `server`; true if an entry existed.
pub fn remove_credential(server: &str) -> Result<bool> {
    let mut creds = load_credentials();
    let existed = creds.servers.remove(&host_key(server)).is_some();
    if existed {
        write_credentials(&creds)?;
    }
    Ok(existed)
}

fn write_credentials(creds: &Credentials) -> Result<()> {
    let path = credentials_path().context("no config directory available")?;
    let parent = path.parent().context("no config directory available")?;
    std::fs::create_dir_all(parent)?;
    let body = toml::to_string_pretty(creds)?;
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        let tmp = path.with_extension("toml.tmp");
        {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            f.write_all(body.as_bytes())?;
            f.flush()?;
        }
        std::fs::rename(&tmp, &path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    // No POSIX modes on Windows; the profile dir is per-user, same as gh/fly.
    std::fs::write(&path, body)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_token_parses_and_is_optional() {
        let c: Config = toml::from_str(r#"server = "http://h:1"
api_token = "secret123""#)
            .unwrap();
        assert_eq!(c.api_token.as_deref(), Some("secret123"));
        // Absent → None (and the rest still defaults).
        let c: Config = toml::from_str(r#"server = "http://h:1""#).unwrap();
        assert_eq!(c.api_token, None);
        assert_eq!(c.prefix, "C-a");
    }

    #[test]
    fn notify_defaults_to_all_and_parses() {
        // Absent → All (notify on, both surfaces).
        let c: Config = toml::from_str(r#"server = "http://h:1""#).unwrap();
        assert_eq!(c.notify, NotifyMode::All);
        assert!(c.notify.wants_toast() && c.notify.wants_bell());
        // Each mode parses lowercase and gates the right surfaces.
        let parse = |v: &str| -> NotifyMode {
            toml::from_str::<Config>(&format!("notify = \"{v}\"")).unwrap().notify
        };
        assert_eq!(parse("off"), NotifyMode::Off);
        assert!(!NotifyMode::Off.wants_toast() && !NotifyMode::Off.wants_bell());
        assert_eq!(parse("toast"), NotifyMode::Toast);
        assert!(NotifyMode::Toast.wants_toast() && !NotifyMode::Toast.wants_bell());
        assert_eq!(parse("bell"), NotifyMode::Bell);
        assert!(!NotifyMode::Bell.wants_toast() && NotifyMode::Bell.wants_bell());
        assert_eq!(parse("all"), NotifyMode::All);
    }

    #[test]
    fn token_resolution_precedence() {
        // CLI wins over everything.
        assert_eq!(
            resolve_token(Some("cli".into()), Some("ccs".into()), None, Some("cred".into()), Some("cfg".into())),
            Some("cli".into())
        );
        // Then CCS_API_TOKEN, then CCWEB_API_TOKEN, then credentials, then config.
        assert_eq!(
            resolve_token(None, Some("ccs".into()), Some("ccweb".into()), Some("cred".into()), Some("cfg".into())),
            Some("ccs".into())
        );
        assert_eq!(
            resolve_token(None, None, Some("ccweb".into()), Some("cred".into()), Some("cfg".into())),
            Some("ccweb".into())
        );
        // The 0060 credentials store outranks the legacy config api_token…
        assert_eq!(
            resolve_token(None, None, None, Some("cred".into()), Some("cfg".into())),
            Some("cred".into())
        );
        // …which keeps working forever when it's all there is.
        assert_eq!(resolve_token(None, None, None, None, Some("cfg".into())), Some("cfg".into()));
        // Blank values are skipped; nothing set → None.
        assert_eq!(
            resolve_token(Some("  ".into()), None, None, None, Some("cfg".into())),
            Some("cfg".into())
        );
        assert_eq!(resolve_token(None, None, None, None, None), None);
    }

    #[test]
    fn host_key_normalizes_scheme_path_and_case() {
        assert_eq!(host_key("https://app.ccscreen.dev"), "app.ccscreen.dev");
        assert_eq!(host_key("https://App.CCscreen.dev/"), "app.ccscreen.dev");
        assert_eq!(host_key("http://hub:8840/path?q=1"), "hub:8840");
        assert_eq!(host_key("hub:8840"), "hub:8840");
        assert_eq!(host_key(" http://127.0.0.1:8839 "), "127.0.0.1:8839");
    }

    #[test]
    fn credentials_parse_by_host() {
        let c: Credentials = toml::from_str(
            r#"[servers."app.ccscreen.dev"]
token = "tok-a"
[servers."hub:8840"]
token = "tok-b""#,
        )
        .unwrap();
        assert_eq!(c.servers.get("app.ccscreen.dev").map(|s| s.token.as_str()), Some("tok-a"));
        assert_eq!(c.servers.get("hub:8840").map(|s| s.token.as_str()), Some("tok-b"));
        // Round-trips through the pretty serializer.
        let out = toml::to_string_pretty(&c).unwrap();
        let back: Credentials = toml::from_str(&out).unwrap();
        assert_eq!(back.servers.len(), 2);
    }

    #[test]
    fn corrupt_config_is_flagged_not_serialized() {
        // A parse failure yields defaults + the corrupt marker (A2)…
        let bad: Result<Config, _> = toml::from_str("server = [not toml");
        assert!(bad.is_err());
        // …and `corrupt` never lands in a serialized config.
        let c = Config { corrupt: true, ..Config::default() };
        let out = toml::to_string_pretty(&c).unwrap();
        assert!(!out.contains("corrupt"));
        // A freshly parsed config is never corrupt.
        let c2: Config = toml::from_str(r#"server = "http://h:1""#).unwrap();
        assert!(!c2.corrupt);
    }
}
