// Minimal static-file server for the cc-screen docs site.
//
// Serves ./public on 0.0.0.0:$PORT (default 8080), falling back to index.html.
// Built to Dibbla's contract: plain HTTP, binds 0.0.0.0 (not loopback), and the
// container runs as a non-root user (see Dockerfile). That's the whole thing.

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

    let serve = ServeDir::new(&public)
        .append_index_html_on_directories(true)
        .not_found_service(ServeFile::new(format!("{public}/index.html")));
    let app = Router::new().fallback_service(serve);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .unwrap_or_else(|e| panic!("bind {addr}: {e}"));
    println!("cc-screen-site: serving {public}/ on http://{addr}");
    axum::serve(listener, app).await.expect("serve");
}
