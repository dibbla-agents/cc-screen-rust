//! Host-side RFC-8628 device enrollment (proposal 0001 §6–7): a browser-free way
//! for this agent to obtain its per-user uplink token from a multi-tenant hub.
//! The host prints a short code; the user approves it from a phone that's already
//! logged in; the agent persists the minted token and hands off to the normal
//! uplink.
//!
//! **Opt-in and backward-compatible.** This only runs when the operator passes
//! `--enroll` (or `CCWEB_HUB_ENROLL=1`) and no token is otherwise available. A
//! single-tenant/open-uplink agent with no token still connects tokenless exactly
//! as before; once enrolled, the persisted token auto-resumes on every restart
//! with no flag.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::time::sleep;

/// Everything persisted under the config dir (`enroll.json`, 0600). `device_id` is
/// written once and outlives re-enrollment; `token`/`agent_id` are filled on
/// approval. Mirrors the proposal's `HostIdentity`.
#[derive(Default, Serialize, Deserialize)]
struct HostIdentity {
    device_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    agent_id: Option<String>,
}

fn id_path(config_dir: &Path) -> PathBuf {
    config_dir.join("enroll.json")
}

/// Load the persisted identity, creating + persisting a fresh random `device_id`
/// on first sight (random UUID-ish, NOT `/etc/machine-id`, which clones into VM
/// images — proposal §6.1).
fn load_identity(config_dir: &Path) -> HostIdentity {
    let mut id: HostIdentity = std::fs::read(id_path(config_dir))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    if id.device_id.trim().is_empty() {
        id.device_id = cc_screen_auth::generate_token();
        let _ = save_identity(config_dir, &id);
    }
    id
}

fn save_identity(config_dir: &Path, id: &HostIdentity) -> std::io::Result<()> {
    let body = serde_json::to_vec_pretty(id).unwrap_or_default();
    cc_screen_auth::write_private_file(&id_path(config_dir), &body)
}

/// The persisted uplink token, if this host has already enrolled. Read on every
/// startup so an enrolled agent reconnects with no flag.
pub fn load_token(config_dir: &Path) -> Option<String> {
    load_identity(config_dir).token.filter(|t| !t.is_empty())
}

#[derive(Deserialize)]
struct CodeResp {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default = "default_interval")]
    interval: u64,
}
fn default_interval() -> u64 {
    5
}

#[derive(Deserialize)]
struct TokenOk {
    uplink_token: String,
    #[serde(default)]
    agent_id: Option<String>,
}

#[derive(Deserialize)]
struct ErrResp {
    error: String,
}

/// Whether a persisted uplink token is still accepted by the hub.
enum TokenState {
    Valid,
    /// The hub rejected it — the machine was unlinked / the token revoked.
    Revoked,
    /// Couldn't tell: an older hub without the validate route (404), or a
    /// transient network/hub error. Treated as valid so a hiccup never forces a
    /// needless re-enrollment of a working machine.
    Unknown,
}

/// Ask the hub whether `token` is still valid for `machine_id`
/// (`POST /api/device/validate`, authed by the token itself). Only an explicit
/// `401` means "revoked"; everything else is `Unknown` (proposal 0048).
async fn validate_token(http: &reqwest::Client, base: &str, machine_id: &str, token: &str) -> TokenState {
    let resp = http
        .post(format!("{base}/api/device/validate"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "machine_id": machine_id }))
        .send()
        .await;
    match resp {
        Ok(r) if r.status().is_success() => TokenState::Valid,
        Ok(r) if r.status() == reqwest::StatusCode::UNAUTHORIZED => TokenState::Revoked,
        _ => TokenState::Unknown,
    }
}

/// Run the device flow against `hub_url` and return the minted uplink token,
/// persisting it (with `agent_id`) so future restarts skip enrollment entirely.
/// Loops back to a fresh code if the user lets one expire; aborts on denial.
pub async fn ensure_token(hub_url: &str, machine_id: &str, config_dir: &Path) -> anyhow::Result<String> {
    let mut id = load_identity(config_dir);
    let base = hub_url.trim_end_matches('/').to_string();
    let http = reqwest::Client::new();

    // A persisted token means "already enrolled" — but only if the hub still
    // honors it. If this machine was unlinked from the dashboard the token is dead,
    // and reusing it just flaps the uplink forever (the hub closes every reconnect
    // with UPLINK_CLOSE_UNAUTHORIZED). Validate first; re-enroll only on an explicit
    // rejection — an older/unreachable hub keeps today's reuse-the-token behavior.
    if let Some(tok) = id.token.clone().filter(|t| !t.is_empty()) {
        match validate_token(&http, &base, machine_id, &tok).await {
            TokenState::Valid | TokenState::Unknown => return Ok(tok),
            TokenState::Revoked => {
                println!("\n  This machine was unlinked from the hub — re-enrolling.");
                id.token = None;
                id.agent_id = None;
                let _ = save_identity(config_dir, &id);
            }
        }
    }

    loop {
        // ── request a code ──────────────────────────────────────────────────
        let code: CodeResp = http
            .post(format!("{base}/api/device/code"))
            .json(&serde_json::json!({ "device_id": id.device_id, "machine_id": machine_id }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        println!(
            "\n  To connect this machine, open  {}\n  and enter code:  {}\n  (waiting…)\n",
            code.verification_uri, code.user_code
        );

        // ── poll until approved / denied / expired ──────────────────────────
        let mut interval = code.interval.max(1);
        loop {
            sleep(Duration::from_secs(interval)).await;
            let resp = http
                .post(format!("{base}/api/device/token"))
                .json(&serde_json::json!({ "device_code": code.device_code }))
                .send()
                .await?;
            if resp.status().is_success() {
                let ok: TokenOk = resp.json().await?;
                id.token = Some(ok.uplink_token.clone());
                id.agent_id = ok.agent_id;
                save_identity(config_dir, &id)?;
                println!("  Approved — connecting as '{machine_id}'.\n");
                return Ok(ok.uplink_token);
            }
            let err = resp.json::<ErrResp>().await.map(|e| e.error).unwrap_or_default();
            match err.as_str() {
                "authorization_pending" => {} // keep polling
                "slow_down" => interval += 5, // RFC 8628: widen the interval
                "expired_token" => {
                    eprintln!("  Code expired before approval — requesting a new one…");
                    break; // re-request via the outer loop
                }
                "access_denied" => anyhow::bail!("enrollment was denied"),
                other => anyhow::bail!("device enrollment failed: {other}"),
            }
        }
    }
}
