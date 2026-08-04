// Minimal static-file server for the cc-screen docs site.
//
// Serves ./public on 0.0.0.0:$PORT (default 8080), falling back to index.html.
// Built to Dibbla's contract: plain HTTP, binds 0.0.0.0 (not loopback), and the
// container runs as a non-root user (see Dockerfile). That's the whole thing.
//
// One extra (proposal 0054): when SITE_CANONICAL is set (the Dibbla alias sets
// it to https://ccscreen.dev), every request 301s to ${SITE_CANONICAL}${path}
// instead of serving — the legacy cc-screen-<id>.dibbla.app links keep working
// and land on the canonical host.

use axum::http::Uri;
use axum::response::Redirect;
use axum::Router;
use std::net::SocketAddr;
use tower_http::services::{ServeDir, ServeFile};

#[tokio::main]
async fn main() {
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    let public = std::env::var("PUBLIC_DIR").unwrap_or_else(|_| "public".to_string());

    let canonical = std::env::var("SITE_CANONICAL")
        .ok()
        .map(|c| c.trim_end_matches('/').to_string())
        .filter(|c| !c.is_empty());

    let app = match canonical.clone() {
        // Redirect mode: this deployment is a legacy alias for the canonical
        // host — 301 everything, preserving path + query.
        Some(canon) => Router::new().fallback(move |uri: Uri| {
            let canon = canon.clone();
            async move {
                let path = uri
                    .path_and_query()
                    .map(|pq| pq.as_str())
                    .unwrap_or("/");
                Redirect::permanent(&format!("{canon}{path}"))
            }
        }),
        // Serve mode: the static site, SPA-fallback to index.html.
        None => {
            let serve = ServeDir::new(&public)
                .append_index_html_on_directories(true)
                .not_found_service(ServeFile::new(format!("{public}/index.html")));
            Router::new().fallback_service(serve)
        }
    };

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .unwrap_or_else(|e| panic!("bind {addr}: {e}"));
    match canonical {
        Some(canon) => println!("cc-screen-site: redirecting all requests to {canon} on http://{addr}"),
        None => println!("cc-screen-site: serving {public}/ on http://{addr}"),
    }
    axum::serve(listener, app).await.expect("serve");
}
