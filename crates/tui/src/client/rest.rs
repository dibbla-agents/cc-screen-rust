//! Thin async REST client over the `/api/*` endpoints.

use anyhow::{Context, Result};
use cc_screen_protocol::{CreateReq, CreateResp, DeleteReq, MachineInfo, SessionInfo, ToolInfo};
use serde::Deserialize;

use super::url::ServerUrls;

/// One subdirectory entry from `GET /api/dirs` — drives the new-session dir
/// autocomplete. `path` is the absolute path (already $HOME-confined server-side).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct DirEntry {
    pub name: String,
    pub path: String,
}

#[derive(Deserialize)]
struct DirsResp {
    #[serde(default)]
    dirs: Vec<DirEntry>,
}

const AUTH_MSG: &str =
    "server requires auth — set `api_token` in ~/.config/cc-screen-tui/config.toml or pass --token";

/// Turn a 401 into a clear, actionable message instead of reqwest's terse
/// "HTTP status 401"; otherwise bubble other 4xx/5xx as usual.
fn check(r: reqwest::Response) -> Result<reqwest::Response> {
    if r.status() == reqwest::StatusCode::UNAUTHORIZED {
        anyhow::bail!(AUTH_MSG);
    }
    Ok(r.error_for_status()?)
}

#[derive(Clone)]
pub struct Rest {
    http: reqwest::Client,
    urls: ServerUrls,
    /// API token (if any), reused for the WebSocket handshake — see `token()`.
    token: Option<String>,
}

impl Rest {
    pub fn new(base: &str, insecure: bool, token: Option<String>) -> Result<Self> {
        let mut builder = reqwest::Client::builder().danger_accept_invalid_certs(insecure);
        // Bake the token into every request so a password-protected server
        // accepts us without the web login.
        if let Some(t) = &token {
            use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
            let mut headers = HeaderMap::new();
            let mut val =
                HeaderValue::from_str(&format!("Bearer {t}")).context("invalid api token")?;
            val.set_sensitive(true);
            headers.insert(AUTHORIZATION, val);
            builder = builder.default_headers(headers);
        }
        let http = builder.build().context("building HTTP client")?;
        Ok(Self { http, urls: ServerUrls::new(base), token })
    }

    pub fn urls(&self) -> &ServerUrls {
        &self.urls
    }

    /// The API token, for the WS layer to send on its handshake (the browser
    /// uses a cookie; the TUI has no cookie jar, so it carries the token).
    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    /// GET /api/sessions
    pub async fn sessions(&self) -> Result<Vec<SessionInfo>> {
        let r = check(self.http.get(self.urls.rest("/api/sessions")).send().await?)?;
        Ok(r.json().await?)
    }

    /// GET /api/session/root (no session) — the server's $HOME (the default
    /// working dir in the new-session form) and, in direct mode, this agent's own
    /// machine name (so the form can label the box without a hub). Returns
    /// `(home, machine)`; `machine` is "" on an older server that omits it.
    pub async fn root_info(&self) -> Result<(String, String)> {
        let v: serde_json::Value =
            check(self.http.get(self.urls.rest("/api/session/root")).send().await?)?
                .json()
                .await?;
        let get = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string();
        Ok((get("home"), get("machine")))
    }

    /// GET /api/tools
    pub async fn tools(&self) -> Result<Vec<ToolInfo>> {
        let r = check(self.http.get(self.urls.rest("/api/tools")).send().await?)?;
        Ok(r.json().await?)
    }

    /// GET /api/machines — the hub's connected agents. A standalone agent has no
    /// such route, so a 404 means "single, unnamed machine" → `Ok(None)`; the
    /// caller then hides the machine picker and routes creates locally.
    pub async fn machines(&self) -> Result<Option<Vec<MachineInfo>>> {
        let r = self.http.get(self.urls.rest("/api/machines")).send().await?;
        if r.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        Ok(Some(check(r)?.json().await?))
    }

    /// GET /api/dirs?path= — the subdirectories of `parent` (absolute path).
    /// Feeds the dir-field autocomplete; `machine` routes it on a hub (empty = the
    /// single/owning agent). A bad path (outside $HOME, missing) yields `[]`.
    pub async fn dirs(&self, parent: &str, machine: &str) -> Result<Vec<DirEntry>> {
        let mut req = self.http.get(self.urls.rest("/api/dirs")).query(&[("path", parent)]);
        if !machine.is_empty() {
            req = req.query(&[("machine", machine)]);
        }
        let r = req.send().await?;
        if !r.status().is_success() {
            return Ok(Vec::new());
        }
        let resp: DirsResp = r.json().await?;
        Ok(resp.dirs)
    }

    /// POST /api/sessions/restore — bring back every restorable session.
    pub async fn restore(&self) -> Result<()> {
        check(self.http.post(self.urls.rest("/api/sessions/restore")).send().await?)?;
        Ok(())
    }

    /// POST /api/session — returns the full session name. Surfaces the server's
    /// message (e.g. "already exists") on a 4xx so the form can show it. `machine`
    /// routes the create to a hub agent (empty = direct agent / single machine; a
    /// standalone agent ignores the unknown query param).
    pub async fn create(&self, req: &CreateReq, machine: &str) -> Result<String> {
        let mut post = self.http.post(self.urls.rest("/api/session"));
        if !machine.is_empty() {
            post = post.query(&[("machine", machine)]);
        }
        let r = post.json(req).send().await?;
        if r.status() == reqwest::StatusCode::UNAUTHORIZED {
            anyhow::bail!(AUTH_MSG);
        }
        if !r.status().is_success() {
            let msg = r.text().await.unwrap_or_default();
            let msg = msg.trim();
            anyhow::bail!(if msg.is_empty() { "create failed".into() } else { msg.to_string() });
        }
        let resp: CreateResp = r.json().await?;
        Ok(resp.name)
    }

    /// POST /api/session/delete — `mode` is "exit" (graceful) or "kill" (hard).
    pub async fn delete(&self, session: &str, mode: &str) -> Result<()> {
        let req = DeleteReq { session: session.to_string(), mode: mode.to_string() };
        check(self.http.post(self.urls.rest("/api/session/delete")).json(&req).send().await?)?;
        Ok(())
    }
}
