//! Persisted client config at `~/.config/cc-screen-tui/config.toml`.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: "http://127.0.0.1:8839".into(),
            prefix: "C-a".into(),
            recents: Vec::new(),
            api_token: None,
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
