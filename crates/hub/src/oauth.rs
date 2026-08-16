//! Google "Sign in with Google" — the second login path (proposal 0001 §3.3),
//! compiled only under `multi-tenant`. The Authorization-Code + PKCE flow runs
//! **entirely server-side in the Rust hub**: Next.js only renders a link to
//! `/api/auth/google/start`; it never sees the client secret or the ID token.
//!
//! On success this mints the *exact same* `ccs_session` identity cookie as the
//! password path, so everything downstream (`user_from_cookie`, the §4.1 relay
//! match) is identical regardless of how the user logged in.
//!
//! Config comes from env, read per request (these endpoints are low-frequency):
//! `GOOGLE_OAUTH_CLIENT_ID`, `GOOGLE_OAUTH_CLIENT_SECRET`, and `CCHUB_PUBLIC_URL`
//! (the public origin the redirect/verification URIs are built against). OAuth is
//! disabled (501) unless all are set *and* the hub is multi-tenant.

use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::state::HubState;

const AUTH_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
/// Name of the short-lived HttpOnly cookie parking `state.verifier` between
/// `/start` and `/callback`. Scoped to the OAuth path.
const OAUTH_COOKIE: &str = "ccs_oauth";
/// How long a `/start` stays completable — the cookie's `Max-Age` and the
/// server-side fallback below use the same window.
const PENDING_TTL: Duration = Duration::from_secs(600);
/// Ceiling on concurrently pending logins held server-side. Expired entries are
/// pruned first; a full map only ever drops *someone else's* pending login, who
/// still has the cookie path.
const PENDING_MAX: usize = 512;

struct OAuthConfig {
    client_id: String,
    client_secret: String,
    public_url: String,
}

impl OAuthConfig {
    fn from_env() -> Option<Self> {
        let var = |k: &str| std::env::var(k).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        Some(OAuthConfig {
            client_id: var("GOOGLE_OAUTH_CLIENT_ID")?,
            client_secret: var("GOOGLE_OAUTH_CLIENT_SECRET")?,
            public_url: var("CCHUB_PUBLIC_URL")
                .unwrap_or_else(|| "http://localhost:8840".to_string())
                .trim_end_matches('/')
                .to_string(),
        })
    }

    /// The redirect URI to register in the Google console — points at *this* Rust
    /// callback, not Next.js.
    fn redirect_uri(&self) -> String {
        format!("{}/api/auth/google/callback", self.public_url)
    }
}

/// Whether Google sign-in is configured (client id + secret in env), so the UI
/// knows to show the "Sign in with Google" button.
pub fn is_configured() -> bool {
    OAuthConfig::from_env().is_some()
}

/// One `/start` waiting for its callback, held server-side so a browser that
/// does not hand the `ccs_oauth` cookie back can still finish the login.
///
/// Why this exists: the cookie is `SameSite=Lax` and the callback is a top-level
/// GET, which is exactly the case Lax permits — and yet an installed PWA window
/// (Chrome app) has been observed arriving at `/callback` without it. Failing
/// that login was the whole bug.
///
/// What it costs: the cookie binds the `state` to *this browser*, which is what
/// makes login-CSRF (a victim silently signed into the attacker's account) hard.
/// The fallback keeps as much of that as it can without the cookie — the entry
/// is **single-use**, expires with the same 10-minute window, and is bound to the
/// `source_key` (proxy-reported IP) the `/start` came from — and is consulted
/// **only** when the cookie is absent. A cookie that *is* present must still
/// match, so the normal path is unchanged.
struct Pending {
    verifier: String,
    expires: Instant,
    source: String,
}

fn pending_map() -> &'static Mutex<HashMap<String, Pending>> {
    static PENDING: OnceLock<Mutex<HashMap<String, Pending>>> = OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

fn pending_put(state: &str, verifier: &str, source: String) {
    let now = Instant::now();
    let mut map = pending_map().lock().unwrap();
    map.retain(|_, p| p.expires > now);
    if map.len() >= PENDING_MAX {
        // Evict the entry closest to expiry rather than an arbitrary one.
        if let Some(k) = map
            .iter()
            .min_by_key(|(_, p)| p.expires)
            .map(|(k, _)| k.clone())
        {
            map.remove(&k);
        }
    }
    map.insert(
        state.to_string(),
        Pending { verifier: verifier.to_string(), expires: now + PENDING_TTL, source },
    );
}

/// Consume the pending entry for `state`. Single-use: a replayed callback URL
/// (the Chrome app restoring the last URL it was on) finds nothing.
fn pending_take(state: &str, source: &str) -> Option<String> {
    let mut map = pending_map().lock().unwrap();
    let p = map.remove(state)?;
    if p.expires <= Instant::now() || p.source != source {
        return None;
    }
    Some(p.verifier)
}

/// Every failure of the callback ends here: back to the app with a code in the
/// query, never a dead-end `400` body. The window a user lands in after Google
/// is often the *app itself* (an installed PWA), where a bare error string is
/// unrecoverable without knowing to retype the URL — and where a restored or
/// reloaded stale callback URL would otherwise render as a broken app.
/// `App.tsx` reads `login_error`, shows it on the sign-in screen, and strips it.
fn fail(reason: &str) -> Response {
    (
        StatusCode::FOUND,
        [
            (header::LOCATION, format!("/?login_error={reason}")),
            (header::CACHE_CONTROL, "no-store".to_string()),
        ],
    )
        .into_response()
}

/// `GET /api/auth/google/start` — 302 to Google's consent screen with a fresh
/// `state` (CSRF) and PKCE `code_challenge`, parking the matching `state.verifier`
/// in a 10-minute HttpOnly cookie.
pub async fn google_start(State(hub): State<HubState>, headers: HeaderMap) -> Response {
    if !hub.multi_tenant() {
        return (StatusCode::NOT_IMPLEMENTED, "multi-tenant only").into_response();
    }
    let Some(cfg) = OAuthConfig::from_env() else {
        return (StatusCode::NOT_IMPLEMENTED, "google oauth not configured").into_response();
    };
    let state = cc_screen_auth::generate_token();
    let verifier = cc_screen_auth::generate_token();
    let challenge = cc_screen_auth::sha256_b64url(&verifier);
    let url = format!(
        "{AUTH_ENDPOINT}?response_type=code&client_id={}&redirect_uri={}&scope={}\
         &state={}&code_challenge={}&code_challenge_method=S256&access_type=online&prompt=select_account",
        enc(&cfg.client_id),
        enc(&cfg.redirect_uri()),
        enc("openid email"),
        enc(&state),
        enc(&challenge),
    );
    // SameSite=Lax (not Strict): the callback is a top-level GET navigation from
    // accounts.google.com, and Strict would drop the cookie on that cross-site hop.
    let secure = if cc_screen_auth::is_https(&headers) { "; Secure" } else { "" };
    let cookie = format!(
        "{OAUTH_COOKIE}={state}.{verifier}; Max-Age=600; Path=/api/auth/google; HttpOnly; SameSite=Lax{secure}"
    );
    // The same pair, server-side, for the browser that loses the cookie (see
    // `Pending`). `no-store` so this 302 — whose Location carries a single-use
    // state — can never be replayed out of the HTTP cache.
    pending_put(&state, &verifier, cc_screen_auth::source_key(&headers));
    (
        StatusCode::FOUND,
        [
            (header::LOCATION, url),
            (header::SET_COOKIE, cookie),
            (header::CACHE_CONTROL, "no-store".to_string()),
        ],
    )
        .into_response()
}

#[derive(Deserialize)]
pub struct CallbackQ {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

/// `GET /api/auth/google/callback` — verify `state`, exchange the code
/// server-side (client secret + PKCE verifier), read the ID token's claims, upsert
/// the user, and mint the identity cookie. Redirects to the app root on success.
pub async fn google_callback(
    State(hub): State<HubState>,
    headers: HeaderMap,
    Query(q): Query<CallbackQ>,
) -> Response {
    if !hub.multi_tenant() {
        return (StatusCode::NOT_IMPLEMENTED, "multi-tenant only").into_response();
    }
    let Some(cfg) = OAuthConfig::from_env() else {
        return (StatusCode::NOT_IMPLEMENTED, "google oauth not configured").into_response();
    };
    if let Some(err) = q.error.as_deref() {
        tracing::warn!("oauth-cb: google denied: {err}");
        return fail("denied");
    }
    let (Some(code), Some(state)) = (q.code.as_deref(), q.state.as_deref()) else {
        tracing::warn!("oauth-cb: missing code/state");
        return fail("request");
    };
    let source = cc_screen_auth::source_key(&headers);
    // Recover the parked state+verifier. Preferred source is the cookie, whose
    // state must match (CSRF defense); only when the browser sent no cookie at
    // all do we fall back to the single-use server-side entry.
    let cookie_pair = cookie_value(&headers, OAUTH_COOKIE).and_then(|c| {
        let (s, v) = c.split_once('.')?;
        Some((s.to_string(), v.to_string()))
    });
    let verifier = match cookie_pair {
        Some((c_state, v)) => {
            // A cookie is present: it decides, exactly as before.
            pending_take(state, &source);
            if c_state != state {
                tracing::warn!("oauth-cb: state mismatch cookie={c_state} query={state}");
                return fail("state");
            }
            v
        }
        None => match pending_take(state, &source) {
            Some(v) => {
                tracing::info!("oauth-cb: no state cookie, completing from server-side pending state");
                v
            }
            None => {
                // Either a genuinely unknown/expired state, or a callback URL
                // replayed after its one use (a restored app window).
                tracing::warn!("oauth-cb: no state cookie and no pending state — expired or replayed");
                return fail("expired");
            }
        },
    };

    // Exchange the code directly with Google over TLS using our client secret.
    let resp = reqwest::Client::new()
        .post(TOKEN_ENDPOINT)
        .form(&[
            ("code", code),
            ("client_id", cfg.client_id.as_str()),
            ("client_secret", cfg.client_secret.as_str()),
            ("redirect_uri", cfg.redirect_uri().as_str()),
            ("grant_type", "authorization_code"),
            ("code_verifier", verifier.as_str()),
        ])
        .send()
        .await;
    let token: TokenResp = match resp {
        Ok(r) if r.status().is_success() => match r.json().await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("oauth-cb: token parse failed: {e}");
                return fail("google");
            }
        },
        Ok(r) => {
            let status = r.status();
            let body = r.text().await.unwrap_or_default();
            tracing::warn!("oauth-cb: token exchange HTTP {status}: {}", body.chars().take(300).collect::<String>());
            return fail("google");
        }
        Err(e) => {
            tracing::warn!("oauth-cb: token exchange transport error: {e}");
            return fail("google");
        }
    };

    // The id_token came straight from Google's token endpoint over an
    // authenticated TLS channel, so the claims are trustworthy without separately
    // verifying the JWT signature (per Google's OIDC guidance for the token
    // response). We only base64url-decode the payload.
    let Some(claims) = decode_id_token(&token.id_token) else {
        tracing::warn!("oauth-cb: malformed id_token");
        return fail("google");
    };
    if !claims.email_verified.unwrap_or(false) {
        tracing::warn!("oauth-cb: google email not verified");
        return fail("unverified");
    }
    let (Some(sub), Some(email)) = (claims.sub.as_deref(), claims.email.as_deref()) else {
        tracing::warn!("oauth-cb: id_token missing sub/email");
        return fail("google");
    };

    let Some(user_id) = hub.upsert_google_user(sub, email).await else {
        tracing::warn!("oauth: could not provision user for {email}");
        return fail("account");
    };
    // Land any email invites waiting for this address (proposal 0056 C3) — this
    // covers a fresh Google signup AND the first Google login of an address
    // invited pre-account. Idempotent (attached rows are stamped converted).
    hub.attach_email_invites(&user_id, email).await;
    let session = hub.client_auth.issue_cookie_for(&user_id, cc_screen_auth::is_https(&headers));
    // Single Set-Cookie: emit ONLY the session cookie. We deliberately do NOT also
    // clear the short-lived ccs_oauth state cookie here — a second Set-Cookie on
    // this 302 can get mangled by intermediaries (Cloudflare), dropping the
    // session cookie. ccs_oauth is path-scoped + Max-Age 600, so it expires on its
    // own and is overwritten on the next attempt; not worth risking the login.
    (
        StatusCode::FOUND,
        [
            (header::LOCATION, "/".to_string()),
            (header::SET_COOKIE, session),
            (header::CACHE_CONTROL, "no-store".to_string()),
        ],
    )
        .into_response()
}

#[derive(Deserialize)]
struct TokenResp {
    id_token: String,
}

#[derive(Deserialize)]
struct IdClaims {
    sub: Option<String>,
    email: Option<String>,
    #[serde(default)]
    email_verified: Option<bool>,
}

/// Decode the (middle) claims segment of a JWT without signature verification —
/// safe here only because the token came from the authenticated token-endpoint
/// response. Google sometimes encodes `email_verified` as the string "true".
fn decode_id_token(jwt: &str) -> Option<IdClaims> {
    let payload = jwt.split('.').nth(1)?;
    let bytes = cc_screen_auth::b64url_decode(payload)?;
    // Tolerate the stringified boolean Google occasionally emits.
    let mut v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    if let Some(s) = v.get("email_verified").and_then(|x| x.as_str()) {
        let b = s.eq_ignore_ascii_case("true");
        v["email_verified"] = serde_json::Value::Bool(b);
    }
    serde_json::from_value(v).ok()
}

/// Percent-encode a query-parameter value (RFC 3986 unreserved set passes through).
fn enc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => o.push(b as char),
            _ => o.push_str(&format!("%{b:02X}")),
        }
    }
    o
}

/// One cookie value by name from the `Cookie:` header.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enc_encodes_reserved_and_passes_unreserved() {
        assert_eq!(enc("openid email"), "openid%20email");
        assert_eq!(enc("http://localhost:8840/api/auth/google/callback"),
                   "http%3A%2F%2Flocalhost%3A8840%2Fapi%2Fauth%2Fgoogle%2Fcallback");
        assert_eq!(enc("Aa0-_.~"), "Aa0-_.~");
    }

    // One test, not three: the store is process-global, and the capacity case
    // evicts — split across parallel tests they would flake on each other.
    #[test]
    fn pending_state_is_single_use_source_bound_expiring_and_bounded() {
        pending_put("st-a", "ver-a", "1.2.3.4".to_string());
        // Wrong source: refused — and the entry is spent either way, so a probe
        // from elsewhere can't be retried from the right address.
        assert_eq!(pending_take("st-a", "9.9.9.9"), None);
        assert_eq!(pending_take("st-a", "1.2.3.4"), None);

        pending_put("st-b", "ver-b", "1.2.3.4".to_string());
        assert_eq!(pending_take("st-b", "1.2.3.4").as_deref(), Some("ver-b"));
        // Replay of the same callback URL (a restored app window) finds nothing.
        assert_eq!(pending_take("st-b", "1.2.3.4"), None);
        // An unknown state was never pending.
        assert_eq!(pending_take("st-never", "1.2.3.4"), None);

        // Expired entries are never handed back.
        pending_map().lock().unwrap().insert(
            "st-old".to_string(),
            Pending {
                verifier: "ver".to_string(),
                expires: Instant::now() - Duration::from_secs(1),
                source: "1.2.3.4".to_string(),
            },
        );
        assert_eq!(pending_take("st-old", "1.2.3.4"), None);

        // And the map cannot grow without bound (evicts, last so it can't
        // disturb the assertions above).
        for i in 0..(PENDING_MAX + 40) {
            pending_put(&format!("cap-{i}"), "v", "1.2.3.4".to_string());
        }
        assert!(pending_map().lock().unwrap().len() <= PENDING_MAX);
    }

    #[test]
    fn failure_redirects_into_the_app_with_a_reason() {
        let r = fail("expired");
        assert_eq!(r.status(), StatusCode::FOUND);
        assert_eq!(r.headers().get(header::LOCATION).unwrap(), "/?login_error=expired");
        assert_eq!(r.headers().get(header::CACHE_CONTROL).unwrap(), "no-store");
    }

    #[test]
    fn decode_id_token_reads_claims_and_string_bool() {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine;
        // A fake JWT whose middle segment carries Google-shaped claims (with the
        // stringified boolean Google sometimes emits).
        let body = serde_json::json!({"sub":"123","email":"a@b.com","email_verified":"true"});
        let seg = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&body).unwrap());
        let c = decode_id_token(&format!("header.{seg}.sig")).expect("decodes");
        assert_eq!(c.sub.as_deref(), Some("123"));
        assert_eq!(c.email.as_deref(), Some("a@b.com"));
        assert_eq!(c.email_verified, Some(true));
    }
}
