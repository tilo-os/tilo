//! The desktop backends against public servers. Ignored by default; run
//! with `cargo test -p tilo-net --all-features --test live -- --ignored`.
#![cfg(not(target_family = "wasm"))]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::Duration;

use tilo_net::Client;

#[tokio::test]
#[ignore = "reaches the public internet"]
async fn https_verifies_against_the_system_trust_bundle() {
    let response = Client::new()
        .get("https://example.com/")
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    assert!(response.text().await.unwrap().contains("Example Domain"));
}

#[tokio::test]
#[ignore = "reaches the public internet"]
async fn compressed_bodies_arrive_decoded() {
    let body = Client::new()
        .get("https://httpbin.org/gzip")
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(body.contains("\"gzipped\": true"), "{body}");
}

#[tokio::test]
#[ignore = "reaches the public internet"]
async fn a_secure_websocket_connects() {
    use tilo_net::ws::{self, Message};
    let mut socket = ws::connect("wss://echo.websocket.org/").await.unwrap();
    socket.send(Message::Text("tilo".into())).await.unwrap();
    // The server greets first, then echoes.
    for _ in 0..3 {
        if socket.recv().await.unwrap().unwrap() == Message::Text("tilo".into()) {
            return;
        }
    }
    panic!("no echo");
}

#[tokio::test]
#[ignore = "reaches the public internet"]
async fn gemini_over_trust_on_first_use_tls() {
    use tilo_net::sockets::{TlsStream, Trust};
    let mut tls = TlsStream::connect("geminiprotocol.net", 1965, Trust::AnyCertificate)
        .await
        .unwrap();
    assert!(tls.peer_certificate().is_some());
    tls.write_all(b"gemini://geminiprotocol.net/\r\n")
        .await
        .unwrap();
    let page = tls.read_to_end(1 << 20).await.unwrap();
    assert!(
        page.starts_with(b"20 "),
        "{:?}",
        String::from_utf8_lossy(&page[..40.min(page.len())])
    );
}
