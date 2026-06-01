//! Derive REST and WebSocket URLs from a single base URL.

/// A server base URL (e.g. `http://127.0.0.1:8839`) with helpers to build the
/// REST paths and the terminal WebSocket URL. The `ws`/`wss` scheme is derived
/// from the base's `http`/`https`, mirroring the web client's `wsURL()`.
#[derive(Debug, Clone)]
pub struct ServerUrls {
    base: String, // normalised: no trailing slash
}

impl ServerUrls {
    pub fn new(base: &str) -> Self {
        Self { base: base.trim_end_matches('/').to_string() }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    pub fn rest(&self, path: &str) -> String {
        format!("{}/{}", self.base, path.trim_start_matches('/'))
    }

    /// `ws(s)://…/api/ws?session=<name>` for attaching to a session's terminal.
    #[allow(dead_code)] // used from M2 (WebSocket attach)
    pub fn ws(&self, session: &str) -> String {
        let ws_base = if let Some(rest) = self.base.strip_prefix("https://") {
            format!("wss://{rest}")
        } else if let Some(rest) = self.base.strip_prefix("http://") {
            format!("ws://{rest}")
        } else {
            // Already a ws(s) scheme, or a bare host — pass through.
            self.base.clone()
        };
        format!("{ws_base}/api/ws?session={}", encode_component(session))
    }
}

/// Minimal percent-encoding of the unreserved set (RFC 3986). Session names are
/// already sanitised server-side, but this keeps a stray character safe.
#[allow(dead_code)] // reachable from ServerUrls::ws (used in M2)
fn encode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rest_join_trims_slashes() {
        let u = ServerUrls::new("http://127.0.0.1:8839/");
        assert_eq!(u.rest("/api/sessions"), "http://127.0.0.1:8839/api/sessions");
        assert_eq!(u.rest("api/tools"), "http://127.0.0.1:8839/api/tools");
    }

    #[test]
    fn ws_scheme_swap() {
        assert_eq!(
            ServerUrls::new("http://127.0.0.1:8839").ws("claude-x"),
            "ws://127.0.0.1:8839/api/ws?session=claude-x"
        );
        assert_eq!(
            ServerUrls::new("https://host.ts.net").ws("codex-y"),
            "wss://host.ts.net/api/ws?session=codex-y"
        );
    }

    #[test]
    fn ws_encodes_session() {
        assert_eq!(
            ServerUrls::new("http://h").ws("a b/c"),
            "ws://h/api/ws?session=a%20b%2Fc"
        );
    }
}
