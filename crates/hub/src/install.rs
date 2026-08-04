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

use axum::extract::{Query, State};
use axum::http::{header, HeaderMap};
use axum::response::{IntoResponse, Response};

use crate::state::HubState;

/// The POSIX machine installer (`curl <hub>/install.sh | sh`), with
/// `__CCSCREEN_HUB_URL__` / `__CCSCREEN_INSTALLER_URL__` placeholders filled in
/// per request.
const INSTALL_SCRIPT: &str = include_str!("../../../scripts/install-machine.sh");

/// The Windows machine installer (`irm <hub>/install.ps1 | iex`), the PowerShell
/// twin of `install-machine.sh` (proposal 0045 Part G). Same placeholders, plus
/// `__CCSCREEN_MACHINE_NAME__` for an optional `?name=` pre-bake.
const INSTALL_PS1: &str = include_str!("../../../scripts/install-machine.ps1");

/// The cross-platform cargo-dist binary installer (auto-detects macOS arm64/x64 +
/// Linux, installs to ~/.local/bin). Always-latest GitHub release asset; override
/// with `CCHUB_INSTALLER_URL` (e.g. to point at the Dibbla /dl mirror).
const DEFAULT_INSTALLER_URL: &str =
    "https://github.com/dibbla-agents/cc-screen-rust/releases/latest/download/cc-screen-rust-installer.sh";

/// The Windows cargo-dist PowerShell installer asset (drops cc-screen-rust.exe
/// into ~\.local\bin + user PATH). Override with `CCHUB_INSTALLER_PS1_URL`.
const DEFAULT_INSTALLER_PS1_URL: &str =
    "https://github.com/dibbla-agents/cc-screen-rust/releases/latest/download/cc-screen-rust-installer.ps1";

/// Optional `?name=<machine>` on the install routes, so the dashboard can pre-bake
/// the machine name into the served script (the PowerShell one-liner can't pass a
/// positional arg as ergonomically as `sh -s -- <name>`).
#[derive(serde::Deserialize, Default)]
pub struct InstallParams {
    name: Option<String>,
    /// Optional `?assistants=all|claude,codex` (proposal 0050): bake the
    /// coding-assistant install choice into the served script. On POSIX the
    /// dashboard prefers the visible `-s -- <name> --assistants` argument (the
    /// consent is auditable in the command the user copies); this query form
    /// exists because PowerShell's `irm … | iex` can't take positional args —
    /// the same reason `?name=` exists.
    assistants: Option<String>,
}

/// Sanitize `?assistants=` to `""` (report only), `"all"`, or a comma list of
/// tool names. Anything unexpected collapses to empty — a served script must
/// never carry a user-supplied string into a shell command line.
fn sanitize_assistants(v: &str) -> String {
    let v = v.trim();
    if v.is_empty() || v.eq_ignore_ascii_case("none") || v == "0" || v.eq_ignore_ascii_case("false") {
        return String::new();
    }
    if v.eq_ignore_ascii_case("all") || v == "1" || v.eq_ignore_ascii_case("true") {
        return "all".into();
    }
    let names: Vec<&str> = v
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .collect();
    names.join(",")
}

/// This hub's public base URL: prefer the configured CCHUB_PUBLIC_URL (the
/// canonical origin, also used for OAuth), else derive from the request so it
/// works behind any proxy without extra config.
fn hub_public_url(headers: &HeaderMap) -> String {
    std::env::var("CCHUB_PUBLIC_URL")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let scheme = if cc_screen_auth::is_https(headers) { "https" } else { "http" };
            let host = headers.get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("localhost");
            format!("{scheme}://{host}")
        })
}

/// Sanitize a `?name=` machine name to the same charset the dashboard/agent use
/// ([A-Za-z0-9._-]); anything else collapses to '-'. Empty in, empty out.
fn sanitize_machine(name: &str) -> String {
    name.trim()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '-' })
        .collect()
}

pub async fn install_sh(
    State(_hub): State<HubState>,
    Query(params): Query<InstallParams>,
    headers: HeaderMap,
) -> Response {
    let hub_url = hub_public_url(&headers);
    let installer_url = std::env::var("CCHUB_INSTALLER_URL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_INSTALLER_URL.to_string());
    let name = params.name.as_deref().map(sanitize_machine).unwrap_or_default();

    let assistants = params.assistants.as_deref().map(sanitize_assistants).unwrap_or_default();

    let body = INSTALL_SCRIPT
        .replace("__CCSCREEN_HUB_URL__", &hub_url)
        .replace("__CCSCREEN_INSTALLER_URL__", &installer_url)
        .replace("__CCSCREEN_MACHINE_NAME__", &name)
        .replace("__CCSCREEN_ASSISTANTS__", &assistants);
    ([(header::CONTENT_TYPE, "text/x-shellscript; charset=utf-8")], body).into_response()
}

/// `GET /install.ps1` — the Windows twin of `install_sh`. Same public,
/// unauthenticated rationale (it reveals only the hub + installer URLs; enrollment
/// still needs dashboard approval). PowerShell's `iex` just reads the body, so a
/// plain `text/plain` content-type is fine.
pub async fn install_ps1(
    State(_hub): State<HubState>,
    Query(params): Query<InstallParams>,
    headers: HeaderMap,
) -> Response {
    let hub_url = hub_public_url(&headers);
    let installer_url = std::env::var("CCHUB_INSTALLER_PS1_URL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_INSTALLER_PS1_URL.to_string());
    let name = params.name.as_deref().map(sanitize_machine).unwrap_or_default();

    let assistants = params.assistants.as_deref().map(sanitize_assistants).unwrap_or_default();

    let body = INSTALL_PS1
        .replace("__CCSCREEN_HUB_URL__", &hub_url)
        .replace("__CCSCREEN_INSTALLER_URL__", &installer_url)
        .replace("__CCSCREEN_MACHINE_NAME__", &name)
        .replace("__CCSCREEN_ASSISTANTS__", &assistants);
    ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], body).into_response()
}
