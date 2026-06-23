//! `GET /install.sh` — the hub serves its own machine installer (proposal 0001
//! Phase 3 follow-up). The script is embedded at build time and the hub's own
//! public URL is templated in, so the dashboard can hand the user a single
//! copy-paste one-liner:
//!
//! ```text
//! curl -fsSL <hub>/install.sh | sh -s -- <machine-name>
//! ```
//!
//! Public + unauthenticated by design (a `curl | sh` carries no cookie): it only
//! reveals the hub URL + the public binary-installer URL, and the enrollment it
//! kicks off still requires a logged-in user to approve the code in the dashboard.

use axum::extract::State;
use axum::http::{header, HeaderMap};
use axum::response::{IntoResponse, Response};

use crate::state::HubState;

/// The machine installer, with `__CCSCREEN_HUB_URL__` / `__CCSCREEN_INSTALLER_URL__`
/// placeholders filled in per request.
const INSTALL_SCRIPT: &str = include_str!("../../../scripts/install-machine.sh");

/// The cross-platform cargo-dist binary installer (auto-detects macOS arm64/x64 +
/// Linux, installs to ~/.local/bin). Always-latest GitHub release asset; override
/// with `CCHUB_INSTALLER_URL` (e.g. to point at the Dibbla /dl mirror).
const DEFAULT_INSTALLER_URL: &str =
    "https://github.com/dibbla-agents/cc-screen-rust/releases/latest/download/cc-screen-rust-installer.sh";

pub async fn install_sh(State(_hub): State<HubState>, headers: HeaderMap) -> Response {
    // This hub's public base URL: prefer the configured CCHUB_PUBLIC_URL (the
    // canonical origin, also used for OAuth), else derive from the request so it
    // works behind any proxy without extra config.
    let hub_url = std::env::var("CCHUB_PUBLIC_URL")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let scheme = if cc_screen_auth::is_https(&headers) { "https" } else { "http" };
            let host = headers.get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("localhost");
            format!("{scheme}://{host}")
        });
    let installer_url = std::env::var("CCHUB_INSTALLER_URL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_INSTALLER_URL.to_string());

    let body = INSTALL_SCRIPT
        .replace("__CCSCREEN_HUB_URL__", &hub_url)
        .replace("__CCSCREEN_INSTALLER_URL__", &installer_url);
    ([(header::CONTENT_TYPE, "text/x-shellscript; charset=utf-8")], body).into_response()
}
