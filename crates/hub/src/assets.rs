//! The embedded React PWA — the same `frontend/dist` the agent embeds, so the
//! phone opens the *hub* URL and gets the UI (the single-endpoint payoff). Served
//! as the router fallback; exempt from client auth (the app shell carries no
//! secrets and must load so the login screen can render).

use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;

#[derive(rust_embed::RustEmbed)]
#[folder = "../../frontend/dist"]
struct Assets;

fn content_type(path: &str) -> String {
    if path.ends_with(".mjs") {
        return "text/javascript".to_string();
    }
    if path.ends_with(".webmanifest") {
        return "application/manifest+json".to_string();
    }
    mime_guess::from_path(path).first_or_octet_stream().to_string()
}

pub async fn static_handler(uri: Uri) -> Response {
    let raw = uri.path().trim_start_matches('/');
    let path = if raw.is_empty() { "index.html" } else { raw };
    if let Some(f) = Assets::get(path) {
        let ct = content_type(path);
        return ([(header::CONTENT_TYPE, ct)], Bytes::from(f.data.into_owned())).into_response();
    }
    // SPA fallback so client routing works on a hard refresh.
    if let Some(f) = Assets::get("index.html") {
        return (
            [(header::CONTENT_TYPE, "text/html".to_string())],
            Bytes::from(f.data.into_owned()),
        )
            .into_response();
    }
    (StatusCode::NOT_FOUND, "frontend not built").into_response()
}
