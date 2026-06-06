//! Origin / Host validation — the browser trust boundary.
//!
//! A web page the operator opens in a normal browser cannot read our responses
//! cross-origin, but it *can* fire `fetch`/`WebSocket` requests at any address the
//! browser can route to (loopback, LAN, the tailnet IP, or a name resolved via
//! DNS rebinding). Because auth is **off by default**, `SameSite` cookies don't
//! help, so without this check such a page could open the terminal WebSocket and
//! type into a YOLO session, or call the file APIs — a browser-driven RCE.
//!
//! This runs **independent of the auth gate**. The rules:
//!
//! * No `Origin` header → not a browser cross-origin request (curl, `ccs`, a
//!   top-level navigation). Allowed here; the auth gate still applies separately.
//! * `Origin` present → its host must be same-origin (equal to the request `Host`)
//!   or on the configured allowlist, **and** the request `Host` must be one we
//!   expect. A raw-IP `Host` can't be DNS-rebound, so it's always accepted; a
//!   *hostname* `Host` must be loopback/tailnet/the bind host/allowlisted — which
//!   is what blocks the DNS-rebinding bypass (where `Origin == Host == attacker`).
//!
//! Operators fronting the server with a reverse-proxy domain (or a non-tailnet
//! hostname) add it to `CCWEB_ALLOWED_ORIGINS` (comma-separated; bare host or full
//! origin URL both work).

use std::net::IpAddr;

use http::HeaderMap;

/// The configured Origin/Host policy, built once at startup and stored in state.
#[derive(Clone, Debug, Default)]
pub struct OriginPolicy {
    /// Extra acceptable hosts (from `CCWEB_ALLOWED_ORIGINS`), normalized to bare
    /// lowercase hosts without scheme or port.
    allowed: Vec<String>,
    /// The host the server binds, lowercased, no port (`""` for a wildcard bind).
    bind_host: String,
}

impl OriginPolicy {
    /// Build from the bind address and the optional `CCWEB_ALLOWED_ORIGINS` value.
    pub fn new(bind_addr: &str, allowed_csv: Option<&str>) -> OriginPolicy {
        let bind_host = normalize_host(crate::netguard::host_of(bind_addr));
        let allowed = allowed_csv
            .map(|csv| {
                csv.split(',')
                    .map(|s| normalize_host(host_of_origin(s.trim())))
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        OriginPolicy { allowed, bind_host }
    }

    /// Whether to accept this request. See the module docs for the rules.
    pub fn check(&self, headers: &HeaderMap) -> bool {
        let host = headers
            .get(http::header::HOST)
            .and_then(|v| v.to_str().ok())
            .map(normalize_host)
            .unwrap_or_default();
        let origin = headers.get(http::header::ORIGIN).and_then(|v| v.to_str().ok());
        match origin {
            // Not a browser cross-origin request — the auth gate covers it.
            None => true,
            Some(o) => {
                // A literal `null` origin (sandboxed iframe, some file:// pages)
                // is never same-origin with us and never allowlistable by host.
                if o.eq_ignore_ascii_case("null") {
                    return false;
                }
                let oh = normalize_host(host_of_origin(o));
                let origin_allowed = (!host.is_empty() && oh == host) || self.is_allowed(&oh);
                origin_allowed && self.host_acceptable(&host)
            }
        }
    }

    fn is_allowed(&self, host: &str) -> bool {
        !host.is_empty() && self.allowed.iter().any(|a| a == host)
    }

    /// Is the request `Host` one we expect (DNS-rebinding defense)? A raw IP can't
    /// be rebound, so any IP literal is fine; a hostname must be explicitly
    /// trusted (localhost / `*.ts.net` / the bind host / the allowlist).
    fn host_acceptable(&self, host: &str) -> bool {
        if host.is_empty() {
            return false;
        }
        if host.parse::<IpAddr>().is_ok() {
            return true; // raw IP — not DNS-rebindable
        }
        host == "localhost"
            || host.ends_with(".ts.net")
            || (!self.bind_host.is_empty() && host == self.bind_host)
            || self.is_allowed(host)
    }
}

/// Lowercase a host and strip a `:port` (and IPv6 brackets). Bare `[::1]` →
/// `::1`. Whitespace-trimmed.
fn normalize_host(host: &str) -> String {
    let h = host.trim();
    // Bracketed IPv6, optionally with a port: `[::1]:8839` → `::1`.
    if let Some(rest) = h.strip_prefix('[') {
        if let Some(inner) = rest.split(']').next() {
            return inner.to_ascii_lowercase();
        }
    }
    // `host:port` — strip only a trailing all-digit port (leave bare IPv6 alone).
    let bare = match h.rsplit_once(':') {
        Some((hh, p)) if !hh.contains(':') && !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => hh,
        _ => h,
    };
    bare.to_ascii_lowercase()
}

/// Strip a `scheme://` prefix from an origin so we can compare/allowlist by host.
/// Leaves a bare host untouched; the caller normalizes the port away.
fn host_of_origin(origin: &str) -> &str {
    let o = origin.trim();
    let after = o.split_once("://").map(|(_, rest)| rest).unwrap_or(o);
    // Drop any path after the authority.
    after.split('/').next().unwrap_or(after)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hdrs(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                http::header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        h
    }

    #[test]
    fn no_origin_is_allowed() {
        let p = OriginPolicy::new("127.0.0.1:8839", None);
        assert!(p.check(&hdrs(&[("host", "127.0.0.1:8839")])));
        // Even a bare request with neither header (some test clients).
        assert!(p.check(&hdrs(&[])));
    }

    #[test]
    fn same_origin_loopback_ok() {
        let p = OriginPolicy::new("127.0.0.1:8839", None);
        assert!(p.check(&hdrs(&[
            ("host", "127.0.0.1:8839"),
            ("origin", "http://127.0.0.1:8839"),
        ])));
        assert!(p.check(&hdrs(&[("host", "localhost:8839"), ("origin", "http://localhost:8839")])));
    }

    #[test]
    fn cross_origin_rejected() {
        let p = OriginPolicy::new("100.64.1.2:8839", None);
        // A foreign page firing at the tailnet IP.
        assert!(!p.check(&hdrs(&[
            ("host", "100.64.1.2:8839"),
            ("origin", "http://evil.example.com"),
        ])));
    }

    #[test]
    fn dns_rebinding_rejected() {
        // Rebinding makes the browser think it's same-origin: Origin == Host ==
        // the attacker's *name*. The host-acceptable check (hostname not trusted)
        // is what rejects it.
        let p = OriginPolicy::new("100.64.1.2:8839", None);
        assert!(!p.check(&hdrs(&[
            ("host", "attacker.com"),
            ("origin", "http://attacker.com"),
        ])));
    }

    #[test]
    fn raw_ip_host_is_accepted_same_origin() {
        // LAN / tailnet IP access (auth on) — same-origin, raw IP, can't rebind.
        let p = OriginPolicy::new("0.0.0.0:8839", None);
        assert!(p.check(&hdrs(&[
            ("host", "192.168.1.5:8839"),
            ("origin", "http://192.168.1.5:8839"),
        ])));
        assert!(p.check(&hdrs(&[
            ("host", "100.64.1.2:8839"),
            ("origin", "http://100.64.1.2:8839"),
        ])));
    }

    #[test]
    fn tailnet_magicdns_name_ok() {
        let p = OriginPolicy::new("100.64.1.2:8839", None);
        assert!(p.check(&hdrs(&[
            ("host", "box.tail1234.ts.net"),
            ("origin", "https://box.tail1234.ts.net"),
        ])));
    }

    #[test]
    fn configured_reverse_proxy_domain_ok() {
        let p = OriginPolicy::new("127.0.0.1:8839", Some("cc-screen.example.com, https://other.example"));
        assert!(p.check(&hdrs(&[
            ("host", "cc-screen.example.com"),
            ("origin", "https://cc-screen.example.com"),
        ])));
        // An allowlisted *origin* hitting an allowlisted *host* also passes.
        assert!(p.check(&hdrs(&[
            ("host", "other.example"),
            ("origin", "https://other.example"),
        ])));
        // Not configured → still rejected.
        assert!(!p.check(&hdrs(&[
            ("host", "unconfigured.example.com"),
            ("origin", "https://unconfigured.example.com"),
        ])));
    }

    #[test]
    fn null_origin_rejected() {
        let p = OriginPolicy::new("127.0.0.1:8839", None);
        assert!(!p.check(&hdrs(&[("host", "127.0.0.1:8839"), ("origin", "null")])));
    }

    #[test]
    fn normalize_and_origin_host_helpers() {
        assert_eq!(normalize_host("Example.COM:8839"), "example.com");
        assert_eq!(normalize_host("[::1]:8839"), "::1");
        assert_eq!(normalize_host("[2001:db8::1]"), "2001:db8::1");
        assert_eq!(host_of_origin("https://host.example/path"), "host.example");
        assert_eq!(host_of_origin("http://127.0.0.1:8839"), "127.0.0.1:8839");
        assert_eq!(host_of_origin("bare.host"), "bare.host");
    }
}
