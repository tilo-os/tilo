//! Local review: the host also serves its browser routes as plain HTTP on a
//! loopback address, and refuses a plain listener anywhere else.

#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

mod support;

use tilo_relay::web::{TlsSource, WebConfig};
use tilo_user_identity::{DeviceCapabilities, UserIdentity};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use support::{enrol, start_host_plain};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_routes_answer_over_loopback_plain_http_only() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let state = tempfile::tempdir().unwrap();
    let host = start_host_plain(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
        Some("127.0.0.1:0".parse().unwrap()),
    )
    .await;
    let origin = host.web.plain_origin.clone().expect("a plain origin");
    let port = origin.strip_prefix("http://localhost:").unwrap();

    let request_to = |path: &'static str| {
        let port = port.to_owned();
        async move {
            let mut stream = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
                .await
                .unwrap();
            let request = format!(
                "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 1\r\nConnection: close\r\n\r\n\0"
            );
            stream.write_all(request.as_bytes()).await.unwrap();
            let mut reply = Vec::new();
            stream.read_to_end(&mut reply).await.unwrap();
            String::from_utf8_lossy(&reply).into_owned()
        }
    };
    let reply = request_to("/peer/v1/host").await;
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    // The live fallback's own paths are routed too: a malformed open is
    // refused, not unknown.
    let reply = request_to("/peer/v3/live/fallback/open").await;
    assert!(!reply.starts_with("HTTP/1.1 404"), "{reply}");

    let refused = host
        .relay
        .serve_web(WebConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            tls: TlsSource::Development {
                hosts: vec!["localhost".into()],
                store: None,
            },
            allowed_origins: Vec::new(),
            plain_listen: Some("0.0.0.0:0".parse().unwrap()),
            public_origin: None,
            live_origin: None,
            limits: tilo_relay::web::WebLimits::default(),
            country: None,
        })
        .await
        .is_err();
    drop(host);
    assert!(refused, "a plain listener must stay on loopback");
}
