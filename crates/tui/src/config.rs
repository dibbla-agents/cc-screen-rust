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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: "http://127.0.0.1:8839".into(),
            prefix: "C-a".into(),
            recents: Vec::new(),
        }
    }
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
