//! Startup bind-safety guard, shared by the agent and the hub.
//!
//! `cc-screen-rust` spawns YOLO coding CLIs and exposes their PTYs + the file
//! plane. With auth disabled (the default), a routable bind hands any network
//! peer full command and file access — equivalent to remote code execution. So we
//! refuse that combination unless an explicit, loud override says otherwise.
//! Loopback binds (local dev) are always fine. This module is axum-free and only
//! classifies the bind host string; the policy messages live with the callers.

use std::net::IpAddr;

/// How a bind host is classified for the fail-closed guard.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum BindScope {
    /// `127.0.0.0/8`, `::1`, or the literal `localhost` — reachable only from
    /// this host.
    Loopback,
    /// Anything else: a wildcard (`0.0.0.0` / `::`), a concrete LAN/tailnet/public
    /// IP, an empty host (all interfaces), or a non-localhost hostname. Reachable
    /// by other machines.
    Routable,
}

/// Extract the host portion from a `HOST:PORT` bind address, handling bracketed
/// IPv6 (`[::1]:8839`) and a bare host with no port.
pub fn host_of(addr: &str) -> &str {
    let addr = addr.trim();
    // Bracketed IPv6: `[::1]:8839` → `::1`.
    if let Some(rest) = addr.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    // `host:port` — only treat the last segment as a port when it's all digits
    // and the remainder has no colon (so a bare unbracketed IPv6 stays intact).
    match addr.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') && !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => h,
        _ => addr,
    }
}

/// Classify a bare host (no port). `localhost` and any loopback IP are
/// `Loopback`; everything else (wildcard, routable IPs, other hostnames, empty)
/// is `Routable`.
pub fn classify_host(host: &str) -> BindScope {
    let h = host.trim();
    if h.eq_ignore_ascii_case("localhost") {
        return BindScope::Loopback;
    }
    match h.parse::<IpAddr>() {
        Ok(ip) if ip.is_loopback() => BindScope::Loopback,
        _ => BindScope::Routable,
    }
}

/// Classify a full `HOST:PORT` bind address.
pub fn bind_scope(addr: &str) -> BindScope {
    classify_host(host_of(addr))
}

/// Fail-closed policy: a routable bind with auth disabled is refused unless
/// `allow_override` is set. Returns `Err(message)` to print and exit non-zero.
/// `cred_hint` names the setting(s) that enable auth; `override_env` names the
/// loud escape hatch.
pub fn require_safe_bind(
    addr: &str,
    auth_enabled: bool,
    allow_override: bool,
    cred_hint: &str,
    override_env: &str,
) -> Result<(), String> {
    if auth_enabled || allow_override {
        return Ok(());
    }
    match bind_scope(addr) {
        BindScope::Loopback => Ok(()),
        BindScope::Routable => Err(format!(
            "refusing to bind {addr} with auth disabled: a routable bind with no \
             credentials hands any network peer full control of this YOLO control \
             plane (session input, file read/write, clipboard injection).\n\
             Fix one of:\n  \
             • set {cred_hint} to require auth, or\n  \
             • bind loopback instead (e.g. 127.0.0.1:<port>), or\n  \
             • if you truly intend an unauthenticated routable bind, set \
             {override_env}=1."
        )),
    }
}

/// Hub-only policy: a routable bind with an OPEN uplink (no `CCHUB_AGENT_TOKENS`)
/// is refused unless `allow_override` is set, so any peer can't register as any
/// machine over a reachable uplink.
pub fn require_gated_uplink(
    addr: &str,
    tokens_configured: bool,
    allow_override: bool,
) -> Result<(), String> {
    if tokens_configured || allow_override {
        return Ok(());
    }
    match bind_scope(addr) {
        BindScope::Loopback => Ok(()),
        BindScope::Routable => Err(format!(
            "refusing to bind {addr} with an OPEN uplink (CCHUB_AGENT_TOKENS is \
             empty): any peer that reaches this address could register as — and \
             impersonate — any machine.\n\
             Fix one of:\n  \
             • set CCHUB_AGENT_TOKENS=machine:token,… to gate the uplink, or\n  \
             • bind loopback instead, or\n  \
             • set CCHUB_ALLOW_OPEN_UPLINK=1 to allow it anyway."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_of_handles_ports_and_brackets() {
        assert_eq!(host_of("127.0.0.1:8839"), "127.0.0.1");
        assert_eq!(host_of("0.0.0.0:8840"), "0.0.0.0");
        assert_eq!(host_of("localhost:8839"), "localhost");
        assert_eq!(host_of("100.64.1.2:8839"), "100.64.1.2");
        assert_eq!(host_of("[::1]:8839"), "::1");
        assert_eq!(host_of("[::]:8840"), "::");
        // No port.
        assert_eq!(host_of("127.0.0.1"), "127.0.0.1");
        assert_eq!(host_of("box.ts.net"), "box.ts.net");
        // Bare (unbracketed) IPv6 with no port stays intact.
        assert_eq!(host_of("::1"), "::1");
        // All-interfaces shorthand.
        assert_eq!(host_of(":8839"), "");
    }

    #[test]
    fn classify_loopback_vs_routable() {
        for lo in ["127.0.0.1", "127.0.0.53", "::1", "localhost", "LocalHost"] {
            assert_eq!(classify_host(lo), BindScope::Loopback, "{lo}");
        }
        for ro in ["0.0.0.0", "::", "100.64.1.2", "192.168.1.5", "8.8.8.8", "box.ts.net", "", "fd7a:115c:a1e0::1"] {
            assert_eq!(classify_host(ro), BindScope::Routable, "{ro}");
        }
    }

    #[test]
    fn bind_scope_parses_full_addr() {
        assert_eq!(bind_scope("127.0.0.1:8839"), BindScope::Loopback);
        assert_eq!(bind_scope("[::1]:8839"), BindScope::Loopback);
        assert_eq!(bind_scope("0.0.0.0:8839"), BindScope::Routable);
        assert_eq!(bind_scope("100.64.1.2:8840"), BindScope::Routable);
    }

    #[test]
    fn safe_bind_allows_loopback_and_auth_and_override() {
        let h = "CCWEB_PASSWORD/CCWEB_API_TOKEN";
        let o = "CCWEB_ALLOW_UNAUTHENTICATED_REMOTE";
        // Loopback, unauthenticated → fine.
        assert!(require_safe_bind("127.0.0.1:8839", false, false, h, o).is_ok());
        // Routable but authenticated → fine.
        assert!(require_safe_bind("0.0.0.0:8839", true, false, h, o).is_ok());
        // Routable, unauthenticated, override → fine.
        assert!(require_safe_bind("0.0.0.0:8839", false, true, h, o).is_ok());
        // Routable, unauthenticated, no override → refused.
        let err = require_safe_bind("0.0.0.0:8839", false, false, h, o).unwrap_err();
        assert!(err.contains("CCWEB_PASSWORD"));
        assert!(err.contains(o));
        assert!(require_safe_bind("100.64.1.2:8839", false, false, h, o).is_err());
    }

    #[test]
    fn gated_uplink_refuses_open_routable() {
        // Loopback open uplink → fine (dev).
        assert!(require_gated_uplink("127.0.0.1:8840", false, false).is_ok());
        // Routable with tokens configured → fine.
        assert!(require_gated_uplink("0.0.0.0:8840", true, false).is_ok());
        // Routable open uplink, override → fine.
        assert!(require_gated_uplink("0.0.0.0:8840", false, true).is_ok());
        // Routable open uplink, no override → refused.
        let err = require_gated_uplink("0.0.0.0:8840", false, false).unwrap_err();
        assert!(err.contains("CCHUB_AGENT_TOKENS"));
        assert!(err.contains("CCHUB_ALLOW_OPEN_UPLINK"));
    }
}
