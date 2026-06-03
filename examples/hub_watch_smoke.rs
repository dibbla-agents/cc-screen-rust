// Throwaway client to smoke-test the fs-watch relay (M5b) through the hub.
// Connects to /api/watch?machine=…, subscribes a dir, creates a file in it, and
// checks the `{t:"fs",…}` event comes back. Prints `WATCH_EVENT=…`.
//
//   cargo run --example hub_watch_smoke -- 'ws://127.0.0.1:18840/api/watch?machine=smoke' /tmp/dir

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

#[tokio::main]
async fn main() {
    let url = std::env::args().nth(1).expect("usage: hub_watch_smoke <ws-url> <dir>");
    let dir = std::env::args().nth(2).expect("usage: hub_watch_smoke <ws-url> <dir>");
    let req = url.into_client_request().expect("valid ws url");
    let (ws, _) = tokio_tungstenite::connect_async(req).await.expect("connect to hub");
    let (mut write, mut read) = ws.split();

    // Subscribe the directory, then create a file in it.
    let sub = format!(r#"{{"t":"sub","dirs":["{dir}"]}}"#);
    write.send(Message::Text(sub)).await.expect("send sub");
    tokio::time::sleep(Duration::from_millis(400)).await;
    std::fs::write(format!("{dir}/watch_probe.txt"), b"hi").expect("create probe file");

    let mut saw = false;
    for _ in 0..40 {
        match tokio::time::timeout(Duration::from_secs(3), read.next()).await {
            Ok(Some(Ok(Message::Text(t)))) => {
                if t.contains("\"fs\"") && t.contains("watch_probe") {
                    saw = true;
                    break;
                }
            }
            Ok(Some(Ok(_))) => {}
            _ => break,
        }
    }
    println!("WATCH_EVENT={saw}");
}
