use super::*;
use rustls::pki_types::{PrivatePkcs8KeyDer, ServerName};

const T: Duration = Duration::from_secs(5);

struct Fixture {
    port: u16,
    certificate: CertificateDer<'static>,
    client_tls: Arc<rustls::ClientConfig>,
    stop: watch::Sender<bool>,
    tcp: tokio::task::JoinHandle<io::Result<()>>,
    udp: tokio::task::JoinHandle<()>,
    wt: tokio::sync::mpsc::Receiver<web_transport_quinn::Request>,
    _assets: tempfile::TempDir,
}

/// The real HTTPS adapter (TCP) plus the shared-endpoint dispatcher (UDP) on
/// one port, as `RunningTransport` installs them.
async fn start() -> (Fixture, IngressReceiver) {
    let assets = tempfile::tempdir().unwrap();
    std::fs::write(assets.path().join("index.html"), b"real asset bytes").unwrap();
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()]).unwrap();
    let key: PrivateKeyDer<'static> = PrivatePkcs8KeyDer::from(signing_key.serialize_der()).into();
    let (ingress, receiver) = super::super::ingress_channel();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let adapter = HttpsAdapter::new(
        listener,
        &format!("https://localhost:{}", address.port()),
        crate::https_assets::AssetRoot::open(assets.path()).unwrap(),
        vec![cert.der().clone()],
        key.clone_key(),
        ingress.clone(),
    )
    .unwrap()
    .with_alt_svc();
    let service = adapter.h3_service();
    let (stop, stopped) = watch::channel(false);
    let tcp = tokio::spawn(adapter.serve(stopped));
    let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![cert.der().clone()], key)
    .unwrap();
    tls.alpn_protocols = vec![b"h3".to_vec()];
    let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls).unwrap();
    let endpoint =
        quinn::Endpoint::server(quinn::ServerConfig::with_crypto(Arc::new(crypto)), address)
            .unwrap();
    let (wt_tx, wt) = tokio::sync::mpsc::channel(4);
    let udp = tokio::spawn(async move {
        while let Some(incoming) = endpoint.accept().await {
            let service = service.clone();
            let wt_tx = wt_tx.clone();
            tokio::spawn(async move {
                let Ok(conn) = incoming.await else { return };
                match accept(conn).await {
                    Ok(Accepted::Http(connection)) => service.serve(connection).await,
                    Ok(Accepted::WebTransport(request)) => {
                        let _ = wt_tx.send(request).await;
                    }
                    Err(error) => tracing::debug!(%error, "test h3 accept failed"),
                }
            });
        }
    });
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert.der().clone()).unwrap();
    let client_tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    (
        Fixture {
            port: address.port(),
            certificate: cert.der().clone(),
            client_tls: Arc::new(client_tls),
            stop,
            tcp,
            udp,
            wt,
            _assets: assets,
        },
        receiver,
    )
}

impl Fixture {
    async fn h3(&self) -> quinn::Connection {
        let mut tls = (*self.client_tls).clone();
        tls.alpn_protocols = vec![b"h3".to_vec()];
        let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(tls).unwrap(),
        )));
        let conn = endpoint
            .connect(
                format!("127.0.0.1:{}", self.port).parse().unwrap(),
                "localhost",
            )
            .unwrap()
            .await
            .unwrap();
        let mut control = conn.open_uni().await.unwrap();
        let mut settings = Vec::new();
        Settings::default().encode(&mut settings);
        control.write_all(&settings).await.unwrap();
        // Keep the client control stream open for the connection lifetime.
        std::mem::forget(control);
        conn
    }
    async fn finish(self) {
        let _ = self.stop.send(true);
        self.tcp.await.unwrap().unwrap();
        self.udp.abort();
    }
}

struct Reply {
    status: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

async fn send(
    conn: &quinn::Connection,
    fields: &[(&str, &str)],
    body: &[u8],
) -> Result<Reply, String> {
    let (mut tx, mut rx) = conn.open_bi().await.map_err(|e| e.to_string())?;
    let mut block = BytesMut::new();
    // Literal encoding, one field line each, duplicates kept (cookie crumbs).
    let mut headers = QpackHeaders::default();
    let mut literal = Vec::new();
    for (name, value) in fields {
        if name.starts_with(':') {
            headers.set(name, value);
        } else {
            literal.push((*name, *value));
        }
    }
    headers.encode(&mut block);
    for (name, value) in literal {
        // Literal field line with literal name: 001N H NameLen(3+), H ValueLen(7+).
        prefix(&mut block, 3, 0b0010_0000, name.len());
        block.extend_from_slice(name.as_bytes());
        prefix(&mut block, 7, 0, value.len());
        block.extend_from_slice(value.as_bytes());
    }
    let mut out = Vec::new();
    put_varint(&mut out, FRAME_HEADERS);
    put_varint(&mut out, block.len() as u64);
    out.extend_from_slice(&block);
    if !body.is_empty() {
        put_varint(&mut out, FRAME_DATA);
        put_varint(&mut out, body.len() as u64);
        out.extend_from_slice(body);
    }
    tx.write_all(&out).await.map_err(|e| e.to_string())?;
    tx.finish().map_err(|e| e.to_string())?;
    let raw = rx
        .read_to_end(4 * 1024 * 1024)
        .await
        .map_err(|e| e.to_string())?;
    let mut rest = raw.as_slice();
    let mut reply = Reply {
        status: String::new(),
        headers: Vec::new(),
        body: Vec::new(),
    };
    while !rest.is_empty() {
        let (kind, len) = (take_varint(&mut rest), take_varint(&mut rest) as usize);
        let (payload, tail) = rest.split_at(len);
        rest = tail;
        match kind {
            FRAME_HEADERS => {
                reply.headers = QpackHeaders::decode_fields(&mut &payload[..]).unwrap();
                reply.status = reply
                    .headers
                    .iter()
                    .find(|(n, _)| n == ":status")
                    .map(|(_, v)| v.clone())
                    .unwrap();
            }
            FRAME_DATA => reply.body.extend_from_slice(payload),
            _ => {}
        }
    }
    Ok(reply)
}

/// RFC 7541 5.1 prefix integer with `flags` in the high bits of the first byte.
fn prefix(out: &mut BytesMut, bits: u8, flags: u8, value: usize) {
    let max = (1usize << bits) - 1;
    if value < max {
        out.extend_from_slice(&[flags | value as u8]);
        return;
    }
    out.extend_from_slice(&[flags | max as u8]);
    let mut rest = value - max;
    while rest >= 128 {
        out.extend_from_slice(&[(rest % 128) as u8 | 0x80]);
        rest /= 128;
    }
    out.extend_from_slice(&[rest as u8]);
}

fn take_varint(rest: &mut &[u8]) -> u64 {
    let extra = (1usize << (rest[0] >> 6)) - 1;
    let mut value = u64::from(rest[0] & 0x3f);
    for byte in &rest[1..=extra] {
        value = (value << 8) | u64::from(*byte);
    }
    *rest = &rest[1 + extra..];
    value
}

fn header<'a>(reply: &'a Reply, name: &str) -> Option<&'a str> {
    reply
        .headers
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.as_str())
}

#[tokio::test]
async fn h3_state_request_reaches_the_same_owner_queue_with_identical_bytes_and_proof() {
    let (fixture, mut receiver) = start().await;
    let conn = fixture.h3().await;
    let authority = format!("localhost:{}", fixture.port);
    let fields = [
        (":method", "POST"),
        (":scheme", "https"),
        (":authority", authority.as_str()),
        (":path", "/peer/v3/state"),
        ("x-tilo-delivery-proof", "0011"),
    ];
    let client = send(&conn, &fields, b"binary\0body");
    let owner = async {
        let request = timeout(T, receiver.recv()).await.unwrap().unwrap();
        assert_eq!(request.method(), Method::POST);
        assert_eq!(request.route(), Route::State);
        assert_eq!(request.target(), "/peer/v3/state");
        assert_eq!(request.body(), b"binary\0body");
        assert_eq!(request.proof(), Some(&[0, 17][..]));
        request
            .respond(StatusCode::OK, b"receipt".to_vec())
            .unwrap();
    };
    let (reply, ()) = tokio::join!(client, owner);
    let reply = reply.unwrap();
    assert_eq!(reply.status, "200");
    assert_eq!(reply.body, b"receipt");
    assert_eq!(header(&reply, "cache-control"), Some("no-store"));
    assert_eq!(
        header(&reply, "alt-svc"),
        None,
        "HTTP/3 responses do not re-advertise"
    );
    fixture.finish().await;
}

#[tokio::test]
async fn h3_serves_assets_and_held_polls_concurrently_on_one_connection() {
    let (fixture, mut receiver) = start().await;
    let conn = fixture.h3().await;
    let authority = format!("localhost:{}", fixture.port);
    let poll = {
        let conn = conn.clone();
        let authority = authority.clone();
        tokio::spawn(async move {
            send(
                &conn,
                &[
                    (":method", "GET"),
                    (":scheme", "https"),
                    (":authority", &authority),
                    (":path", "/peer/v3/events?mode=poll"),
                    ("x-tilo-delivery-proof", "00"),
                ],
                b"",
            )
            .await
        })
    };
    let held = timeout(T, receiver.recv()).await.unwrap().unwrap();
    assert_eq!(held.route(), Route::EventsPoll);
    let asset = send(
        &conn,
        &[
            (":method", "GET"),
            (":scheme", "https"),
            (":authority", &authority),
            (":path", "/"),
        ],
        b"",
    )
    .await
    .unwrap();
    assert_eq!(asset.status, "200");
    assert_eq!(asset.body, b"real asset bytes");
    held.respond(StatusCode::OK, Vec::new()).unwrap();
    let poll = timeout(T, poll).await.unwrap().unwrap().unwrap();
    assert_eq!(poll.status, "200");
    assert!(poll.body.is_empty());
    fixture.finish().await;
}

#[tokio::test]
async fn h3_applies_the_listener_origin_and_request_checks() {
    let (fixture, mut receiver) = start().await;
    let conn = fixture.h3().await;
    let authority = format!("localhost:{}", fixture.port);
    for (fields, status) in [
        // Foreign authority: denied exactly as over HTTP/2.
        (
            vec![
                (":method", "POST"),
                (":scheme", "https"),
                (":authority", "example.test"),
                (":path", "/peer/v3/state"),
                ("x-tilo-delivery-proof", "00"),
            ],
            "403",
        ),
        // Missing proof on a data route.
        (
            vec![
                (":method", "POST"),
                (":scheme", "https"),
                (":authority", authority.as_str()),
                (":path", "/peer/v3/state"),
            ],
            "401",
        ),
        // Missing :path pseudo-header: malformed.
        (
            vec![
                (":method", "GET"),
                (":scheme", "https"),
                (":authority", authority.as_str()),
            ],
            "400",
        ),
        // Wrong scheme.
        (
            vec![
                (":method", "GET"),
                (":scheme", "http"),
                (":authority", authority.as_str()),
                (":path", "/"),
            ],
            "400",
        ),
    ] {
        let reply = send(&conn, &fields, b"x").await.unwrap();
        assert_eq!(reply.status, status, "{fields:?}");
    }
    // Oversized body never reaches the owner queue.
    let big = vec![0u8; BODY_BYTES + 1];
    let reply = send(
        &conn,
        &[
            (":method", "POST"),
            (":scheme", "https"),
            (":authority", &authority),
            (":path", "/peer/v3/state"),
            ("x-tilo-delivery-proof", "00"),
        ],
        &big,
    )
    .await;
    assert!(reply.is_err() || reply.unwrap().status != "200");
    assert!(
        receiver.try_recv().is_none(),
        "no rejected request reached the owner"
    );
    fixture.finish().await;
}

#[test]
fn split_cookie_crumbs_are_rejoined_and_pseudo_headers_are_strict() {
    let fields = |extra: &[(&str, &str)]| {
        let mut all = vec![
            (":method".to_string(), "GET".to_string()),
            (":scheme".into(), "https".into()),
            (":authority".into(), "localhost:1".into()),
            (":path".into(), "/".into()),
        ];
        all.extend(extra.iter().map(|(n, v)| (n.to_string(), v.to_string())));
        all
    };
    let head = request_head(fields(&[("cookie", "a=1"), ("cookie", "b=2")]))
        .unwrap()
        .unwrap()
        .body(())
        .unwrap();
    assert_eq!(head.headers().get("cookie").unwrap(), "a=1; b=2");
    assert_eq!(head.version(), Version::HTTP_3);
    assert_eq!(head.uri(), "https://localhost:1/");
    for bad in [
        vec![(":protocol", "webtransport")],
        vec![("connection", "close")],
        vec![("Upper", "x")],
        vec![(":path", "/again")],
    ] {
        assert!(request_head(fields(&bad)).unwrap().is_err(), "{bad:?}");
    }
    // A pseudo-header after a regular field is malformed.
    let mut late = fields(&[("accept", "*/*")]);
    late.push((":status".into(), "200".into()));
    assert!(request_head(late).unwrap().is_err());
}

#[tokio::test]
async fn webtransport_connect_on_the_shared_endpoint_is_handed_over_unchanged() {
    let (mut fixture, _receiver) = start().await;
    let client = web_transport_quinn::ClientBuilder::new()
        .with_server_certificates(vec![fixture.certificate.clone()])
        .unwrap();
    // `localhost` may resolve to ::1 first; the test endpoint is IPv4.
    let url: url::Url = format!("https://127.0.0.1:{}/moq/v21/voice", fixture.port)
        .parse()
        .unwrap();
    let connect = tokio::spawn(async move {
        client
            .connect(web_transport_quinn::proto::ConnectRequest::new(url).with_protocol("moqt-21"))
            .await
    });
    let request = timeout(T, fixture.wt.recv()).await.unwrap().unwrap();
    assert_eq!(request.raw_target(), Some("/moq/v21/voice"));
    assert!(request.protocols.iter().any(|p| p == "moqt-21"));
    let _session = request
        .respond(web_transport_quinn::proto::ConnectResponse::OK.with_protocol("moqt-21"))
        .await
        .unwrap();
    let session = timeout(T, connect).await.unwrap().unwrap().unwrap();
    assert_eq!(session.protocol(), Some("moqt-21"));
    fixture.finish().await;
}

#[tokio::test]
async fn h2_responses_advertise_h3_and_keep_serving_when_udp_is_unused() {
    let (fixture, mut receiver) = start().await;
    let mut tls = (*fixture.client_tls).clone();
    tls.alpn_protocols = vec![b"h2".to_vec()];
    let socket = tokio_rustls::TlsConnector::from(Arc::new(tls))
        .connect(
            ServerName::try_from("localhost").unwrap(),
            tokio::net::TcpStream::connect(("127.0.0.1", fixture.port))
                .await
                .unwrap(),
        )
        .await
        .unwrap();
    let (mut sender, connection) = hyper::client::conn::http2::handshake(
        hyper_util::rt::TokioExecutor::new(),
        hyper_util::rt::TokioIo::new(socket),
    )
    .await
    .unwrap();
    let connection = tokio::spawn(connection);
    let state = sender.send_request(
        Request::builder()
            .method("POST")
            .uri(format!("https://localhost:{}/peer/v3/state", fixture.port))
            .header("x-tilo-delivery-proof", "00")
            .body(http_body_util::Full::new(Bytes::from_static(b"item")))
            .unwrap(),
    );
    let owner = async {
        let request = timeout(T, receiver.recv()).await.unwrap().unwrap();
        assert_eq!(request.body(), b"item");
        request.respond(StatusCode::OK, b"ok".to_vec()).unwrap();
    };
    let (response, ()) = tokio::join!(state, owner);
    let response = response.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("alt-svc").unwrap(),
        &format!("h3=\":{}\"; ma=3600", fixture.port)
    );
    let mut body = Vec::new();
    let mut stream = response.into_body();
    while let Some(frame) = stream.frame().await {
        if let Ok(data) = frame.unwrap().into_data() {
            body.extend_from_slice(&data);
        }
    }
    assert_eq!(body, b"ok");
    fixture.finish().await;
    let _ = connection.await;
}
