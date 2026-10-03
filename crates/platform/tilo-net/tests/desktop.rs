//! The desktop backends against loopback servers.
#![cfg(not(target_family = "wasm"))]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::Duration;

use test_http::{Reply, Responder};
use tilo_net::{Client, NetError, Redirects, StatusCode};

#[tokio::test]
async fn a_get_streams_its_body_and_sends_its_headers() {
    let body = "x".repeat(200_000);
    let server = Responder::start(vec![
        Reply::status(200)
            .header("X-Answer", "42")
            .body(body.clone()),
    ]);
    let client = Client::builder()
        .user_agent("tilo-net-test")
        .build()
        .unwrap();
    let mut response = client
        .get(format!("{}/path?q=1", server.url()))
        .header("X-Asked", "yes")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-answer"], "42");
    assert_eq!(response.content_length(), Some(200_000));
    let mut received = Vec::new();
    let mut chunks = 0;
    while let Some(chunk) = response.chunk().await.unwrap() {
        chunks += 1;
        received.extend_from_slice(&chunk);
    }
    assert_eq!(received, body.as_bytes());
    assert!(chunks >= 1);

    let request = server.next_request();
    assert_eq!(request.method, "GET");
    assert_eq!(request.target, "/path?q=1");
    assert_eq!(request.header("x-asked"), Some("yes"));
    assert_eq!(request.header("user-agent"), Some("tilo-net-test"));
}

#[tokio::test]
async fn a_json_body_round_trips() {
    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    struct Item {
        name: String,
        count: u32,
    }
    let server = Responder::start(vec![
        Reply::status(201)
            .header("Content-Type", "application/json")
            .body(r#"{"name":"b","count":2}"#),
    ]);
    let reply: Item = Client::new()
        .post(server.url())
        .json(&Item {
            name: "a".into(),
            count: 1,
        })
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        reply,
        Item {
            name: "b".into(),
            count: 2
        }
    );
    let request = server.next_request();
    assert_eq!(request.method, "POST");
    assert_eq!(request.header("content-type"), Some("application/json"));
    assert_eq!(request.body_text(), r#"{"name":"a","count":1}"#);
}

#[tokio::test]
async fn an_error_status_is_a_response_until_asked() {
    let server = Responder::start(vec![Reply::status(404).body("gone")]);
    let response = Client::new().get(server.url()).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        response.error_for_status().unwrap_err(),
        NetError::Status(StatusCode::NOT_FOUND)
    );
}

#[tokio::test]
async fn redirects_follow_or_surface() {
    let target = Responder::start(vec![Reply::status(200).body("there")]);
    let hop = Responder::start(vec![
        Reply::status(302).header("Location", &format!("{}/end", target.url())),
        Reply::status(302).header("Location", &format!("{}/end", target.url())),
    ]);
    let followed = Client::new().get(hop.url()).send().await.unwrap();
    assert_eq!(followed.url().path(), "/end");
    assert_eq!(followed.text().await.unwrap(), "there");

    let manual = Client::builder()
        .redirects(Redirects::None)
        .build()
        .unwrap()
        .get(hop.url())
        .send()
        .await
        .unwrap();
    assert_eq!(manual.status(), StatusCode::FOUND);
}

#[tokio::test]
async fn a_body_over_the_limit_is_refused() {
    let server = Responder::start(vec![
        Reply::status(200).body(vec![0; 4096]),
        Reply::status(200)
            .body(vec![0; 4096])
            .without_content_length(),
    ]);
    let client = Client::new();
    let declared = client.get(server.url()).send().await.unwrap();
    assert_eq!(
        declared.bytes_limited(1024).await,
        Err(NetError::TooLarge { limit: 1024 })
    );
    let streamed = client.get(server.url()).send().await.unwrap();
    assert_eq!(
        streamed.bytes_limited(1024).await,
        Err(NetError::TooLarge { limit: 1024 })
    );
}

#[tokio::test]
async fn a_silent_server_times_out() {
    let server = Responder::silent();
    let result = Client::new()
        .get(server.url())
        .timeout(Duration::from_millis(300))
        .send()
        .await;
    assert_eq!(result.unwrap_err(), NetError::Timeout);
}

#[tokio::test]
async fn an_unreachable_server_is_a_connect_error() {
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let error = Client::new()
        .get(format!("http://127.0.0.1:{port}/"))
        .send()
        .await
        .unwrap_err();
    assert!(error.is_connect(), "{error:?}");
    assert!(!error.may_be_cors());
}

#[tokio::test]
async fn a_websocket_echoes() {
    use futures_util::{SinkExt as _, StreamExt as _};
    use tilo_net::ws::{self, Message};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (_, mut socket) = tokio_websockets::ServerBuilder::new()
            .accept(stream)
            .await
            .unwrap();
        while let Some(Ok(frame)) = socket.next().await {
            if frame.is_text() || frame.is_binary() {
                socket.send(frame).await.unwrap();
            }
        }
    });

    let mut socket = ws::connect(&format!("ws://127.0.0.1:{port}/echo"))
        .await
        .unwrap();
    socket.send(Message::Text("hi".into())).await.unwrap();
    assert_eq!(
        socket.recv().await.unwrap().unwrap(),
        Message::Text("hi".into())
    );
    socket
        .send(Message::Binary(vec![1, 2, 3].into()))
        .await
        .unwrap();
    assert_eq!(
        socket.recv().await.unwrap().unwrap(),
        Message::Binary(vec![1, 2, 3].into())
    );
    socket.close().await.unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn a_websocket_url_must_be_ws() {
    let error = tilo_net::ws::connect("https://example.com/")
        .await
        .unwrap_err();
    assert!(matches!(error, NetError::InvalidUrl(_)));
}

#[tokio::test]
async fn a_webtransport_session_carries_streams_and_datagrams() {
    use sha2::Digest as _;
    use tilo_net::webtransport::{self, Options};

    let params = rcgen::CertificateParams::new(vec!["127.0.0.1".to_owned()]).unwrap();
    let key = rcgen::KeyPair::generate().unwrap();
    let certificate = params.self_signed(&key).unwrap();
    let der = certificate.der().clone();
    let hash: [u8; 32] = sha2::Sha256::digest(der.as_ref()).into();
    let port = free_udp_port();
    let mut server = web_transport_quinn::ServerBuilder::new()
        .with_addr(([127, 0, 0, 1], port).into())
        .with_certificate(
            vec![der],
            rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into()),
        )
        .unwrap();
    let echo = tokio::spawn(async move {
        let session = server.accept().await.unwrap().ok().await.unwrap();
        let (mut send, mut recv) = session.accept_bi().await.unwrap();
        let request = recv.read_to_end(1024).await.unwrap();
        send.write_all(&request).await.unwrap();
        send.finish().unwrap();
        let datagram = session.read_datagram().await.unwrap();
        session.send_datagram(datagram).unwrap();
        session.closed().await;
    });

    let session = webtransport::connect(
        &format!("https://127.0.0.1:{port}/"),
        Options::pinned(vec![hash]),
    )
    .await
    .unwrap();
    let (mut send, recv) = session.open_bi().await.unwrap();
    send.write_all(b"over a stream").await.unwrap();
    send.finish().await.unwrap();
    assert_eq!(recv.read_to_end(1024).await.unwrap(), b"over a stream");
    session
        .send_datagram(tilo_net::Bytes::from_static(b"unreliable"))
        .await
        .unwrap();
    assert_eq!(&session.recv_datagram().await.unwrap()[..], b"unreliable");
    session.close(0, "done");
    echo.await.unwrap();
}

#[tokio::test]
async fn an_unpinned_self_signed_webtransport_server_is_refused() {
    let params = rcgen::CertificateParams::new(vec!["127.0.0.1".to_owned()]).unwrap();
    let key = rcgen::KeyPair::generate().unwrap();
    let certificate = params.self_signed(&key).unwrap();
    let port = free_udp_port();
    let mut server = web_transport_quinn::ServerBuilder::new()
        .with_addr(([127, 0, 0, 1], port).into())
        .with_certificate(
            vec![certificate.der().clone()],
            rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into()),
        )
        .unwrap();
    tokio::spawn(async move { while server.accept().await.is_some() {} });
    let error = tilo_net::webtransport::connect(
        &format!("https://127.0.0.1:{port}/"),
        tilo_net::webtransport::Options::default(),
    )
    .await
    .unwrap_err();
    assert!(
        error.to_string().contains("certificate"),
        "refused for the certificate, not a timeout: {error:?}"
    );
}

fn free_udp_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
