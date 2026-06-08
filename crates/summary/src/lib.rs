//! Haiku-backed session-status summarizer.
//!
//! Given the operator's recent typed submissions and a window of a session's
//! recent terminal output, produce a terse two-tier status — a ≤6-word
//! `headline` for dense surfaces and a 2–3 sentence `detail` for tooltips/pushes.
//!
//! This is the single place that talks to Anthropic. The **hub** is the canonical
//! caller (it holds `CCHUB_ANTHROPIC_API_KEY` and the spend gate); the **agent**
//! reuses the exact same code path only for the optional standalone-only
//! `CCWEB_ANTHROPIC_API_KEY` fallback. See cc-screen-saas proposal 0022.
//!
//! The Anthropic call is a raw `POST /v1/messages` (there is no official Rust
//! SDK). We force a structured `{headline, detail}` via tool-use (`tool_choice`),
//! which Haiku supports and which is more robust than free-form JSON parsing. The
//! request sets **no** streaming / thinking / effort (effort 400s on Haiku).

use serde::{Deserialize, Serialize};

/// The Anthropic Messages endpoint. Overridable in tests via [`summarize_at`].
pub const ANTHROPIC_URL: &str = "https://api.anthropic.com/v1/messages";
/// The default model — fast + cheap, 200K context.
pub const DEFAULT_MODEL: &str = "claude-haiku-4-5";
/// Output cap; the structured reply is tiny (a headline + a few sentences).
const MAX_TOKENS: u32 = 256;
/// The forced structured-output tool name.
const TOOL_NAME: &str = "record_status";

/// The two-tier summary returned to the caller (and ultimately the clients).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    /// ≤ 6 words — dense surfaces (session box, status table).
    pub headline: String,
    /// 2–3 sentences — tooltip / detail pane / push body.
    pub detail: String,
}

/// The system prompt. Stable across calls so the hub's prompt cache can hold it
/// (`cache_control: ephemeral`); only the volatile `(inputs, tail)` user turn
/// changes, so steady-state cost is dominated by cheap cached input.
pub const SYSTEM_PROMPT: &str = "You summarize the live state of an AI coding-agent terminal session for an \
operator who is NOT currently looking at it. You are given the operator's most recent typed submissions \
and a window of the session's recent terminal output. Produce a terse status by calling the record_status \
tool. \
LEAD WITH THE ACTION THE OPERATOR NEEDS TO TAKE, if any — both fields are often shown cropped, so the \
required action must come first. \
headline: at most 6 words. If the agent needs the operator to do something, make it an imperative naming \
that action (e.g. \"Approve running tests\", \"Answer its question\", \"Resolve merge conflict\"); otherwise \
name the current state (e.g. \"Working — refactoring auth\", \"Idle, task done\"). \
detail: 2-3 plain sentences. If the agent is waiting on the operator, the FIRST sentence must state exactly \
what to do (e.g. \"Approve the test run.\" / \"Tell it which DB to use.\" / \"Review the diff and confirm.\"); \
then briefly give context — what the operator asked and what the agent did. If nothing is needed from the \
operator, start with the current state instead. Be concrete and specific; never invent facts that are not \
present in the input. If the session looks idle or finished, say so.";

/// Build the JSON request body for the Messages API. Pure (no I/O) so it is
/// unit-testable; `summarize` POSTs whatever this returns.
pub fn build_request(model: &str, inputs: &[String], tail: &str) -> serde_json::Value {
    let user = user_content(inputs, tail);
    serde_json::json!({
        "model": model,
        "max_tokens": MAX_TOKENS,
        // Cacheable stable prefix (the hub reuses it across every session/call).
        "system": [{
            "type": "text",
            "text": SYSTEM_PROMPT,
            "cache_control": { "type": "ephemeral" }
        }],
        // Force structured output via a single tool the model MUST call.
        "tools": [{
            "name": TOOL_NAME,
            "description": "Record the session's current status.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "headline": { "type": "string", "description": "At most 6 words." },
                    "detail": { "type": "string", "description": "2-3 sentences." }
                },
                "required": ["headline", "detail"]
            }
        }],
        "tool_choice": { "type": "tool", "name": TOOL_NAME },
        "messages": [{ "role": "user", "content": user }],
    })
}

/// The volatile user turn: the operator's recent submissions, then the terminal
/// tail. Goes AFTER the cached system prefix.
fn user_content(inputs: &[String], tail: &str) -> String {
    let inputs_block = if inputs.is_empty() {
        "(none captured)".to_string()
    } else {
        inputs.iter().map(|s| format!("- {s}")).collect::<Vec<_>>().join("\n")
    };
    format!(
        "Operator's recent typed submissions (oldest first):\n{inputs_block}\n\n\
         Recent terminal output (most recent at the bottom):\n{tail}"
    )
}

/// Parse a Messages API response body, pulling the forced tool-use input out of
/// the `content` array. Pure so it is unit-testable against a recorded body.
pub fn parse_response(body: &serde_json::Value) -> anyhow::Result<Summary> {
    let content = body
        .get("content")
        .and_then(|c| c.as_array())
        .ok_or_else(|| anyhow::anyhow!("response has no content array"))?;
    for block in content {
        if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
            let input = block.get("input").ok_or_else(|| anyhow::anyhow!("tool_use has no input"))?;
            let headline = input.get("headline").and_then(|v| v.as_str()).unwrap_or("").trim();
            let detail = input.get("detail").and_then(|v| v.as_str()).unwrap_or("").trim();
            if headline.is_empty() && detail.is_empty() {
                anyhow::bail!("tool_use input had empty headline and detail");
            }
            return Ok(Summary { headline: headline.to_string(), detail: detail.to_string() });
        }
    }
    anyhow::bail!("no tool_use block in response")
}

/// Call Anthropic and return the summary. `client` is a shared [`reqwest::Client`]
/// (reuse one, not one-per-call). Errors (network, non-2xx, parse) bubble up so
/// the caller can log + decline rather than crash.
pub async fn summarize(
    client: &reqwest::Client,
    api_key: &str,
    model: &str,
    inputs: &[String],
    tail: &str,
) -> anyhow::Result<Summary> {
    summarize_at(client, ANTHROPIC_URL, api_key, model, inputs, tail).await
}

/// Like [`summarize`] but against an explicit URL (so tests can point at a mock
/// server).
pub async fn summarize_at(
    client: &reqwest::Client,
    url: &str,
    api_key: &str,
    model: &str,
    inputs: &[String],
    tail: &str,
) -> anyhow::Result<Summary> {
    let body = build_request(model, inputs, tail);
    let resp = client
        .post(url)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("anthropic {}: {}", status.as_u16(), text.chars().take(300).collect::<String>());
    }
    let json: serde_json::Value = serde_json::from_str(&text)?;
    parse_response(&json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_is_haiku_minimal_and_structured() {
        let req = build_request("claude-haiku-4-5", &["fix the auth bug".into()], "login() rewritten");
        // No streaming / thinking / effort — those 400 or are wrong for this call.
        assert!(req.get("stream").is_none());
        assert!(req.get("thinking").is_none());
        assert!(req.get("effort").is_none());
        // Structured output is forced via the single tool.
        assert_eq!(req["tool_choice"]["type"], "tool");
        assert_eq!(req["tool_choice"]["name"], TOOL_NAME);
        assert_eq!(req["model"], "claude-haiku-4-5");
        // The stable system prefix is marked cacheable.
        assert_eq!(req["system"][0]["cache_control"]["type"], "ephemeral");
        // The volatile inputs/tail ride the user turn, after the cached prefix.
        let user = req["messages"][0]["content"].as_str().unwrap();
        assert!(user.contains("fix the auth bug"));
        assert!(user.contains("login() rewritten"));
    }

    #[test]
    fn parse_pulls_tool_use_input() {
        let body = serde_json::json!({
            "content": [
                { "type": "text", "text": "ignored" },
                { "type": "tool_use", "name": "record_status",
                  "input": { "headline": "Waiting to run tests", "detail": "It refactored auth and is paused." } }
            ],
            "usage": { "cache_read_input_tokens": 1200 }
        });
        let s = parse_response(&body).unwrap();
        assert_eq!(s.headline, "Waiting to run tests");
        assert_eq!(s.detail, "It refactored auth and is paused.");
    }

    #[test]
    fn parse_errors_on_missing_tool_use() {
        let body = serde_json::json!({ "content": [{ "type": "text", "text": "hi" }] });
        assert!(parse_response(&body).is_err());
        // An empty payload is an error, not a blank summary.
        let empty = serde_json::json!({
            "content": [{ "type": "tool_use", "input": { "headline": "", "detail": "" } }]
        });
        assert!(parse_response(&empty).is_err());
    }

    // End-to-end through reqwest against a one-shot mock server: proves the
    // request is sent and a real Messages-shaped response parses into a Summary.
    #[tokio::test]
    async fn summarize_at_posts_and_parses_a_response() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            // Drain enough of the request that the client finishes writing.
            let mut buf = vec![0u8; 16 * 1024];
            let _ = sock.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf);
            assert!(req.contains("x-api-key: test-key"), "api key header sent");
            assert!(req.contains("anthropic-version: 2023-06-01"));
            let body = r#"{"content":[{"type":"tool_use","name":"record_status","input":{"headline":"Waiting to run tests","detail":"It refactored auth and is paused."}}]}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
            sock.flush().await.unwrap();
        });

        let client = reqwest::Client::new();
        let url = format!("http://{addr}/v1/messages");
        let s = summarize_at(&client, &url, "test-key", DEFAULT_MODEL, &["fix the bug".into()], "tail")
            .await
            .unwrap();
        assert_eq!(s.headline, "Waiting to run tests");
        assert_eq!(s.detail, "It refactored auth and is paused.");
        server.await.unwrap();
    }

    // A non-2xx response is an error (the caller maps it to a declined result),
    // never a panic or a blank summary.
    #[tokio::test]
    async fn summarize_at_surfaces_non_2xx_as_error() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let _ = sock.read(&mut buf).await.unwrap();
            let body = r#"{"error":{"type":"overloaded_error"}}"#;
            let resp = format!(
                "HTTP/1.1 429 Too Many Requests\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(resp.as_bytes()).await;
        });
        let client = reqwest::Client::new();
        let url = format!("http://{addr}/v1/messages");
        let r = summarize_at(&client, &url, "k", DEFAULT_MODEL, &[], "x").await;
        assert!(r.is_err(), "non-2xx must be an error");
    }

    #[test]
    fn empty_inputs_render_without_panicking() {
        let req = build_request(DEFAULT_MODEL, &[], "some output");
        let user = req["messages"][0]["content"].as_str().unwrap();
        assert!(user.contains("(none captured)"));
        assert!(user.contains("some output"));
    }
}
