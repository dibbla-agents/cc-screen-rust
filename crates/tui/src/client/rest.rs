//! Thin async REST client over the `/api/*` endpoints.

use anyhow::{Context, Result};
use cc_screen_protocol::{CreateReq, CreateResp, DeleteReq, SessionInfo, ToolInfo};

use super::url::ServerUrls;

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

    /// GET /api/session/root (no session) — the server's $HOME, used as the
    /// default working dir in the new-session form.
    pub async fn home(&self) -> Result<String> {
        let v: serde_json::Value =
            check(self.http.get(self.urls.rest("/api/session/root")).send().await?)?
                .json()
                .await?;
        Ok(v.get("home").and_then(|h| h.as_str()).unwrap_or_default().to_string())
    }

    /// GET /api/tools
    pub async fn tools(&self) -> Result<Vec<ToolInfo>> {
        let r = check(self.http.get(self.urls.rest("/api/tools")).send().await?)?;
        Ok(r.json().await?)
    }

    /// POST /api/sessions/restore — bring back every restorable session.
    pub async fn restore(&self) -> Result<()> {
        check(self.http.post(self.urls.rest("/api/sessions/restore")).send().await?)?;
        Ok(())
    }

    /// POST /api/session — returns the full session name. Surfaces the server's
    /// message (e.g. "already exists") on a 4xx so the form can show it.
    pub async fn create(&self, req: &CreateReq) -> Result<String> {
        let r = self.http.post(self.urls.rest("/api/session")).json(req).send().await?;
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
