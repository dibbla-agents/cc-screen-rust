//! `ccs activate` / `ccs logout` — the RFC-8628 device sign-in against a
//! multi-tenant hub (proposal 0060 Part C), as testable library code. The
//! binary's pre-clap arms are thin shells over [`run_activate`]/[`run_logout`];
//! everything here prints to the injected writer on the NORMAL screen (the flow
//! runs before the TUI starts, so the URL is selectable and exit codes are
//! scriptable), mirroring the agent's proven `src/enroll.rs` loop.

use std::io::Write;
use std::time::Duration;

use serde::Deserialize;

/// Why an activation didn't complete — each variant maps to a message that
/// names the next step (proposal 0060 C4) and a process exit code.
#[derive(Debug)]
pub enum ActivateError {
    /// The hub reports `multiTenant:false` — static-token territory. Exit 2.
    SingleTenant,
    /// `/api/device/client/code` 404/501ed — a pre-0060 multi-tenant hub. Exit 2.
    HubTooOld,
    /// The user denied the code on the approve page. Exit 1.
    Denied,
    /// The code expired before approval. Exit 1.
    Expired,
    /// Network / unexpected-server trouble, with the reason. Exit 1.
    Failed(String),
}

impl ActivateError {
    /// The process exit code the binary maps this to: 2 = "this hub can't do
    /// device sign-in" (wrong tenancy / too old), 1 = the attempt failed.
    pub fn exit_code(&self) -> i32 {
        match self {
            ActivateError::SingleTenant | ActivateError::HubTooOld => 2,
            _ => 1,
        }
    }

    /// The user-facing message; every dead end names the next step (C4).
    pub fn message(&self) -> String {
        match self {
            ActivateError::SingleTenant => "this hub uses a static token — set `api_token` in \
                ~/.config/cc-screen-tui/config.toml or pass --token (see the self-hosting docs)"
                .into(),
            ActivateError::HubTooOld => "this hub doesn't support device sign-in yet (needs hub ≥ 0.5) \
                — ask the operator to update, or use a static token"
                .into(),
            ActivateError::Denied => "Sign-in was denied.".into(),
            ActivateError::Expired => "Code expired. Run `ccs activate` to get a new one.".into(),
            ActivateError::Failed(e) => format!("activation failed: {e}"),
        }
    }
}

#[derive(Deserialize)]
struct MeResp {
    #[serde(default, rename = "multiTenant")]
    multi_tenant: bool,
}

#[derive(Deserialize)]
struct CodeResp {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default = "default_interval")]
    interval: u64,
    #[serde(default = "default_expires")]
    expires_in: u64,
}
fn default_interval() -> u64 {
    5
}
fn default_expires() -> u64 {
    600
}

#[derive(Deserialize)]
struct TokenOk {
    client_token: String,
    #[serde(default)]
    email: String,
}

#[derive(Deserialize)]
struct ErrResp {
    error: String,
}

/// A completed sign-in: the minted client token (persist it, never print it)
/// and the account email (DO print it — it catches wrong-account mistakes).
/// Deliberately NOT Debug — a `{:?}` must never leak the token into logs.
pub struct Activated {
    pub token: String,
    pub email: String,
}

fn http_client(insecure: bool) -> Result<reqwest::Client, ActivateError> {
    reqwest::Client::builder()
        .danger_accept_invalid_certs(insecure)
        .connect_timeout(Duration::from_secs(10))
        // An overall ceiling so a stalled server can never hang the flow (a
        // healthy poll answers instantly; only the sleeps take time).
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| ActivateError::Failed(e.to_string()))
}

/// Probe `GET /api/me` (always 200, unauthenticated): is this a multi-tenant
/// hub at all? Unreachable → `Failed` with the connection error.
pub async fn probe_multi_tenant(base: &str, insecure: bool) -> Result<bool, ActivateError> {
    let http = http_client(insecure)?;
    let me: MeResp = http
        .get(format!("{}/api/me", base.trim_end_matches('/')))
        .send()
        .await
        .map_err(|e| ActivateError::Failed(format!("couldn't reach {base}: {e}")))?
        .error_for_status()
        .map_err(|e| ActivateError::Failed(e.to_string()))?
        .json()
        .await
        .map_err(|e| ActivateError::Failed(format!("unexpected reply from {base}: {e}")))?;
    Ok(me.multi_tenant)
}

/// The whole `ccs activate` choreography (C2), minus persistence (the caller
/// stores the credential + config so this stays drivable from tests):
/// probe → request code → print code-then-URL → poll immediately, honoring
/// `interval`/`slow_down`, with a live countdown — until approved / denied /
/// expired. `open_browser` gates the best-effort auto-open (tests pass false;
/// the printed pair is the flow either way).
pub async fn run_activate(
    server: &str,
    insecure: bool,
    label: &str,
    open_browser: bool,
    out: &mut dyn Write,
) -> Result<Activated, ActivateError> {
    let base = server.trim_end_matches('/').to_string();
    if !probe_multi_tenant(&base, insecure).await? {
        return Err(ActivateError::SingleTenant);
    }

    let http = http_client(insecure)?;
    let resp = http
        .post(format!("{base}/api/device/client/code"))
        .json(&serde_json::json!({ "label": label }))
        .send()
        .await
        .map_err(|e| ActivateError::Failed(format!("couldn't reach {base}: {e}")))?;
    match resp.status().as_u16() {
        404 | 501 => return Err(ActivateError::HubTooOld),
        s if !resp.status().is_success() => {
            return Err(ActivateError::Failed(format!("{base} answered {s}")))
        }
        _ => {}
    }
    let code: CodeResp =
        resp.json().await.map_err(|e| ActivateError::Failed(format!("unexpected reply: {e}")))?;

    // The code BEFORE the URL (an auto-opened browser steals focus mid-read),
    // and polling starts immediately — never wait for an Enter. Styling is
    // TTY-gated so pipes/tests see plain text.
    let (bold_cyan, cyan, reset) = ansi();
    let _ = writeln!(out, "Signing in to {bold_cyan}{}{reset}\n", display_host(&base));
    let _ = writeln!(out, "  First, copy your one-time code:  {bold_cyan}{}{reset}\n", code.user_code);
    let _ = writeln!(out, "  Then open:  {cyan}{}{reset}", code.verification_uri);
    let _ = writeln!(out, "  (on this machine, or on your phone)\n");
    if open_browser && browser_plausible() {
        try_open_browser(&code.verification_uri);
    }

    let mut interval = code.interval.max(1);
    let deadline = std::time::Instant::now() + Duration::from_secs(code.expires_in);
    let mut first = true;
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now()).as_secs();
        let _ = write!(out, "\r  Waiting for approval… (code expires in {}:{:02})  ", left / 60, left % 60);
        let _ = out.flush();
        // Poll immediately the first time (the cli/cli#12925 lesson — never
        // wait), then honor the server's interval.
        if !first {
            tokio::time::sleep(Duration::from_secs(interval)).await;
        }
        first = false;
        let resp = http
            .post(format!("{base}/api/device/client/token"))
            .json(&serde_json::json!({ "device_code": code.device_code }))
            .send()
            .await
            .map_err(|e| ActivateError::Failed(format!("lost the hub mid-flow: {e}")))?;
        if resp.status().is_success() {
            let ok: TokenOk =
                resp.json().await.map_err(|e| ActivateError::Failed(format!("unexpected reply: {e}")))?;
            let _ = writeln!(out);
            return Ok(Activated { token: ok.client_token, email: ok.email });
        }
        let err = resp.json::<ErrResp>().await.map(|e| e.error).unwrap_or_default();
        match err.as_str() {
            "authorization_pending" => {}
            "slow_down" => interval += 5, // RFC 8628: widen the interval
            "access_denied" => {
                let _ = writeln!(out);
                return Err(ActivateError::Denied);
            }
            "expired_token" => {
                let _ = writeln!(out);
                return Err(ActivateError::Expired);
            }
            other => {
                let _ = writeln!(out);
                return Err(ActivateError::Failed(format!("device sign-in failed: {other}")));
            }
        }
    }
}

/// Persist a completed sign-in (C3): the token into the 0600 credentials store
/// keyed by host, and the server into config.toml — so the very next plain
/// `ccs` connects to the hub just signed into.
pub fn persist_login(server: &str, token: &str) -> anyhow::Result<()> {
    crate::config::store_credential(server, token)?;
    let mut cfg = crate::config::Config::load();
    cfg.server = server.trim_end_matches('/').to_string();
    cfg.save()?;
    Ok(())
}

/// What `ccs logout` did. Local removal always proceeds (offline logout must
/// work); `revoked` says whether the server confirmed killing the token.
pub struct LogoutOutcome {
    pub had_credential: bool,
    pub revoked: bool,
}

/// `ccs logout` (C2): best-effort `POST /api/client-tokens/revoke-self` with
/// the stored token, then remove the local credentials entry regardless.
pub async fn run_logout(server: &str, insecure: bool) -> anyhow::Result<LogoutOutcome> {
    let base = server.trim_end_matches('/').to_string();
    let Some(token) = crate::config::credential_for(&base) else {
        return Ok(LogoutOutcome { had_credential: false, revoked: false });
    };
    let revoked = match http_client(insecure) {
        Ok(http) => http
            .post(format!("{base}/api/client-tokens/revoke-self"))
            .bearer_auth(&token)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false),
        Err(_) => false,
    };
    crate::config::remove_credential(&base)?;
    Ok(LogoutOutcome { had_credential: true, revoked })
}

/// The host part of a base URL, for friendly "Signing in to …" copy.
fn display_host(base: &str) -> String {
    crate::config::host_key(base)
}

/// `(bold_cyan, cyan, reset)` when stdout is an interactive terminal, else
/// three empty strings — so piped output and the test writers stay plain.
pub fn ansi() -> (&'static str, &'static str, &'static str) {
    use std::io::IsTerminal;
    if std::io::stdout().is_terminal() {
        ("\x1b[1;36m", "\x1b[36m", "\x1b[0m")
    } else {
        ("", "", "")
    }
}

/// Green checkmark prefix for success lines, TTY-gated like [`ansi`].
pub fn check_mark() -> &'static str {
    use std::io::IsTerminal;
    if std::io::stdout().is_terminal() {
        "\x1b[32m✓\x1b[0m"
    } else {
        "✓"
    }
}

/// Whether attempting a local browser open is even plausible: skip over SSH
/// (`SSH_TTY`/`SSH_CONNECTION`) and on Linux with no display server — the
/// wrangler lesson. The printed URL+code is the flow; this is garnish.
fn browser_plausible() -> bool {
    if std::env::var_os("SSH_TTY").is_some() || std::env::var_os("SSH_CONNECTION").is_some() {
        return false;
    }
    if cfg!(target_os = "linux")
        && std::env::var_os("DISPLAY").is_none()
        && std::env::var_os("WAYLAND_DISPLAY").is_none()
    {
        return false;
    }
    true
}

/// Try `$BROWSER`, then the platform opener; ignore every failure silently.
fn try_open_browser(url: &str) {
    let candidates: Vec<String> = std::env::var("BROWSER")
        .ok()
        .filter(|b| !b.trim().is_empty())
        .into_iter()
        .chain(
            if cfg!(target_os = "macos") {
                vec!["open".to_string()]
            } else if cfg!(windows) {
                vec![] // `start` is a cmd builtin; not worth shelling out for
            } else {
                vec!["xdg-open".to_string()]
            },
        )
        .collect();
    for cmd in candidates {
        if std::process::Command::new(cmd)
            .arg(url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .is_ok()
        {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_follow_the_degradation_matrix() {
        // C4: wrong-tenancy / too-old are "use another path" (2); failures are 1.
        assert_eq!(ActivateError::SingleTenant.exit_code(), 2);
        assert_eq!(ActivateError::HubTooOld.exit_code(), 2);
        assert_eq!(ActivateError::Denied.exit_code(), 1);
        assert_eq!(ActivateError::Expired.exit_code(), 1);
        assert_eq!(ActivateError::Failed("x".into()).exit_code(), 1);
    }

    #[test]
    fn every_dead_end_names_the_next_step() {
        assert!(ActivateError::SingleTenant.message().contains("api_token"));
        assert!(ActivateError::HubTooOld.message().contains("static token"));
        assert!(ActivateError::Expired.message().contains("ccs activate"));
    }
}
