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

use crate::state::HubState;

const AUTH_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
/// Name of the short-lived HttpOnly cookie parking `state.verifier` between
/// `/start` and `/callback`. Scoped to the OAuth path.
const OAUTH_COOKIE: &str = "ccs_oauth";

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
    (StatusCode::FOUND, [(header::LOCATION, url), (header::SET_COOKIE, cookie)]).into_response()
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
        return (StatusCode::UNAUTHORIZED, format!("google denied: {err}")).into_response();
    }
    let (Some(code), Some(state)) = (q.code.as_deref(), q.state.as_deref()) else {
        return (StatusCode::BAD_REQUEST, "missing code/state").into_response();
    };
    // Recover the parked state+verifier; the state must match (CSRF defense).
    let Some((c_state, verifier)) = cookie_value(&headers, OAUTH_COOKIE).and_then(|c| {
        let (s, v) = c.split_once('.')?;
        Some((s.to_string(), v.to_string()))
    }) else {
        return (StatusCode::BAD_REQUEST, "missing oauth state cookie").into_response();
    };
    if c_state != state {
        return (StatusCode::BAD_REQUEST, "oauth state mismatch").into_response();
    }

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
            Err(e) => return (StatusCode::BAD_GATEWAY, format!("token parse: {e}")).into_response(),
        },
        Ok(r) => return (StatusCode::UNAUTHORIZED, format!("token exchange failed ({})", r.status())).into_response(),
        Err(e) => return (StatusCode::BAD_GATEWAY, format!("token exchange: {e}")).into_response(),
    };

    // The id_token came straight from Google's token endpoint over an
    // authenticated TLS channel, so the claims are trustworthy without separately
    // verifying the JWT signature (per Google's OIDC guidance for the token
    // response). We only base64url-decode the payload.
    let Some(claims) = decode_id_token(&token.id_token) else {
        return (StatusCode::BAD_GATEWAY, "malformed id_token").into_response();
    };
    if !claims.email_verified.unwrap_or(false) {
        return (StatusCode::FORBIDDEN, "google email not verified").into_response();
    }
    let (Some(sub), Some(email)) = (claims.sub.as_deref(), claims.email.as_deref()) else {
        return (StatusCode::BAD_GATEWAY, "id_token missing sub/email").into_response();
    };

    let Some(user_id) = hub.upsert_google_user(sub, email).await else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "could not provision user").into_response();
    };

    let session = hub.client_auth.issue_cookie_for(&user_id, cc_screen_auth::is_https(&headers));
    let clear = format!("{OAUTH_COOKIE}=; Max-Age=0; Path=/api/auth/google; HttpOnly; SameSite=Lax");
    // Same-origin app root; the freshly-set session cookie rides subsequent loads.
    (
        StatusCode::FOUND,
        [
            (header::LOCATION, "/".to_string()),
            (header::SET_COOKIE, session),
            (header::SET_COOKIE, clear),
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
