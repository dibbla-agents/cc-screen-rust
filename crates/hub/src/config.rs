//! Hub runtime config: where it binds, its own config dir, the client-auth
//! secrets, and the per-agent uplink tokens.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct HubConfig {
    pub addr: String,
    pub config_dir: PathBuf,
    /// Client-facing web-login password (CCWEB_PASSWORD); gates the PWA/`ccs`.
    pub password: Option<String>,
    /// Client-facing API token (CCWEB_API_TOKEN).
    pub api_token: Option<String>,
    /// `machine_id → uplink token`, parsed from CCHUB_AGENT_TOKENS. Empty = open
    /// mode (any agent may register — tailnet/dev only).
    pub agent_tokens: HashMap<String, String>,
    /// Comma-separated extra allowed Origin/Host values (CCWEB_ALLOWED_ORIGINS) for
    /// the browser trust boundary — the reverse-proxy domain fronting the hub.
    pub allowed_origins: Option<String>,
    /// Loud override (CCWEB_ALLOW_UNAUTHENTICATED_REMOTE): permit a routable bind
    /// with client auth disabled.
    pub allow_unauthenticated_remote: bool,
    /// Loud override (CCHUB_ALLOW_OPEN_UPLINK): permit a routable bind with an
    /// empty CCHUB_AGENT_TOKENS (open uplink).
    pub allow_open_uplink: bool,
    /// Session-summary key (CCHUB_ANTHROPIC_API_KEY) — the single keyholder
    /// (proposal 0022). `None` disables summaries fleet-wide.
    pub anthropic_api_key: Option<String>,
    /// Fleet master switch (CCHUB_SUMMARY=on|off). Defaults to **on iff a key is
    /// set** — flipping it off disables summaries without touching any agent.
    pub summary_enabled: bool,
    /// Model for summaries (CCHUB_SUMMARY_MODEL, default `claude-haiku-4-5`).
    pub summary_model: String,
    /// Optional spend cap in USD (CCHUB_SUMMARY_BUDGET) since process start; the
    /// gate of §4. `None` = uncapped.
    pub summary_budget_usd: Option<f64>,
    /// Multi-tenant database URL (CCHUB_DATABASE_URL), e.g.
    /// `sqlite:///var/lib/cc-screen-hub/hub.db`. Set ⇒ the hub runs multi-tenant
    /// (proposal 0001), but only in a `--features multi-tenant` build; a default
    /// build ignores it and stays single-tenant. `None` = single-tenant.
    pub database_url: Option<String>,
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

/// Where the hub keeps its state (`session.key`, favorites, push keys). Defaults
/// to `~/.config/cc-screen-hub`, but `CCWEB_CONFIG_DIR` overrides it so several
/// hubs (e.g. a dockerized prod on :8840 and a host-native test on :8841) can run
/// on one host with FULLY ISOLATED state instead of fighting over one dir.
fn resolve_config_dir(home: &Path, override_dir: Option<PathBuf>) -> PathBuf {
    override_dir.unwrap_or_else(|| home.join(".config").join("cc-screen-hub"))
}

/// Parse `m1:tok1,m2:tok2` into a map. Tokens are base64url (no `:`/`,`), so
/// splitting is unambiguous. Blank/!malformed entries are skipped.
fn parse_agent_tokens(spec: Option<&str>) -> HashMap<String, String> {
    let mut m = HashMap::new();
    let Some(spec) = spec else { return m };
    for pair in spec.split(',') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        if let Some((k, v)) = pair.split_once(':') {
            let (k, v) = (k.trim(), v.trim());
            if !k.is_empty() && !v.is_empty() {
                m.insert(k.to_string(), v.to_string());
            }
        }
    }
    m
}

pub fn load() -> HubConfig {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"));
    let config_dir = resolve_config_dir(&home, std::env::var_os("CCWEB_CONFIG_DIR").map(PathBuf::from));
    let _ = std::fs::create_dir_all(&config_dir);
    let addr = arg_value("--addr")
        .or_else(|| std::env::var("CCWEB_ADDR").ok())
        .unwrap_or_else(|| "127.0.0.1:8840".to_string());
    let password = std::env::var("CCWEB_PASSWORD").ok();
    let api_token = std::env::var("CCWEB_API_TOKEN").ok();
    let agent_tokens = parse_agent_tokens(std::env::var("CCHUB_AGENT_TOKENS").ok().as_deref());
    let allowed_origins = std::env::var("CCWEB_ALLOWED_ORIGINS").ok().filter(|s| !s.trim().is_empty());
    let truthy = |k: &str| {
        std::env::var(k)
            .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
            .unwrap_or(false)
    };
    let allow_unauthenticated_remote = truthy("CCWEB_ALLOW_UNAUTHENTICATED_REMOTE");
    let allow_open_uplink = truthy("CCHUB_ALLOW_OPEN_UPLINK");
    let anthropic_api_key = std::env::var("CCHUB_ANTHROPIC_API_KEY").ok().filter(|s| !s.trim().is_empty());
    // Default on iff a key is present; CCHUB_SUMMARY=off forces it off.
    let summary_enabled = match std::env::var("CCHUB_SUMMARY").ok().map(|v| v.trim().to_ascii_lowercase()) {
        Some(v) if matches!(v.as_str(), "0" | "off" | "false" | "no") => false,
        Some(v) if matches!(v.as_str(), "1" | "on" | "true" | "yes") => true,
        _ => anthropic_api_key.is_some(),
    };
    let summary_model = std::env::var("CCHUB_SUMMARY_MODEL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| cc_screen_summary::DEFAULT_MODEL.to_string());
    let summary_budget_usd = std::env::var("CCHUB_SUMMARY_BUDGET")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|&b| b > 0.0);
    let database_url = std::env::var("CCHUB_DATABASE_URL").ok().filter(|s| !s.trim().is_empty());
    HubConfig {
        addr,
        config_dir,
        password,
        api_token,
        agent_tokens,
        allowed_origins,
        allow_unauthenticated_remote,
        allow_open_uplink,
        anthropic_api_key,
        summary_enabled,
        summary_model,
        summary_budget_usd,
        database_url,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_agent_token_spec() {
        let m = parse_agent_tokens(Some("alpha:tokA, beta:tokB ,,bad,"));
        assert_eq!(m.get("alpha").map(String::as_str), Some("tokA"));
        assert_eq!(m.get("beta").map(String::as_str), Some("tokB"));
        assert_eq!(m.len(), 2, "blank and colon-less entries are skipped");
        assert!(parse_agent_tokens(None).is_empty());
        assert!(parse_agent_tokens(Some("")).is_empty());
    }

    #[test]
    fn config_dir_defaults_to_home_but_override_wins() {
        let home = PathBuf::from("/home/x");
        assert_eq!(
            resolve_config_dir(&home, None),
            PathBuf::from("/home/x/.config/cc-screen-hub"),
        );
        assert_eq!(
            resolve_config_dir(&home, Some(PathBuf::from("/tmp/hub-test"))),
            PathBuf::from("/tmp/hub-test"),
        );
    }
}
