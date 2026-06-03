// A throwaway WebSocket client for manually smoke-testing the hub terminal relay
// (M3). Connects to a hub's `/api/ws?machine=&session=`, asserts the first frame
// is an RIS-prefixed snapshot, types a command, and checks it echoes back.
//
//   cargo run --example hub_attach_smoke -- 'ws://127.0.0.1:18840/api/ws?machine=smoke&session=shell-probe'
//
// Prints `RIS_SNAPSHOT=… INPUT_ECHOED=…` for the smoke script to grep.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

#[tokio::main]
async fn main() {
    let url = std::env::args().nth(1).expect("usage: hub_attach_smoke <ws-url>");
    let req = url.into_client_request().expect("valid ws url");
    let (ws, _resp) = tokio_tungstenite::connect_async(req).await.expect("connect to hub");
    let (mut write, mut read) = ws.split();

    // Send our size; the hub relays it to the agent as a resize.
    write
        .send(Message::Text(r#"{"t":"r","c":80,"r":24}"#.to_string()))
        .await
        .expect("send resize");

    // First binary frame should be the RIS-prefixed snapshot.
    let mut got_ris = false;
    for _ in 0..20 {
        match tokio::time::timeout(Duration::from_secs(3), read.next()).await {
            Ok(Some(Ok(Message::Binary(b)))) => {
                if b.starts_with(b"\x1bc") {
                    got_ris = true;
                    break;
                }
            }
            Ok(Some(Ok(_))) => {}
            _ => break,
        }
    }

    // Type a command; its output (and terminal echo) must come back through the hub.
    write
        .send(Message::Binary(b"echo RELAY_PONG\r".to_vec()))
        .await
        .expect("send input");

    let mut echoed = false;
    for _ in 0..60 {
        match tokio::time::timeout(Duration::from_secs(3), read.next()).await {
            Ok(Some(Ok(Message::Binary(b)))) => {
                if String::from_utf8_lossy(&b).contains("RELAY_PONG") {
                    echoed = true;
                    break;
                }
            }
            Ok(Some(Ok(_))) => {}
            _ => break,
        }
    }

    println!("RIS_SNAPSHOT={got_ris} INPUT_ECHOED={echoed}");
}
