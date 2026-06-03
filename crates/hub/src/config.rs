//! Hub runtime config: where it binds, its own config dir, the client-auth
//! secrets, and the per-agent uplink tokens.

use std::collections::HashMap;
use std::path::PathBuf;

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
    let config_dir = home.join(".config").join("cc-screen-hub");
    let _ = std::fs::create_dir_all(&config_dir);
    let addr = arg_value("--addr")
        .or_else(|| std::env::var("CCWEB_ADDR").ok())
        .unwrap_or_else(|| "127.0.0.1:8840".to_string());
    let password = std::env::var("CCWEB_PASSWORD").ok();
    let api_token = std::env::var("CCWEB_API_TOKEN").ok();
    let agent_tokens = parse_agent_tokens(std::env::var("CCHUB_AGENT_TOKENS").ok().as_deref());
    HubConfig { addr, config_dir, password, api_token, agent_tokens }
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
}
