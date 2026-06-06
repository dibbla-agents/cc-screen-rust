//! Browser security headers for the embedded PWA responses (the app shell). The
//! agent and hub both serve `frontend/dist` and attach these to every static
//! response. Kept here (the shared, axum-free crate) so the two stay identical.

use http::header::{HeaderName, HeaderValue};

/// Default Content-Security-Policy for the embedded React PWA. Tuned for what the
/// app actually does:
///   * `script-src 'self'` — Vite emits external module scripts (no inline JS);
///   * `style-src 'unsafe-inline'` — libraries set element style attributes;
///   * `img-src`/`media-src` `data: blob:` — downloads, clipboard images;
///   * `connect-src ws: wss:` — the terminal + fs-watch WebSockets;
///   * `worker-src blob:` — the service worker + the pdf.js worker;
///   * `frame-ancestors 'none'` — the app is never framed.
/// Override or disable per deployment with `CCWEB_CSP` (see [`resolve_csp`]).
pub const DEFAULT_CSP: &str = "default-src 'self'; \
script-src 'self'; \
style-src 'self' 'unsafe-inline'; \
img-src 'self' data: blob:; \
font-src 'self' data:; \
connect-src 'self' ws: wss:; \
worker-src 'self' blob:; \
manifest-src 'self'; \
media-src 'self' blob:; \
object-src 'none'; \
base-uri 'self'; \
frame-ancestors 'none'";

/// Resolve the CSP to send: `CCWEB_CSP` overrides the default; `off` (or empty)
/// disables it (escape hatch if a deployment's PWA needs a different policy and
/// can't rebuild). Unset → [`DEFAULT_CSP`].
pub fn resolve_csp() -> Option<String> {
    match std::env::var("CCWEB_CSP") {
        Ok(v) if v.trim().is_empty() || v.trim().eq_ignore_ascii_case("off") => None,
        Ok(v) => Some(v),
        Err(_) => Some(DEFAULT_CSP.to_string()),
    }
}

/// The security headers to attach to embedded-app responses. `csp` is the policy
/// (from [`resolve_csp`]); `None` omits the CSP header but keeps the rest.
pub fn app_security_headers(csp: Option<&str>) -> Vec<(HeaderName, HeaderValue)> {
    let mut h = vec![
        (http::header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff")),
        (HeaderName::from_static("referrer-policy"), HeaderValue::from_static("no-referrer")),
        (HeaderName::from_static("x-frame-options"), HeaderValue::from_static("DENY")),
        (
            HeaderName::from_static("permissions-policy"),
            HeaderValue::from_static("camera=(), microphone=(), geolocation=(), interest-cohort=()"),
        ),
    ];
    if let Some(policy) = csp {
        if let Ok(v) = HeaderValue::from_str(policy) {
            h.push((HeaderName::from_static("content-security-policy"), v));
        }
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_csp_is_valid_header_value() {
        assert!(HeaderValue::from_str(DEFAULT_CSP).is_ok());
        assert!(DEFAULT_CSP.contains("frame-ancestors 'none'"));
        assert!(DEFAULT_CSP.contains("connect-src 'self' ws: wss:"));
    }

    #[test]
    fn headers_present_with_and_without_csp() {
        let with = app_security_headers(Some(DEFAULT_CSP));
        assert!(with.iter().any(|(n, _)| n == "content-security-policy"));
        assert!(with.iter().any(|(n, _)| n == "x-content-type-options"));
        let without = app_security_headers(None);
        assert!(!without.iter().any(|(n, _)| n == "content-security-policy"));
        assert!(without.iter().any(|(n, _)| n == "x-frame-options"));
    }
}
