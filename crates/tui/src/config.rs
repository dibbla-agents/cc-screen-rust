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
            prefix: "C-a".into(),
            recents: Vec::new(),
            api_token: None,
            notify: NotifyMode::default(),
        }
    }
}

/// Resolve the API token by precedence: `--token` > `CCS_API_TOKEN` >
/// `CCWEB_API_TOKEN` > config `api_token`. Blank values count as unset.
pub fn resolve_token(
    cli: Option<String>,
    env_ccs: Option<String>,
    env_ccweb: Option<String>,
    cfg: Option<String>,
) -> Option<String> {
    [cli, env_ccs, env_ccweb, cfg]
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
    /// Load config, falling back to defaults on any error (missing file, parse
    /// failure) — the client should always start.
    pub fn load() -> Self {
        let Some(path) = config_path() else {
            return Config::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(s) => toml::from_str(&s).unwrap_or_default(),
            Err(_) => Config::default(),
        }
    }

    #[allow(dead_code)] // used from M4 (recents persistence)
    pub fn save(&self) -> Result<()> {
        let path = config_path().context("no config directory available")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, toml::to_string_pretty(self)?)?;
        Ok(())
    }
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
            resolve_token(Some("cli".into()), Some("ccs".into()), None, Some("cfg".into())),
            Some("cli".into())
        );
        // Then CCS_API_TOKEN, then CCWEB_API_TOKEN, then config.
        assert_eq!(
            resolve_token(None, Some("ccs".into()), Some("ccweb".into()), Some("cfg".into())),
            Some("ccs".into())
        );
        assert_eq!(
            resolve_token(None, None, Some("ccweb".into()), Some("cfg".into())),
            Some("ccweb".into())
        );
        assert_eq!(resolve_token(None, None, None, Some("cfg".into())), Some("cfg".into()));
        // Blank values are skipped; nothing set → None.
        assert_eq!(resolve_token(Some("  ".into()), None, None, Some("cfg".into())), Some("cfg".into()));
        assert_eq!(resolve_token(None, None, None, None), None);
    }
}
