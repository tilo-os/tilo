#![cfg(not(target_family = "wasm"))]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
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

/// A path that speaks one protocol carries it whether or not the browser
/// negotiated it: Chrome before `WebTransport.protocol` shipped offers no
/// subprotocols at all, so a session opened there negotiates none, and both
/// ends take the path's protocol ([`tilo_net::webtransport::Session::speaking`]).
#[tokio::test]
async fn a_session_at_a_one_protocol_path_speaks_it_whether_or_not_it_was_negotiated() {
    use sha2::Digest as _;
    use tilo_net::webtransport::{self, Answer, Listener, Options};

    let params = rcgen::CertificateParams::new(vec!["127.0.0.1".to_owned()]).unwrap();
    let key = rcgen::KeyPair::generate().unwrap();
    let certificate = params.self_signed(&key).unwrap();
    let der = certificate.der().clone();
    let hash: [u8; 32] = sha2::Sha256::digest(der.as_ref()).into();
    let resolver = rustls::sign::SingleCertAndKey::from(
        rustls::sign::CertifiedKey::from_der(
            vec![der],
            rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into()),
            &rustls::crypto::ring::default_provider(),
        )
        .unwrap(),
    );
    let socket = std::net::UdpSocket::bind(("127.0.0.1", 0)).unwrap();
    let listener = Listener::bind(socket, std::sync::Arc::new(resolver)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (heard, mut hearing) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(listener.serve(
        |_path: &str, offered: &[String]| {
            if offered.iter().any(|protocol| protocol == "one-03") {
                Answer::AcceptAs("one-03".to_owned())
            } else {
                Answer::Accept
            }
        },
        move |_path, session| {
            let heard = heard.clone();
            async move {
                let _ = heard.send((
                    session.protocol().map(str::to_owned),
                    session
                        .clone()
                        .speaking("one-03")
                        .protocol()
                        .map(str::to_owned),
                ));
                // Kept open until the client has read what it negotiated.
                let _ = session.accept_bi().await;
            }
        },
    ));
    let url = format!("https://127.0.0.1:{port}/one");

    // As Chrome 140 dials: no subprotocols offered.
    let unnegotiated = webtransport::connect(&url, Options::pinned(vec![hash]))
        .await
        .unwrap();
    assert_eq!(unnegotiated.protocol(), None);
    assert_eq!(
        unnegotiated.clone().speaking("one-03").protocol(),
        Some("one-03"),
        "the client takes the protocol from the path"
    );
    assert_eq!(
        hearing.recv().await.unwrap(),
        (None, Some("one-03".to_owned())),
        "so does the server"
    );
    unnegotiated.close(0, "done");

    // A browser that negotiates keeps what it negotiated.
    let negotiated = webtransport::connect(
        &url,
        Options::pinned(vec![hash]).with_protocols(vec!["one-03".to_owned()]),
    )
    .await
    .unwrap();
    assert_eq!(negotiated.protocol(), Some("one-03"));
    assert_eq!(
        negotiated.clone().speaking("other-01").protocol(),
        Some("one-03"),
        "a negotiated protocol stands"
    );
    assert_eq!(
        hearing.recv().await.unwrap(),
        (Some("one-03".to_owned()), Some("one-03".to_owned()))
    );
    negotiated.close(0, "done");
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

/// A client that dials directly sends nothing to a proxy its environment
/// names, by any of the names one goes by: its request goes where its own
/// lookup answered. A client built without `direct()` sends the same request
/// to that proxy and never asks its lookup, so whoever judges addresses in
/// the lookup has to dial directly.
#[tokio::test]
async fn a_direct_client_sends_nothing_to_a_proxy_its_environment_names() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    if !tilo_test_env::in_own_process_here(
        module_path!(),
        "a_direct_client_sends_nothing_to_a_proxy_its_environment_names",
    ) {
        return;
    }
    let proxy = Responder::handle(|_| Some(Reply::status(200).body("the proxy's answer")));
    let page = Responder::handle(|_| Some(Reply::status(200).body("the page")));
    let mut environment = tilo_test_env::ScopedEnv::new();
    for name in [
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        environment.set(name, proxy.url());
    }
    // Nothing exempts an address from that proxy, and nothing has a name of it ignored.
    for name in ["NO_PROXY", "no_proxy", "REQUEST_METHOD"] {
        environment.remove(name);
    }
    let asked = Arc::new(AtomicUsize::new(0));
    let looking = |asked: &Arc<AtomicUsize>| -> tilo_net::Lookup {
        let asked = Arc::clone(asked);
        Arc::new(
            move |_name: String| -> std::pin::Pin<
                Box<dyn Future<Output = std::io::Result<Vec<std::net::IpAddr>>> + Send>,
            > {
                asked.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok(vec![std::net::IpAddr::from([127, 0, 0, 1])]) })
            },
        )
    };
    // A name no resolver knows: only the client's own lookup says where it is.
    let address = format!(
        "http://a-name-only-the-lookup-knows.invalid:{}/page",
        page.port()
    );

    let through = Client::builder().lookup(looking(&asked)).build().unwrap();
    let answer = through
        .get(address.as_str())
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(
        answer, "the proxy's answer",
        "without `direct()` the environment's proxy is taken: the test sees what it tests"
    );
    assert_eq!(proxy.drain_requests().len(), 1);
    assert_eq!(
        asked.load(Ordering::SeqCst),
        0,
        "a proxy resolves the name itself: the client's lookup is never asked"
    );
    assert!(page.drain_requests().is_empty());

    let direct = Client::builder()
        .direct()
        .lookup(looking(&asked))
        .build()
        .unwrap();
    let answer = direct.get(address.as_str()).send().await;
    let sent = proxy.drain_requests();
    assert!(
        sent.is_empty(),
        "a direct client sent the proxy its environment names {} request(s)",
        sent.len()
    );
    assert_eq!(answer.unwrap().text().await.unwrap(), "the page");
    assert_eq!(
        asked.load(Ordering::SeqCst),
        1,
        "the address dialled is the one the client's own lookup answered"
    );
    assert_eq!(page.drain_requests().len(), 1);
}

/// Destinations on this machine (`localhost`, `*.localhost`, `127.0.0.0/8`,
/// `[::1]`) never leave it through an environment proxy, even when `HTTP_PROXY`
/// and `ALL_PROXY` are set without `NO_PROXY`; destinations off this machine
/// keep their proxy behaviour.
#[tokio::test]
async fn local_destinations_never_reach_an_environment_proxy() {
    if !tilo_test_env::in_own_process_here(
        module_path!(),
        "local_destinations_never_reach_an_environment_proxy",
    ) {
        return;
    }
    let proxy = Responder::handle(|_| Some(Reply::status(200).body("the proxy's answer")));
    let local = Responder::handle(|_| Some(Reply::status(200).body("this machine")));
    let mut environment = tilo_test_env::ScopedEnv::new();
    for name in [
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        environment.set(name, proxy.url());
    }
    for name in ["NO_PROXY", "no_proxy", "REQUEST_METHOD"] {
        environment.remove(name);
    }

    let local_urls = [
        format!("http://tilo.localhost:{}/local", local.port()),
        format!("http://localhost.:{}/local", local.port()),
        format!("http://127.0.0.1:{}/local", local.port()),
    ];
    let remote_url = format!(
        "http://outside-this-machine.invalid:{}/remote",
        local.port()
    );

    let async_client = tilo_net::http::client().build().unwrap();
    for url in &local_urls {
        let body = async_client
            .get(url.as_str())
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(body, "this machine", "async request to {url}");
    }
    assert!(
        proxy.drain_requests().is_empty(),
        "local async requests must never reach the environment proxy"
    );
    assert_eq!(local.drain_requests().len(), local_urls.len());

    let proxied = async_client
        .get(remote_url.as_str())
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(proxied, "the proxy's answer");
    assert_eq!(proxy.drain_requests().len(), 1);
    assert!(local.drain_requests().is_empty());

    #[cfg(feature = "blocking")]
    {
        let local_urls_blocking = local_urls.clone();
        let remote_url_blocking = remote_url.clone();
        let (local_bodies, proxied_body) = tokio::task::spawn_blocking(move || {
            let blocking_client = tilo_net::http::blocking_client().build().unwrap();
            let bodies: Vec<String> = local_urls_blocking
                .iter()
                .map(|url| {
                    blocking_client
                        .get(url.as_str())
                        .send()
                        .unwrap()
                        .text()
                        .unwrap()
                })
                .collect();
            let remote = blocking_client
                .get(remote_url_blocking.as_str())
                .send()
                .unwrap()
                .text()
                .unwrap();
            (bodies, remote)
        })
        .await
        .unwrap();
        for (url, body) in local_urls.iter().zip(&local_bodies) {
            assert_eq!(body, "this machine", "blocking request to {url}");
        }
        assert_eq!(proxied_body, "the proxy's answer");
        assert_eq!(proxy.drain_requests().len(), 1);
        assert_eq!(local.drain_requests().len(), local_urls.len());
    }
}

/// A client whose lookup is the one judge's (`address::public_only`) is
/// refused a name that answers with an address of this machine: the request
/// ends as blocked, told apart from a connection that failed, and nothing is
/// sent.
#[tokio::test]
async fn a_client_that_looks_up_public_only_is_refused_a_name_that_answers_with_this_machine() {
    let page = Responder::handle(|_| Some(Reply::status(200).body("the page")));
    let lookup: tilo_net::Lookup = std::sync::Arc::new(
        |_name: String| -> std::pin::Pin<
            Box<dyn Future<Output = std::io::Result<Vec<std::net::IpAddr>>> + Send>,
        > { Box::pin(async { Ok(vec![std::net::IpAddr::from([127, 0, 0, 1])]) }) },
    );
    let client = Client::builder()
        .direct()
        .lookup(tilo_net::address::public_only(lookup))
        .build()
        .unwrap();
    let address = format!(
        "http://a-name-only-the-lookup-knows.invalid:{}/page",
        page.port()
    );
    let refused = client.get(address.as_str()).send().await;
    assert!(
        matches!(refused, Err(NetError::Blocked(_))),
        "a name the one judge refuses ends as blocked"
    );
    assert!(page.drain_requests().is_empty(), "nothing was sent");
}

/// A body the server compressed arrives decoded: a deployment's CDN and a
/// proxy compress as a matter of course, and a client that handed the gzip
/// bytes on as the page would show a reader binary.
#[tokio::test]
async fn a_compressed_body_arrives_decoded() {
    use std::io::Write as _;

    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(b"{\"gzipped\": true}").unwrap();
    let server = Responder::start(vec![
        Reply::status(200)
            .header("Content-Encoding", "gzip")
            .body(encoder.finish().unwrap()),
    ]);
    let body = Client::new()
        .get(server.url())
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(body, "{\"gzipped\": true}");
}

/// A server with no certificate a trust store knows is still reached, first
/// use, and its certificate is what the caller pins: the exchange on it
/// carries a real request and the reply comes back.
#[tokio::test]
async fn trust_on_first_use_reaches_a_self_signed_server_and_yields_its_certificate() {
    use tilo_net::sockets::{TlsStream, Trust};

    let key = rcgen::KeyPair::generate().unwrap();
    let certificate = rcgen::CertificateParams::new(vec!["127.0.0.1".to_owned()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    let port = https_server(vec![certificate.der().clone()], &key);

    let mut tls = TlsStream::connect("127.0.0.1", port, Trust::AnyCertificate)
        .await
        .unwrap();
    assert_eq!(
        tls.peer_certificate().as_deref(),
        Some(certificate.der().as_ref()),
        "the certificate first use was asked to accept, before any pin"
    );
    tls.write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
        .await
        .unwrap();
    let page = tls.read_to_end(1 << 20).await.unwrap();
    assert!(page.starts_with(b"HTTP/1.1 200 OK"), "{page:?}");
}
