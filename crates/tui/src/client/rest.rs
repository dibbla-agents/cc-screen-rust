//! Thin async REST client over the `/api/*` endpoints.

use anyhow::{Context, Result};
use cc_screen_protocol::{CreateReq, CreateResp, DeleteReq, SessionInfo, ToolInfo};

use super::url::ServerUrls;

#[derive(Clone)]
pub struct Rest {
    http: reqwest::Client,
    urls: ServerUrls,
}

impl Rest {
    pub fn new(base: &str, insecure: bool) -> Result<Self> {
        let http = reqwest::Client::builder()
            .danger_accept_invalid_certs(insecure)
            .build()
            .context("building HTTP client")?;
        Ok(Self { http, urls: ServerUrls::new(base) })
    }

    pub fn urls(&self) -> &ServerUrls {
        &self.urls
    }

    /// GET /api/sessions
    pub async fn sessions(&self) -> Result<Vec<SessionInfo>> {
        let r = self
            .http
            .get(self.urls.rest("/api/sessions"))
            .send()
            .await?
            .error_for_status()?;
        Ok(r.json().await?)
    }

    /// GET /api/session/root (no session) — the server's $HOME, used as the
    /// default working dir in the new-session form.
    pub async fn home(&self) -> Result<String> {
        let v: serde_json::Value = self
            .http
            .get(self.urls.rest("/api/session/root"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(v.get("home").and_then(|h| h.as_str()).unwrap_or_default().to_string())
    }

    /// GET /api/tools
    pub async fn tools(&self) -> Result<Vec<ToolInfo>> {
        let r = self.http.get(self.urls.rest("/api/tools")).send().await?.error_for_status()?;
        Ok(r.json().await?)
    }

    /// POST /api/sessions/restore — bring back every restorable session.
    pub async fn restore(&self) -> Result<()> {
        self.http
            .post(self.urls.rest("/api/sessions/restore"))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    /// POST /api/session — returns the full session name. Surfaces the server's
    /// message (e.g. "already exists") on a 4xx so the form can show it.
    pub async fn create(&self, req: &CreateReq) -> Result<String> {
        let r = self.http.post(self.urls.rest("/api/session")).json(req).send().await?;
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
        self.http
            .post(self.urls.rest("/api/session/delete"))
            .json(&req)
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }
}
