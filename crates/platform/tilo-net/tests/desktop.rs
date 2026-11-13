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
    // A socket that holds its address without listening on it: the port is
    // nothing's to take, so the client's refusal is this socket's own for as
    // long as the test runs.
    let held = tokio::net::TcpSocket::new_v4().unwrap();
    held.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let port = held.local_addr().unwrap().port();
    let error = Client::new()
        .get(format!("http://127.0.0.1:{port}/"))
        .send()
        .await
        .unwrap_err();
    assert!(error.is_connect(), "{error:?}");
    assert!(!error.may_be_cors());
    drop(held);
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
    let mut server = web_transport_quinn::ServerBuilder::new()
        .with_addr(([127, 0, 0, 1], 0).into())
        .with_certificate(
            vec![der],
            rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into()),
        )
        .unwrap();
    let port = server.local_addr().unwrap().port();
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
    let mut server = web_transport_quinn::ServerBuilder::new()
        .with_addr(([127, 0, 0, 1], 0).into())
        .with_certificate(
            vec![certificate.der().clone()],
            rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into()),
        )
        .unwrap();
    let port = server.local_addr().unwrap().port();
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

/// An HTTPS server on loopback presenting `chain`, answering `ok` to every
/// request, one connection at a time.
fn https_server(
    chain: Vec<rustls::pki_types::CertificateDer<'static>>,
    key: &rcgen::KeyPair,
) -> u16 {
    use std::io::{Read as _, Write as _};
    let config = rustls::ServerConfig::builder_with_provider(tilo_net::crypto_provider())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            chain,
            rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into()),
        )
        .unwrap();
    let config = std::sync::Arc::new(config);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for tcp in listener.incoming() {
            let Ok(tcp) = tcp else { continue };
            let connection = rustls::ServerConnection::new(config.clone()).unwrap();
            let mut tls = rustls::StreamOwned::new(connection, tcp);
            let mut head = Vec::new();
            let mut byte = [0_u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                if tls.read(&mut byte).unwrap_or(0) == 0 {
                    break;
                }
                head.push(byte[0]);
            }
            let _ = tls
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
            let _ = tls.flush();
            tls.conn.send_close_notify();
            let _ = tls.flush();
        }
    });
    port
}

async fn fetched(client: &Client, port: u16) -> Result<StatusCode, NetError> {
    Ok(client
        .get(format!("https://127.0.0.1:{port}/"))
        .send()
        .await?
        .status())
}

#[tokio::test]
async fn extra_trust_takes_a_ca_or_an_exact_self_signed_certificate_and_nothing_else() {
    let loopback = || rcgen::CertificateParams::new(vec!["127.0.0.1".to_owned()]).unwrap();
    let leaf_key = rcgen::KeyPair::generate().unwrap();
    let leaf = loopback().self_signed(&leaf_key).unwrap();
    let self_signed = https_server(vec![leaf.der().clone()], &leaf_key);
    let mut ca_params = rcgen::CertificateParams::new(Vec::new()).unwrap();
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let ca_issuer = rcgen::Issuer::new(ca_params, &ca_key);
    let issued_key = rcgen::KeyPair::generate().unwrap();
    let issued = loopback().signed_by(&issued_key, &ca_issuer).unwrap();
    let chained = https_server(vec![issued.der().clone()], &issued_key);
    let other_key = rcgen::KeyPair::generate().unwrap();
    let other = loopback().self_signed(&other_key).unwrap();
    let stranger = https_server(vec![other.der().clone()], &other_key);

    let trusting = Client::builder()
        .tls(
            tilo_net::client_config_also_trusting(vec![leaf.der().clone(), ca.der().clone()])
                .unwrap(),
        )
        .build()
        .unwrap();
    assert_eq!(
        fetched(&trusting, self_signed).await.unwrap(),
        StatusCode::OK
    );
    assert_eq!(fetched(&trusting, chained).await.unwrap(), StatusCode::OK);
    assert!(
        fetched(&trusting, stranger).await.is_err(),
        "verification stays on for everything else"
    );
    let plain = Client::new();
    assert!(fetched(&plain, self_signed).await.is_err());
    assert!(fetched(&plain, chained).await.is_err());
}
