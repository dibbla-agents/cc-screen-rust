// Opt-in auth, shared by the cc-screen-rust agent and the cc-screen-hub. There is
// no user database and no login form on the wire beyond a single shared secret:
// the threat model is "another person on my Tailscale network" (and, for the hub,
// "someone who reached the hub's address"), not arbitrary public internet without
// other hardening (see AGENTS.md — the agents run YOLO). Two credentials, each
// optional:
//
//   * a PASSWORD — typed into the web login, which mints a 2-week session cookie;
//   * an API TOKEN — a long random string the TUI (and scripts, and the web
//     login) present directly, so headless clients never need the password.
//
// Auth is enabled iff at least one is configured. With neither set the server
// behaves exactly as before — no gate — so existing installs don't break until
// the user opts in.
//
// The web client rides a **signed session cookie**: same-origin fetches, the
// terminal/watch WebSocket handshakes, and `<img>`/`<a>` downloads all carry it
// automatically, so the only client-side work is a login screen. Headless clients
// send `Authorization: Bearer <token>`. The cookie is stateless (HMAC-SHA256 over
// its own expiry, signed with a persisted random key), so a 2-week session
// survives server restarts/redeploys with no session store.
//
// This crate is intentionally axum-free: it works on `http::HeaderMap` and raw
// query strings. The axum `require_auth` middleware lives in each binary (it's
// coupled to that binary's app state).

pub mod netguard;
pub mod origin;
pub mod throttle;

pub use netguard::{bind_scope, require_gated_uplink, require_safe_bind, BindScope};
pub use origin::OriginPolicy;
pub use throttle::{source_key, LoginThrottle};

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use http::{header, HeaderMap};
use rand_core::{OsRng, RngCore};
use sha2::Sha256;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

const COOKIE_NAME: &str = "ccs_session";
/// 2-week session. Both the cookie's `Max-Age` and the signed expiry baked into
/// its value.
const SESSION_TTL: u64 = 14 * 24 * 60 * 60;

/// Seconds since the Unix epoch. Local to this crate so it doesn't depend on the
/// agent's engine (the hub uses it too).
fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[derive(Clone)]
pub struct Auth {
    password: Option<String>,
    token: Option<String>,
    /// HMAC key for signing session cookies. Random, persisted, never sent.
    secret: [u8; 32],
}

impl Auth {
    pub fn new(password: Option<String>, token: Option<String>, secret: [u8; 32]) -> Auth {
        // Treat blank/whitespace env values as "unset" so `CCWEB_PASSWORD=` in
        // web.env doesn't accidentally enable a guessable empty password.
        let norm = |s: Option<String>| s.map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        Auth { password: norm(password), token: norm(token), secret }
    }

    /// Build from the resolved config: load (or first-time create) the cookie
    /// signing key under the config dir, then fold in the two optional secrets.
    pub fn load(config_dir: &Path, password: Option<String>, token: Option<String>) -> Auth {
        let secret = load_or_create_secret(config_dir);
        Auth::new(password, token, secret)
    }

    /// True when a credential is configured — i.e. the gate is on. With this
    /// false the middleware lets everything through.
    pub fn enabled(&self) -> bool {
        self.password.is_some() || self.token.is_some()
    }

    /// True when a *password* is set but short enough to be a weak online-guessing
    /// surface (the binaries log a startup warning). The random API token isn't
    /// guessable, so it doesn't count.
    pub fn weak_password(&self) -> bool {
        self.password.as_deref().is_some_and(|p| p.chars().count() < 12)
    }

    /// Is this request authenticated? Either it presents the API token (bearer
    /// header / `X-Api-Token`) or a valid, unexpired session cookie. Shared by the
    /// middleware and `GET /api/auth`.
    ///
    /// NB: long-lived tokens are deliberately **not** accepted via `?token=` — a
    /// credential in a URL leaks through browser history, proxy logs, referrers,
    /// and screenshots. Headless clients send `Authorization: Bearer <token>`; the
    /// browser rides the cookie. `query` is retained in the signature only because
    /// `GET /api/auth` reports status from it; it is no longer a credential source.
    pub fn is_authed(&self, headers: &HeaderMap, _query: Option<&str>) -> bool {
        // 1) API token via header.
        if let Some(bearer) = bearer_token(headers) {
            if self.token_matches(bearer) {
                return true;
            }
        }
        if let Some(x) = headers.get("x-api-token").and_then(|v| v.to_str().ok()) {
            if self.token_matches(x.trim()) {
                return true;
            }
        }
        // 2) Signed session cookie (how the browser authenticates).
        if let Some(c) = cookie_value(headers, COOKIE_NAME) {
            if self.validate_cookie(c) {
                return true;
            }
        }
        false
    }

    /// Constant-time check of a login attempt against the password *or* token —
    /// the web login field accepts either.
    pub fn verify_login(&self, secret: &str) -> bool {
        self.password.as_deref().is_some_and(|p| ct_eq(secret, p))
            || self.token_matches(secret)
    }

    fn token_matches(&self, candidate: &str) -> bool {
        self.token.as_deref().is_some_and(|t| ct_eq(candidate, t))
    }

    // ── session cookie ────────────────────────────────────────────────────────
    fn sign(&self, msg: &[u8]) -> Vec<u8> {
        let mut mac = HmacSha256::new_from_slice(&self.secret).expect("HMAC key is 32 bytes");
        mac.update(msg);
        mac.finalize().into_bytes().to_vec()
    }

    /// The `Set-Cookie` value for a fresh 2-week session. `secure` adds the
    /// `Secure` attribute (only when we know the browser is on https, e.g.
    /// behind `tailscale serve` / a TLS proxy — see `is_https`); a plain tailnet
    /// http origin must omit it or the browser drops the cookie.
    pub fn issue_cookie(&self, secure: bool) -> String {
        let exp = now_secs() + SESSION_TTL;
        let exp_str = exp.to_string();
        let sig = URL_SAFE_NO_PAD.encode(self.sign(exp_str.as_bytes()));
        let mut c = format!(
            "{COOKIE_NAME}={exp_str}.{sig}; Max-Age={SESSION_TTL}; Path=/; HttpOnly; SameSite=Strict"
        );
        if secure {
            c.push_str("; Secure");
        }
        c
    }

    /// A `Set-Cookie` value that immediately clears the session (logout).
    pub fn clear_cookie(&self) -> String {
        format!("{COOKIE_NAME}=; Max-Age=0; Path=/; HttpOnly; SameSite=Strict")
    }

    /// Validate a `<exp>.<sig>` cookie value: well-formed, unexpired, and the
    /// signature recomputes (constant-time) under our key.
    fn validate_cookie(&self, value: &str) -> bool {
        let Some((exp_str, sig_b64)) = value.split_once('.') else {
            return false;
        };
        let Ok(exp) = exp_str.parse::<u64>() else {
            return false;
        };
        if now_secs() >= exp {
            return false;
        }
        let Ok(sig) = URL_SAFE_NO_PAD.decode(sig_b64) else {
            return false;
        };
        let mut mac = HmacSha256::new_from_slice(&self.secret).expect("HMAC key is 32 bytes");
        mac.update(exp_str.as_bytes());
        mac.verify_slice(&sig).is_ok()
    }
}

/// A fresh random API token (32 bytes, base64url). Used by `install --password`
/// to mint one automatically when the user doesn't supply their own, and by the
/// hub to mint per-agent uplink tokens.
pub fn generate_token() -> String {
    let mut buf = [0u8; 32];
    OsRng.fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

/// Whether the *browser* is on https (so a `Secure` cookie is appropriate),
/// inferred from the `X-Forwarded-Proto` set by `tailscale serve` / a TLS proxy.
/// The server itself always speaks plain http on the tailnet.
pub fn is_https(headers: &HeaderMap) -> bool {
    headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("https"))
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Constant-time string compare. (subtle short-circuits on length mismatch — a
/// length leak we accept for this threat model.)
fn ct_eq(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
}

/// Look up one cookie by name from the `Cookie:` header (`a=1; b=2`).
fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}

fn load_or_create_secret(config_dir: &Path) -> [u8; 32] {
    let path = config_dir.join("session.key");
    if let Ok(bytes) = std::fs::read(&path) {
        if bytes.len() >= 32 {
            let mut s = [0u8; 32];
            s.copy_from_slice(&bytes[..32]);
            return s;
        }
    }
    let mut s = [0u8; 32];
    OsRng.fill_bytes(&mut s);
    if let Err(e) = write_secret(&path, &s) {
        // Non-fatal: auth still works this run, but sessions won't survive a
        // restart (a new key invalidates old cookies, forcing re-login).
        tracing::warn!("auth: couldn't persist session key {}: {e}", path.display());
    }
    s
}

#[cfg(unix)]
fn write_secret(path: &Path, s: &[u8; 32]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(s)
}

#[cfg(not(unix))]
fn write_secret(path: &Path, s: &[u8; 32]) -> std::io::Result<()> {
    std::fs::write(path, s)
}

/// Atomically write secret-bearing content to `path` with private (`0600`)
/// permissions on Unix — the bar `session.key` already meets, extended here to
/// installer files (`web.env`, launchd plists that inline secrets). Creates
/// parent dirs, writes a `0600` temp file, atomically renames over any existing
/// file, then re-asserts the mode (fixing a pre-existing world-readable file on
/// migration). Non-Unix falls back to a plain write (no mode control —
/// documented weaker).
#[cfg(unix)]
pub fn write_private_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp-private");
    let tmp = std::path::PathBuf::from(tmp);
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(contents)?;
        f.flush()?;
    }
    std::fs::rename(&tmp, path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
pub fn write_private_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, contents)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [7u8; 32];

    fn pw_auth() -> Auth {
        Auth::new(Some("pw".into()), Some("tok".into()), KEY)
    }

    #[test]
    fn disabled_when_no_credentials() {
        let a = Auth::new(None, None, KEY);
        assert!(!a.enabled());
        // Blank env values count as unset.
        let a = Auth::new(Some("  ".into()), Some(String::new()), KEY);
        assert!(!a.enabled());
    }

    #[test]
    fn verify_login_accepts_password_or_token() {
        let a = pw_auth();
        assert!(a.verify_login("pw"));
        assert!(a.verify_login("tok"));
        assert!(!a.verify_login("nope"));
        assert!(!a.verify_login(""));
    }

    #[test]
    fn cookie_round_trips() {
        let a = pw_auth();
        let set = a.issue_cookie(false);
        // Extract the cookie value from the Set-Cookie string.
        let value = set
            .strip_prefix("ccs_session=")
            .and_then(|s| s.split("; ").next())
            .unwrap();
        assert!(a.validate_cookie(value));
        // The session is ~2 weeks out.
        let exp: u64 = value.split_once('.').unwrap().0.parse().unwrap();
        let dt = exp - now_secs();
        assert!(dt > SESSION_TTL - 5 && dt <= SESSION_TTL, "exp ~2 weeks: {dt}");
        assert!(set.contains("HttpOnly") && set.contains("SameSite=Strict"));
        assert!(!set.contains("Secure"));
        assert!(a.issue_cookie(true).contains("; Secure"));
    }

    #[test]
    fn cookie_rejects_expired_tampered_and_foreign() {
        let a = pw_auth();
        // Expired: sign a past expiry ourselves.
        let past = (now_secs() - 10).to_string();
        let sig = URL_SAFE_NO_PAD.encode(a.sign(past.as_bytes()));
        assert!(!a.validate_cookie(&format!("{past}.{sig}")));
        // Tampered signature.
        let good = a.issue_cookie(false);
        let value = good.strip_prefix("ccs_session=").unwrap().split("; ").next().unwrap();
        let (exp, _) = value.split_once('.').unwrap();
        assert!(!a.validate_cookie(&format!("{exp}.AAAA")));
        // Signed by a different key (different server secret).
        let other = Auth::new(Some("pw".into()), None, [9u8; 32]);
        let theirs = other.issue_cookie(false);
        let tv = theirs.strip_prefix("ccs_session=").unwrap().split("; ").next().unwrap();
        assert!(!a.validate_cookie(tv));
        // Garbage shapes.
        assert!(!a.validate_cookie("nodot"));
        assert!(!a.validate_cookie("notanum.AAAA"));
    }

    #[test]
    fn is_authed_via_token_header_and_query() {
        let a = pw_auth();
        let mut h = HeaderMap::new();
        assert!(!a.is_authed(&h, None));
        h.insert(header::AUTHORIZATION, "Bearer tok".parse().unwrap());
        assert!(a.is_authed(&h, None));
        h.insert(header::AUTHORIZATION, "Bearer wrong".parse().unwrap());
        assert!(!a.is_authed(&h, None));
        // X-Api-Token header.
        let mut h2 = HeaderMap::new();
        h2.insert("x-api-token", "tok".parse().unwrap());
        assert!(a.is_authed(&h2, None));
        // Query-string tokens are NOT accepted (they leak via logs/history) —
        // even the correct token in `?token=` must be rejected now.
        assert!(!a.is_authed(&HeaderMap::new(), Some("session=foo&token=tok")));
        assert!(!a.is_authed(&HeaderMap::new(), Some("session=foo&token=bad")));
    }

    #[test]
    fn is_authed_via_cookie() {
        let a = pw_auth();
        let set = a.issue_cookie(false);
        let value = set.strip_prefix("ccs_session=").unwrap().split("; ").next().unwrap();
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, format!("other=1; ccs_session={value}; x=y").parse().unwrap());
        assert!(a.is_authed(&h, None));
    }

    #[test]
    fn cookie_value_picks_the_right_one() {
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, "a=1; ccs_session=zzz; b=2".parse().unwrap());
        assert_eq!(cookie_value(&h, "ccs_session"), Some("zzz"));
        assert_eq!(cookie_value(&h, "missing"), None);
    }

    #[test]
    fn https_detected_from_forwarded_proto() {
        let mut h = HeaderMap::new();
        assert!(!is_https(&h));
        h.insert("x-forwarded-proto", "https".parse().unwrap());
        assert!(is_https(&h));
        h.insert("x-forwarded-proto", "http".parse().unwrap());
        assert!(!is_https(&h));
    }

    #[test]
    fn generated_tokens_differ() {
        assert_ne!(generate_token(), generate_token());
    }

    #[cfg(unix)]
    #[test]
    fn write_private_file_is_0600_and_fixes_existing() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("ccauth-priv-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("web.env");
        // Pre-create a world-readable file to confirm migration fixes the mode.
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, b"old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        write_private_file(&path, b"CCWEB_PASSWORD=secret\n").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "mode should be private");
        assert_eq!(std::fs::read(&path).unwrap(), b"CCWEB_PASSWORD=secret\n");
        // No leftover temp file.
        assert!(!dir.join("web.env.tmp-private").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
