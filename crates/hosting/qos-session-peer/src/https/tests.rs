use super::*;
use http_body_util::{BodyExt, Full};
use hyper::{Method, Request, StatusCode};
use rustls::pki_types::{PrivatePkcs8KeyDer, ServerName};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub(crate) fn asset_test_lease() -> Arc<Lease> {
    ingress_channel().0.reserve(0, Lane::Asset).unwrap()
}

// Gate the actual Tokio filesystem work queue, not a mock filesystem or a
// production test hook. Drop always releases the worker, including on panic.
struct BlockingPoolGate {
    release: Option<std::sync::mpsc::Sender<()>>,
    worker: tokio::task::JoinHandle<()>,
}
impl BlockingPoolGate {
    async fn occupy() -> Self {
        let (entered, ready) = oneshot::channel();
        let (release, released) = std::sync::mpsc::channel();
        let worker = tokio::task::spawn_blocking(move || {
            let _ = entered.send(());
            let _ = released.recv();
        });
        ready.await.unwrap();
        Self {
            release: Some(release),
            worker,
        }
    }
    async fn finish(mut self) {
        let _ = self.release.take().unwrap().send(());
        (&mut self.worker).await.unwrap();
    }
}
impl Drop for BlockingPoolGate {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
    }
}

#[test]
fn cancelled_http_asset_resolution_keeps_queued_worker_charged() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap()
        .block_on(async {
            let (mut fixture, receiver) = Fixture::start().await;
            let mut client = fixture.connect(false).await;
            let gate = BlockingPoolGate::occupy().await;
            client
                .write_all(
                    format!(
                        "GET / HTTP/1.1\r\nHost: localhost:{}\r\nConnection: close\r\n\r\n",
                        fixture.address.port()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            timeout(Duration::from_secs(2), async {
                while receiver.available_bytes()
                    != receiver.initial_bytes - CONNECTION_BYTES - (4 * IO_BYTES + HEADER_BYTES)
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let _ = fixture.stop.send(true);
            (&mut fixture.task).await.unwrap().unwrap();
            let retained_bytes = receiver.available_bytes();
            let retained_slots = fixture.ingress.asset_requests.available_permits();
            gate.finish().await;
            timeout(Duration::from_secs(2), async {
                while receiver.available_bytes() != receiver.initial_bytes {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert_eq!(
                retained_bytes,
                receiver.initial_bytes - (4 * IO_BYTES + HEADER_BYTES),
                "cancelled HTTP resolve detached uncharged filesystem work"
            );
            assert_eq!(retained_slots, 23);
            assert_eq!(fixture.ingress.asset_requests.available_permits(), 24);
        });
}

#[test]
fn cancelled_asset_read_keeps_queued_file_and_buffer_work_charged() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap()
        .block_on(async {
            let directory = tempfile::tempdir().unwrap();
            std::fs::write(directory.path().join("read.js"), vec![0x61; IO_BYTES]).unwrap();
            let (ingress, receiver) = ingress_channel();
            let root = AssetRoot::open(directory.path()).unwrap();
            let lease = ingress.reserve(0, Lane::Asset).unwrap();
            let asset = root
                .resolve("/read.js", false, lease.clone())
                .await
                .unwrap();
            let mut body = ResponseBody::file(asset.file, asset.length, lease);
            let gate = BlockingPoolGate::occupy().await;
            let (started, queued) = oneshot::channel();
            let consumer = tokio::spawn(async move {
                let _ = started.send(());
                let _ = body.frame().await;
            });
            queued.await.unwrap(); // Same-thread task has polled the real read before yielding.
            consumer.abort();
            assert!(consumer.await.unwrap_err().is_cancelled());
            let retained_bytes = receiver.available_bytes();
            let retained_slots = ingress.asset_requests.available_permits();
            gate.finish().await;
            timeout(Duration::from_secs(2), async {
                while receiver.available_bytes() != receiver.initial_bytes {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert_eq!(
                retained_bytes,
                receiver.initial_bytes - (4 * IO_BYTES + HEADER_BYTES),
                "cancelled body detached uncharged file/buffer work"
            );
            assert_eq!(retained_slots, 23);
            assert_eq!(ingress.asset_requests.available_permits(), 24);
        });
}

#[tokio::test]
async fn h2_slow_reader_keeps_response_backing_charged_until_drain_or_reset() {
    for reset in [false, true] {
        let (fixture, mut receiver) = Fixture::start().await;
        let socket = fixture.connect(true).await;
        let (mut sender, connection) =
            hyper::client::conn::http2::Builder::new(TokioExecutor::new())
                .initial_stream_window_size(4096)
                .handshake(TokioIo::new(socket))
                .await
                .unwrap();
        let connection = tokio::spawn(connection);
        let request = sender.send_request(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "https://localhost:{}/peer/v3/challenge",
                    fixture.address.port()
                ))
                .body(Full::new(Bytes::from_static(b"inert")))
                .unwrap(),
        );
        let request = tokio::spawn(request);
        let inert = receiver.recv().await.unwrap();
        let lease = Arc::downgrade(&inert.lease);
        let charged = receiver.available_bytes();
        inert
            .respond(StatusCode::SERVICE_UNAVAILABLE, vec![0x6a; BODY_BYTES])
            .unwrap();
        let response = request.await.unwrap().unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let mut body = response.into_body();
        let first = body.frame().await.unwrap().unwrap().into_data().unwrap();
        assert_eq!(first.len(), 4096);
        assert!(
            lease.upgrade().is_some(),
            "H2 still has the reply backing but its request/byte lease retired"
        );
        assert_eq!(receiver.available_bytes(), charged);
        if reset {
            drop(body);
        } else {
            let rest = timeout(Duration::from_secs(3), body.collect())
                .await
                .unwrap()
                .unwrap()
                .to_bytes();
            assert_eq!(first.len() + rest.len(), BODY_BYTES);
            assert!(rest.iter().all(|byte| *byte == 0x6a));
        }
        timeout(Duration::from_secs(2), async {
            while lease.upgrade().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            receiver.available_bytes(),
            receiver.initial_bytes - CONNECTION_BYTES
        );
        fixture.finish().await;
        assert_eq!(receiver.available_bytes(), receiver.initial_bytes);
        drop(sender);
        let _ = connection.await;
    }
}

#[tokio::test]
async fn response_rejects_a_small_view_of_an_oversized_backing() {
    let (fixture, mut receiver) = Fixture::start().await;
    let client = fixture.raw("/peer/v3/challenge", "", b"inert");
    let actor = async {
        let request = receiver.recv().await.unwrap();
        let mut backing = vec![0; 2 * BODY_BYTES];
        backing.truncate(1);
        assert!(
            request
                .respond(StatusCode::SERVICE_UNAVAILABLE, backing)
                .is_err()
        );
    };
    let (response, _) = tokio::join!(client, actor);
    assert!(response.starts_with("HTTP/1.1 503"));
    fixture.finish().await;
}

#[tokio::test]
async fn h2_final_static_frame_retains_its_asset_charge_until_drain_or_reset() {
    for reset in [false, true] {
        let (fixture, receiver) = Fixture::start().await;
        std::fs::write(
            fixture._assets.path().join("chunk.js"),
            vec![0x78; IO_BYTES],
        )
        .unwrap();
        let (mut sender, connection) =
            hyper::client::conn::http2::Builder::new(TokioExecutor::new())
                .initial_stream_window_size(4096)
                .handshake(TokioIo::new(fixture.connect(true).await))
                .await
                .unwrap();
        let connection = tokio::spawn(connection);
        let response = sender
            .send_request(
                Request::builder()
                    .uri(format!(
                        "https://localhost:{}/chunk.js",
                        fixture.address.port()
                    ))
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let mut body = response.into_body();
        let first = body.frame().await.unwrap().unwrap().into_data().unwrap();
        assert_eq!(first.len(), 4096);
        assert_eq!(
            receiver.available_bytes(),
            receiver.initial_bytes - CONNECTION_BYTES - (4 * IO_BYTES + HEADER_BYTES)
        );
        if reset {
            drop(body);
        } else {
            let rest = body.collect().await.unwrap().to_bytes();
            assert_eq!(first.len() + rest.len(), IO_BYTES);
            assert!(rest.iter().all(|byte| *byte == 0x78));
        }
        timeout(Duration::from_secs(2), async {
            while receiver.available_bytes() != receiver.initial_bytes - CONNECTION_BYTES {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        fixture.finish().await;
        assert_eq!(receiver.available_bytes(), receiver.initial_bytes);
        drop(sender);
        let _ = connection.await;
    }
}

#[tokio::test]
async fn raw_tls_rejects_all_transfer_codings_and_conflicting_lengths_before_queueing() {
    let (fixture, mut receiver) = Fixture::start().await;
    let cases = [
        "Transfer-Encoding: chunked\r\n",
        "Transfer-Encoding: chunked\r\nContent-Length: 999\r\n",
        "Content-Length: 999\r\nTransfer-Encoding: chunked\r\n",
        "Transfer-Encoding: gzip, chunked\r\n",
        "Transfer-Encoding: gzip, chunked\r\nContent-Length: 999\r\n",
        "Content-Length: 999\r\nTransfer-Encoding: gzip, chunked\r\n",
        "Content-Length: 1\r\nContent-Length: 2\r\n",
    ];
    for headers in cases {
        let wire_body: &[u8] = if headers.contains("Transfer-Encoding") {
            b"1\r\nx\r\n0\r\n\r\n"
        } else {
            b"x"
        };
        tokio::select! {
            response = fixture.raw_framing(headers, wire_body) => assert!(response.starts_with("HTTP/1.1 400"), "{headers:?}: {response}"),
            request = receiver.recv() => panic!("forbidden wire framing reached the mailbox: {:?}", request.map(|r| r.route())),
        }
    }
    assert!(receiver.try_recv().is_none());
    fixture.finish().await;
}

#[tokio::test]
async fn raw_tls_identical_duplicate_content_length_is_the_documented_normalization_exception() {
    let (fixture, mut receiver) = Fixture::start().await;
    let client = fixture.raw_framing("Content-Length: 1\r\nContent-Length: 1\r\n", b"x");
    let actor = async {
        let request = receiver.recv().await.unwrap();
        assert_eq!(request.body(), b"x");
        request
            .respond(
                StatusCode::SERVICE_UNAVAILABLE,
                b"no owner installed".to_vec(),
            )
            .unwrap();
    };
    let (response, _) = tokio::join!(client, actor);
    assert!(response.starts_with("HTTP/1.1 503"));
    assert!(receiver.try_recv().is_none());
    fixture.finish().await;
}

#[test]
fn bulk_reservations_cannot_exhaust_control_capacity() {
    let (ingress, receiver) = ingress_channel();
    let mut bulk = Vec::new();
    while let Ok(lease) = ingress.reserve(BODY_BYTES, Lane::Data) {
        bulk.push(lease);
    }
    assert!(!bulk.is_empty());
    let control = ingress.reserve(0, Lane::Control);
    assert!(
        control.is_ok(),
        "bulk exhausted the reserved control service"
    );
    drop(control);
    drop(bulk);
    assert_eq!(receiver.available_bytes(), receiver.initial_bytes);
}

#[test]
fn worst_configured_lanes_leave_control_memory_with_all_connection_reservations() {
    let (ingress, receiver) = ingress_channel();
    let connections = ingress
        .bytes
        .clone()
        .try_acquire_bulk(64 * CONNECTION_BYTES)
        .unwrap();
    let bulk: Vec<_> = (0..32)
        .map(|_| ingress.reserve(BODY_BYTES, Lane::Data).unwrap())
        .collect();
    let assets: Vec<_> = (0..24)
        .map(|_| ingress.reserve(0, Lane::Asset).unwrap())
        .collect();
    assert!(ingress.reserve(BODY_BYTES, Lane::Data).is_err());
    assert!(ingress.reserve(0, Lane::Asset).is_err());
    let controls: Vec<_> = (0..8)
        .map(|_| ingress.reserve(AUTH_BYTES, Lane::Control).unwrap())
        .collect();
    assert!(receiver.available_bytes() > 0);
    assert!(ingress.reserve(AUTH_BYTES, Lane::Control).is_err());
    drop((connections, bulk, assets, controls));
    assert_eq!(receiver.available_bytes(), receiver.initial_bytes);
}

// Raw test frames exercise the actual locked h2 decoder; production has no H2 parser.
async fn h2_frame(
    socket: &mut tokio_rustls::client::TlsStream<tokio::net::TcpStream>,
    kind: u8,
    flags: u8,
    stream: u32,
    payload: &[u8],
) {
    let length = payload.len() as u32;
    let mut header = Vec::from(&length.to_be_bytes()[1..]);
    header.extend([kind, flags]);
    header.extend(stream.to_be_bytes());
    socket.write_all(&header).await.unwrap();
    socket.write_all(payload).await.unwrap();
}
async fn h2_read_frame(
    socket: &mut tokio_rustls::client::TlsStream<tokio::net::TcpStream>,
) -> (u8, u8, u32, Vec<u8>) {
    let mut header = [0; 9];
    timeout(Duration::from_secs(2), socket.read_exact(&mut header))
        .await
        .unwrap()
        .unwrap();
    let length = u32::from_be_bytes([0, header[0], header[1], header[2]]) as usize;
    assert!(length <= 16384);
    let mut body = vec![0; length];
    socket.read_exact(&mut body).await.unwrap();
    (
        header[3],
        header[4],
        u32::from_be_bytes(header[5..9].try_into().unwrap()),
        body,
    )
}

#[tokio::test]
async fn stalled_h2_headers_leave_control_lane_available_and_continuation_flood_is_closed() {
    let (fixture, mut receiver) = Fixture::start().await;
    let mut stalled = fixture.connect(true).await;
    stalled
        .write_all(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n")
        .await
        .unwrap();
    h2_frame(&mut stalled, 4, 0, 0, &[]).await;
    let authority = format!("localhost:{}", fixture.address.port());
    let mut headers = vec![0x82, 0x87, 0x84, 0x01, authority.len() as u8];
    headers.extend(authority.as_bytes());
    h2_frame(&mut stalled, 1, 5, 1, &headers).await;
    loop {
        let (kind, flags, stream, _) = h2_read_frame(&mut stalled).await;
        if kind == 0 && stream == 1 && flags & 1 == 1 {
            break;
        }
    }
    h2_frame(&mut stalled, 1, 1, 3, &headers).await; // No END_HEADERS.
    let response = fixture.raw("/peer/v3/challenge", "", b"challenge bytes");
    let control = async {
        let request = timeout(Duration::from_secs(1), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(request.route(), Route::Challenge);
        request
            .respond(
                StatusCode::SERVICE_UNAVAILABLE,
                b"no owner installed".to_vec(),
            )
            .unwrap();
    };
    let (response, _) = tokio::join!(response, control);
    assert!(response.starts_with("HTTP/1.1 503"));
    for _ in 0..6 {
        h2_frame(&mut stalled, 9, 0, 3, &[]).await;
    }
    loop {
        let (kind, _, _, body) = h2_read_frame(&mut stalled).await;
        if kind == 7 {
            assert_eq!(u32::from_be_bytes(body[4..8].try_into().unwrap()), 11); // ENHANCE_YOUR_CALM
            break;
        }
    }
    drop(stalled);
    fixture.finish().await;
    assert_eq!(receiver.available_bytes(), receiver.initial_bytes);
}

#[tokio::test]
async fn missing_first_h2_header_has_finite_deadline() {
    let (fixture, _) = Fixture::start().await;
    let mut socket = fixture.connect(true).await;
    socket
        .write_all(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n")
        .await
        .unwrap();
    h2_frame(&mut socket, 4, 0, 0, &[]).await;
    let mut bytes = Vec::new();
    let ended = timeout(Duration::from_secs(7), socket.read_to_end(&mut bytes)).await;
    assert!(
        ended.is_ok(),
        "incomplete initial H2 headers outlived first-header deadline"
    );
    fixture.finish().await;
}

#[tokio::test]
async fn normal_tls_rejects_unknown_issuer_and_wrong_hostname() {
    let (fixture, _) = Fixture::start().await;
    let untrusted = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(rustls::RootCertStore::empty())
    .with_no_client_auth();
    for (config, hostname) in [
        (Arc::new(untrusted), "localhost"),
        (fixture.tls.clone(), "other.test"),
    ] {
        let socket = tokio::net::TcpStream::connect(fixture.address)
            .await
            .unwrap();
        let result = tokio_rustls::TlsConnector::from(config)
            .connect(ServerName::try_from(hostname).unwrap(), socket)
            .await;
        assert!(
            result.is_err(),
            "untrusted or wrong-name TLS certificate was accepted"
        );
    }
    fixture.finish().await;
}

#[tokio::test]
async fn slow_body_and_unanswered_control_expire_and_release_admission() {
    let (fixture, mut receiver) = Fixture::start().await;
    let mut slow = fixture.connect(false).await;
    slow.write_all(
        format!(
            "POST /peer/v3/challenge HTTP/1.1\r\nHost: localhost:{}\r\nContent-Length: 3\r\n\r\na",
            fixture.address.port()
        )
        .as_bytes(),
    )
    .await
    .unwrap();
    let mut waiting = fixture.connect(false).await;
    waiting.write_all(format!("POST /peer/v3/challenge HTTP/1.1\r\nHost: localhost:{}\r\nContent-Length: 1\r\nConnection: close\r\n\r\na", fixture.address.port()).as_bytes()).await.unwrap();
    let pending = receiver.recv().await.unwrap();
    let read = async |mut socket: tokio_rustls::client::TlsStream<tokio::net::TcpStream>| {
        let mut bytes = Vec::new();
        timeout(Duration::from_secs(7), socket.read_to_end(&mut bytes))
            .await
            .unwrap()
            .unwrap();
        String::from_utf8(bytes).unwrap()
    };
    let (slow_response, waiting_response) = tokio::join!(read(slow), read(waiting));
    assert!(slow_response.starts_with("HTTP/1.1 400"));
    assert!(waiting_response.starts_with("HTTP/1.1 503"));
    assert!(pending.is_cancelled());
    assert!(pending.respond(StatusCode::OK, Vec::new()).is_err());
    assert!(receiver.try_recv().is_none());
    fixture.finish().await;
    assert_eq!(receiver.available_bytes(), receiver.initial_bytes);
}

// Removing exact origin, proof, target or body validation must change these outcomes.
#[test]
fn rejects_ambient_credentials_and_noncanonical_delivery_requests() {
    let origin = Origin::parse("https://localhost:9443").unwrap();
    let cases = [
        ("POST", "/peer/v3/state", "cookie", "secret", 400),
        (
            "POST",
            "/peer/v3/state",
            "authorization",
            "Bearer secret",
            400,
        ),
        (
            "POST",
            "/peer/v3/state",
            "origin",
            "https://elsewhere.test",
            403,
        ),
        ("POST", "/peer/v3/state", "content-encoding", "gzip", 400),
        ("POST", "/peer/v3/state", "x-tilo-delivery-proof", "AB", 400),
        (
            "GET",
            "/peer/v3/events?mode=poll&cursor=a",
            "x-tilo-delivery-proof",
            "00",
            400,
        ),
        (
            "GET",
            "/peer/v3/events?mode=poll&mode=poll",
            "x-tilo-delivery-proof",
            "00",
            400,
        ),
        ("POST", "/peer/v3/state", "host", "elsewhere.test", 403),
        (
            "OPTIONS",
            "/peer/v3/state",
            "origin",
            "https://localhost:9443",
            400,
        ),
    ];
    for (method, target, name, value, expected) in cases {
        let mut request = Request::builder().method(method).uri(target);
        if name != "host" {
            request = request.header("host", "localhost:9443");
        }
        let (parts, _) = request.header(name, value).body(()).unwrap().into_parts();
        assert_eq!(
            parse_head(&parts, &origin).unwrap_err().status.as_u16(),
            expected,
            "{method} {target} {name}"
        );
    }
}

#[test]
fn accepts_only_fixed_routes_and_preserves_exact_signed_target() {
    let origin = Origin::parse("https://localhost:9443").unwrap();
    let request = Request::builder()
        .uri("/peer/v3/events?cursor=abcdef&mode=poll")
        .header("host", "localhost:9443")
        .header("x-tilo-delivery-proof", "0011")
        .body(())
        .unwrap();
    let (parts, _) = request.into_parts();
    let parsed = parse_head(&parts, &origin).unwrap();
    assert_eq!(parsed.target, "/peer/v3/events?cursor=abcdef&mode=poll");
    assert_eq!(parsed.proof.as_deref(), Some(&[0, 17][..]));
    assert_eq!(parsed.route, Route::EventsPoll);
    for path in [
        "/peer/v3/missing",
        "/peer/v3/connect",
        "/peer/v3/state/extra",
        "/qos-peer/enrollment/challenge",
    ] {
        let request = Request::builder()
            .uri(path)
            .header("host", "localhost:9443")
            .body(())
            .unwrap();
        assert!(
            parse_head(&request.into_parts().0, &origin).is_err(),
            "{path}"
        );
    }
}

#[test]
fn header_and_cursor_limits_fail_before_any_mailbox_admission() {
    let origin = Origin::parse("https://localhost:9443").unwrap();
    let mut request = Request::builder()
        .uri("/peer/v3/events?mode=poll")
        .header("host", "localhost:9443")
        .header("x-tilo-delivery-proof", "00");
    for index in 0..33 {
        request = request.header(format!("x-extra-{index}"), "v");
    }
    assert_eq!(
        parse_head(&request.body(()).unwrap().into_parts().0, &origin)
            .unwrap_err()
            .status,
        StatusCode::BAD_REQUEST
    );
    let request = Request::builder()
        .uri(format!(
            "/peer/v3/events?cursor={}&mode=poll",
            "a".repeat(8194)
        ))
        .header("host", "localhost:9443")
        .header("x-tilo-delivery-proof", "00")
        .body(())
        .unwrap();
    assert_eq!(
        parse_head(&request.into_parts().0, &origin)
            .unwrap_err()
            .status,
        StatusCode::BAD_REQUEST
    );
}

struct Fixture {
    address: std::net::SocketAddr,
    tls: Arc<rustls::ClientConfig>,
    stop: tokio::sync::watch::Sender<bool>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
    _assets: tempfile::TempDir,
    ingress: Ingress,
}
impl Fixture {
    async fn start() -> (Self, IngressReceiver) {
        Self::start_with_enrollment(None).await
    }
    async fn start_with_enrollment(
        enrollment: Option<(
            Arc<crate::enrollment_http::Authorization>,
            mpsc::Sender<crate::enrollment_http::EnrollmentCommand>,
        )>,
    ) -> (Self, IngressReceiver) {
        let assets = tempfile::tempdir().unwrap();
        std::fs::write(assets.path().join("index.html"), b"real asset bytes").unwrap();
        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert.der().clone()).unwrap();
        let mut client = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
        client.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        let (ingress, receiver) = ingress_channel();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let adapter = HttpsAdapter::new(
            listener,
            &format!("https://localhost:{}", address.port()),
            crate::https_assets::AssetRoot::open(assets.path()).unwrap(),
            vec![cert.der().clone()],
            PrivatePkcs8KeyDer::from(signing_key.serialize_der()).into(),
            ingress.clone(),
        )
        .unwrap();
        let adapter = if let Some((authorization, commands)) = enrollment {
            adapter
                .with_local_enrollment(Arc::new(
                    LocalEnrollment::new(
                        &format!("https://localhost:{}", address.port()),
                        authorization,
                        commands,
                    )
                    .unwrap(),
                ))
                .unwrap()
        } else {
            adapter
        };
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(adapter.serve(stopped));
        (
            Self {
                address,
                tls: Arc::new(client),
                stop,
                task,
                _assets: assets,
                ingress,
            },
            receiver,
        )
    }
    async fn connect(&self, h2: bool) -> tokio_rustls::client::TlsStream<tokio::net::TcpStream> {
        let mut config = (*self.tls).clone();
        config.alpn_protocols = vec![if h2 {
            b"h2".to_vec()
        } else {
            b"http/1.1".to_vec()
        }];
        tokio_rustls::TlsConnector::from(Arc::new(config))
            .connect(
                ServerName::try_from("localhost").unwrap(),
                tokio::net::TcpStream::connect(self.address).await.unwrap(),
            )
            .await
            .unwrap()
    }
    async fn raw(&self, path: &str, extra: &str, body: &[u8]) -> String {
        let mut socket = self.connect(false).await;
        socket.write_all(format!("POST {path} HTTP/1.1\r\nHost: localhost:{}\r\nConnection: close\r\nContent-Length: {}\r\n{extra}\r\n", self.address.port(), body.len()).as_bytes()).await.unwrap();
        socket.write_all(body).await.unwrap();
        let mut output = Vec::new();
        tokio::time::timeout(Duration::from_secs(3), socket.read_to_end(&mut output))
            .await
            .unwrap()
            .unwrap();
        String::from_utf8(output).unwrap()
    }
    async fn raw_framing(&self, headers: &str, body: &[u8]) -> String {
        let mut socket = self.connect(false).await;
        socket.write_all(format!("POST /peer/v3/challenge HTTP/1.1\r\nHost: localhost:{}\r\nConnection: close\r\n{headers}\r\n", self.address.port()).as_bytes()).await.unwrap();
        socket.write_all(body).await.unwrap();
        let mut output = Vec::new();
        timeout(Duration::from_secs(3), socket.read_to_end(&mut output))
            .await
            .unwrap()
            .unwrap();
        String::from_utf8(output).unwrap()
    }
    async fn finish(self) {
        let _ = self.stop.send(true);
        self.task.await.unwrap().unwrap();
    }
}

// A transport-only test receiver returns an explicit rejection, never a durable receipt.
#[tokio::test]
async fn normal_tls_http1_forwards_exact_inert_bytes_and_releases_budget() {
    let (fixture, mut receiver) = Fixture::start().await;
    let client = fixture.raw(
        "/peer/v3/state",
        "X-Tilo-Delivery-Proof: 0011\r\n",
        b"binary\0body",
    );
    let owner = async {
        let request = receiver.recv().await.unwrap();
        assert_eq!(request.method(), Method::POST);
        assert_eq!(request.target(), "/peer/v3/state");
        assert_eq!(request.body(), b"binary\0body");
        assert_eq!(request.proof(), Some(&[0, 17][..]));
        assert!(!request.is_cancelled());
        request
            .respond(
                StatusCode::SERVICE_UNAVAILABLE,
                b"test receiver; no owner".to_vec(),
            )
            .unwrap();
    };
    let (response, _) = tokio::join!(client, owner);
    assert!(response.starts_with("HTTP/1.1 503"));
    assert!(response.contains("cache-control: no-store"));
    assert_eq!(receiver.available_bytes(), receiver.initial_bytes);
    fixture.finish().await;
}

#[tokio::test]
async fn tls_http2_serves_assets_while_poll_is_pending_and_cancels_on_shutdown() {
    let (fixture, mut receiver) = Fixture::start().await;
    let socket = fixture.connect(true).await;
    assert_eq!(socket.get_ref().1.alpn_protocol(), Some(&b"h2"[..]));
    let (mut sender, connection) = hyper::client::conn::http2::handshake(
        hyper_util::rt::TokioExecutor::new(),
        hyper_util::rt::TokioIo::new(socket),
    )
    .await
    .unwrap();
    let connection = tokio::spawn(connection);
    let poll = sender.send_request(
        Request::builder()
            .uri(format!(
                "https://localhost:{}/peer/v3/events?mode=poll",
                fixture.address.port()
            ))
            .header("x-tilo-delivery-proof", "00")
            .body(Full::new(Bytes::new()))
            .unwrap(),
    );
    let poll = tokio::spawn(poll);
    let pending = receiver.recv().await.unwrap();
    let response = sender
        .send_request(
            Request::builder()
                .uri(format!("https://localhost:{}/", fixture.address.port()))
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        "real asset bytes"
    );
    fixture.finish().await;
    assert!(pending.is_cancelled());
    drop(pending);
    assert_eq!(receiver.available_bytes(), receiver.initial_bytes);
    let _ = poll.await;
    let _ = connection.await;
}

#[tokio::test]
async fn oversized_body_and_missing_proof_never_reach_receiver() {
    let (fixture, mut receiver) = Fixture::start().await;
    assert!(
        fixture
            .raw("/peer/v3/state", "", b"x")
            .await
            .starts_with("HTTP/1.1 401")
    );
    assert!(
        fixture
            .raw("/peer/v3/challenge", "", &vec![0; 8193])
            .await
            .starts_with("HTTP/1.1 400")
    );
    assert!(receiver.try_recv().is_none());
    assert_eq!(receiver.available_bytes(), receiver.initial_bytes);
    fixture.finish().await;
}

#[test]
fn media_post_routes_require_exact_target_proof_and_bounded_body() {
    let origin = Origin::parse("https://localhost:9443").unwrap();
    for tail in [
        "grant/challenge",
        "grant/complete",
        "relay/challenge",
        "relay/refresh/challenge",
        "relay/refresh/complete",
        "relay/close",
        "presence",
    ] {
        let target = format!("/peer/v3/media/{tail}");
        let parts = Request::builder()
            .method("POST")
            .uri(&target)
            .header("host", "localhost:9443")
            .header(PROOF_HEADER, "00")
            .body(())
            .unwrap()
            .into_parts()
            .0;
        let parsed =
            parse_head(&parts, &origin).expect("exact media POST must enter native owner queue");
        assert_eq!(parsed.target, target);
        assert_eq!(parsed.route.body_limit(), 8192);
        for (method, raw, proof) in [
            ("GET", target.clone(), true),
            ("POST", format!("{target}?x=y"), true),
            ("POST", format!("{target}/"), true),
            ("POST", target.clone(), false),
        ] {
            let mut builder = Request::builder()
                .method(method)
                .uri(raw)
                .header("host", "localhost:9443");
            if proof {
                builder = builder.header(PROOF_HEADER, "00");
            }
            assert!(parse_head(&builder.body(()).unwrap().into_parts().0, &origin).is_err());
        }
    }
}

#[test]
fn media_lane_is_bounded_and_preserves_existing_transport_budget() {
    let (ingress, receiver) = ingress_channel();
    let connections = ingress
        .bytes
        .clone()
        .try_acquire_bulk(64 * CONNECTION_BYTES)
        .unwrap();
    let data: Vec<_> = (0..32)
        .map(|_| ingress.reserve(BODY_BYTES, Lane::Data).unwrap())
        .collect();
    let assets: Vec<_> = (0..24)
        .map(|_| ingress.reserve(0, Lane::Asset).unwrap())
        .collect();
    let media: Vec<_> = (0..32)
        .map(|_| ingress.reserve(AUTH_BYTES, Lane::Media).unwrap())
        .collect();
    assert!(ingress.reserve(AUTH_BYTES, Lane::Media).is_err());
    let controls: Vec<_> = (0..8)
        .map(|_| ingress.reserve(AUTH_BYTES, Lane::Control).unwrap())
        .collect();
    assert!(receiver.available_bytes() > 0);
    drop((connections, data, assets, media, controls));
    assert_eq!(receiver.available_bytes(), receiver.initial_bytes);
}

#[tokio::test]
async fn media_tls_routes_preserve_inert_request_and_reject_oversize_reply() {
    let (fixture, mut receiver) = Fixture::start().await;
    for route in [
        MediaRoute::GrantChallenge,
        MediaRoute::GrantComplete,
        MediaRoute::RelayChallenge,
        MediaRoute::RefreshChallenge,
        MediaRoute::RefreshComplete,
        MediaRoute::RelayClose,
        MediaRoute::Presence,
    ] {
        let client = fixture.raw(
            route.path(),
            "X-Tilo-Delivery-Proof: 0011\r\n",
            b"bounded-inert-body",
        );
        let actor = async {
            let request = timeout(Duration::from_secs(2), receiver.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(request.target(), route.path());
            assert_eq!(request.body(), b"bounded-inert-body");
            assert_eq!(request.proof(), Some(&[0, 17][..]));
            assert_eq!(request.route(), Route::Media(route));
            request
                .respond(
                    StatusCode::SERVICE_UNAVAILABLE,
                    b"no authority in transport test".to_vec(),
                )
                .unwrap();
        };
        let (response, _) = tokio::join!(client, actor);
        assert!(response.starts_with("HTTP/1.1 503"));
    }
    let client = fixture.raw(
        MediaRoute::Presence.path(),
        "X-Tilo-Delivery-Proof: 00\r\n",
        b"bounded",
    );
    let actor = async {
        let request = receiver.recv().await.unwrap();
        assert!(
            request
                .respond(StatusCode::OK, vec![0; AUTH_BYTES + 1])
                .is_err()
        );
    };
    let (response, _) = tokio::join!(client, actor);
    assert!(response.starts_with("HTTP/1.1 503"));
    fixture.finish().await;
}

#[test]
fn bootstrap_route_is_exact_bounded_and_does_not_accept_session_proof() {
    let origin = Origin::parse("https://localhost:9443").unwrap();
    let make = |path: &str| {
        Request::builder()
            .method("POST")
            .uri(path)
            .header("host", "localhost:9443")
            .body(())
            .unwrap()
            .into_parts()
            .0
    };
    let parsed =
        parse_head(&make("/peer/v3/bootstrap"), &origin).expect("dedicated bootstrap route");
    assert_eq!(parsed.route.body_limit(), 4096);
    for path in [
        "/peer/v3/bootstrap/",
        "/peer/v3/bootstrap?x=1",
        "/peer/v3/%62ootstrap",
    ] {
        assert!(parse_head(&make(path), &origin).is_err());
    }
    let mut proof = make("/peer/v3/bootstrap");
    proof.headers.insert(PROOF_HEADER, "00".parse().unwrap());
    assert!(parse_head(&proof, &origin).is_err());
}

/// Actual ingress reservation and cancellation endpoint for sibling held-poll tests.
pub(crate) struct HeldPollTestClient {
    _response: oneshot::Receiver<Reply>,
    _cancel: watch::Sender<bool>,
}
impl HeldPollTestClient {
    /// Whether the held request has been answered (any reply sent).
    pub(crate) fn answered(&mut self) -> bool {
        self._response.try_recv().is_ok()
    }
}
pub(crate) fn held_poll_test_request(
    target: &str,
) -> (BufferedRequest, HeldPollTestClient, IngressReceiver) {
    held_poll_test_request_with_deadline(target, Instant::now() + POLL_DEADLINE)
}
pub(crate) fn held_poll_test_request_with_deadline(
    target: &str,
    deadline: Instant,
) -> (BufferedRequest, HeldPollTestClient, IngressReceiver) {
    let (ingress, receiver) = ingress_channel();
    let lease = ingress.reserve(0, Lane::Data).unwrap();
    let (response, response_rx) = oneshot::channel();
    let (cancel, cancelled) = watch::channel(false);
    (
        BufferedRequest {
            method: Method::GET,
            target: target.into(),
            body: Bytes::new(),
            proof: None,
            route: Route::EventsPoll,
            deadline,
            cancelled,
            response,
            lease,
        },
        HeldPollTestClient {
            _response: response_rx,
            _cancel: cancel,
        },
        receiver,
    )
}

#[test]
fn poll_cursor_preserves_exact_bounded_lowercase_hex_envelope() {
    let origin = Origin::parse("https://localhost:9443").unwrap();
    let make = |cursor: &str| {
        Request::builder()
            .method("GET")
            .uri(format!("/peer/v3/events?cursor={cursor}&mode=poll"))
            .header("host", "localhost:9443")
            .header(PROOF_HEADER, "00")
            .body(())
            .unwrap()
            .into_parts()
            .0
    };
    let full = "ab".repeat(4096);
    let parsed = parse_head(&make(&full), &origin)
        .expect("4096 raw cursor bytes must fit canonical hex envelope");
    assert_eq!(
        parsed.target,
        format!("/peer/v3/events?cursor={full}&mode=poll")
    );
    for invalid in [
        String::new(),
        "a".into(),
        "AA".into(),
        "gg".into(),
        "%61%62".into(),
        "ab".repeat(4097),
    ] {
        assert!(parse_head(&make(&invalid), &origin).is_err());
    }
}

#[test]
fn owner_bulk_exhaustion_still_leaves_all_control_requests_and_retains_output_credit() {
    let (ingress, receiver) = ingress_channel();
    let pool = ingress.host_bytes();
    assert!(pool.same_pool(&ingress.clone().host_bytes()));
    let bulk = pool
        .try_acquire_bulk(receiver.initial_bytes - crate::host_bytes::CONTROL_RESERVE_BYTES)
        .unwrap();
    assert!(ingress.reserve(0, Lane::Data).is_err());
    assert!(ingress.reserve(0, Lane::Asset).is_err());
    assert!(ingress.reserve(AUTH_BYTES, Lane::Media).is_err());
    #[cfg(feature = "media")]
    assert!(ingress.reserve_media_transport().is_none());
    let mut controls: Vec<_> = (0..8)
        .map(|_| {
            ingress
                .reserve(crate::host_bytes::CONTROL_INPUT_BYTES, Lane::Control)
                .unwrap()
        })
        .collect();
    assert_eq!(receiver.available_bytes(), 0);
    let backing = leased_bytes(vec![7; 64], controls.pop().unwrap());
    let slice = backing.slice(1..2);
    drop(backing);
    drop(controls);
    assert_eq!(
        receiver.available_bytes(),
        7 * (2 * crate::host_bytes::CONTROL_INPUT_BYTES + BODY_BYTES + HEADER_BYTES)
    );
    assert_eq!(&slice[..], &[7]);
    drop(slice);
    drop(bulk);
    assert_eq!(receiver.available_bytes(), receiver.initial_bytes);
}

#[tokio::test]
async fn logical_response_credits_survive_reply_and_body_clones_until_final_backing_drop() {
    let window = Arc::new(Semaphore::new(1024));
    let uploads = Arc::new(Semaphore::new(2));
    let credits = ResponseCredits {
        download_window: Some(window.clone().try_acquire_many_owned(1024).unwrap()),
        upload_slot: Some(uploads.clone().try_acquire_owned().unwrap()),
        ..ResponseCredits::default()
    };
    let (request, client, _receiver) = held_poll_test_request("/peer/v3/events?mode=poll");
    request
        .respond_with_credits(StatusCode::OK, vec![7; 64], credits)
        .unwrap();
    let reply = client._response.await.unwrap();
    let frame = reply.body.clone();
    let slice = frame.slice(1..2);
    drop(reply);
    drop(client._cancel);
    drop(frame);
    assert_eq!(window.available_permits(), 0);
    assert_eq!(uploads.available_permits(), 1);
    assert_eq!(&slice[..], &[7]);
    drop(slice);
    assert_eq!(window.available_permits(), 1024);
    assert_eq!(uploads.available_permits(), 2);
}
#[test]
fn cancelled_or_rejected_response_returns_logical_credits_without_retained_output() {
    for cancel in [true, false] {
        let window = Arc::new(Semaphore::new(1024));
        let uploads = Arc::new(Semaphore::new(2));
        let credits = ResponseCredits {
            download_window: Some(window.clone().try_acquire_many_owned(1024).unwrap()),
            upload_slot: Some(uploads.clone().try_acquire_owned().unwrap()),
            ..ResponseCredits::default()
        };
        let (request, client, receiver) = held_poll_test_request("/peer/v3/events?mode=poll");
        if cancel {
            drop(client);
            assert!(
                request
                    .respond_with_credits(StatusCode::OK, vec![7; 64], credits)
                    .is_err()
            );
        } else {
            assert!(
                request
                    .respond_with_credits(StatusCode::OK, vec![7; BODY_BYTES + 1], credits)
                    .is_err()
            );
            drop(client);
        }
        assert_eq!(window.available_permits(), 1024);
        assert_eq!(uploads.available_permits(), 2);
        assert_eq!(receiver.available_bytes(), receiver.initial_bytes);
    }
}

#[tokio::test]
async fn local_https_enrollment_reuses_private_native_issuer_and_committed_owner() {
    use crate::enrollment_http::{Authorization, EnrollmentRequest};
    let temp = tempfile::tempdir().unwrap();
    let mut owner = crate::SessionOwner::initialize(&temp.path().join("state")).unwrap();
    let authorization =
        Arc::new(Authorization::local(owner.root(), "127.0.0.1:4000".into()).unwrap());
    let credential = std::fs::read_to_string(owner.root().join("enrollment-capability")).unwrap();
    let (commands, mut requests) = mpsc::channel(64);
    let (fixture, mut wire) = Fixture::start_with_enrollment(Some((authorization, commands))).await;
    let headers = format!(
        "Origin: https://localhost:{}\r\nContent-Type: application/json\r\nCookie: __Host-qos-local-owner={credential}\r\n",
        fixture.address.port()
    );
    let key = ed25519_dalek::SigningKey::from_bytes(&[67; 32]);
    let body = serde_json::to_vec(&qos_peer_state::enrollment::ChallengeRequest {
        device_public_key: hex::encode(key.verifying_key().to_bytes()),
        request_id: hex::encode([68; 32]),
    })
    .unwrap();
    let mut enrollment = crate::enrollment::EnrollmentAuthority::default();
    let client = fixture.raw("/qos-peer/enrollment/challenge", &headers, &body);
    let actor = async {
        let Some(command) = timeout(Duration::from_secs(2), requests.recv())
            .await
            .ok()
            .flatten()
        else {
            return;
        };
        let EnrollmentRequest::Challenge(request) = command.request else {
            panic!("wrong native command")
        };
        let challenge = enrollment
            .challenge(&owner, request, tilo_relay::clock::now_ns())
            .unwrap();
        command
            .reply
            .send(Ok(serde_json::to_vec(&challenge).unwrap()))
            .unwrap();
    };
    let (response, ()) = tokio::join!(client, actor);
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "authorized same-origin enrollment was not installed"
    );
    assert!(
        response
            .to_ascii_lowercase()
            .contains("cache-control: no-store")
    );
    let challenge: qos_peer_state::enrollment::ChallengeResponse =
        serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap();
    let signed = qos_peer_state::enrollment::sign(
        &qos_peer_state::enrollment::decode(&challenge.challenge).unwrap(),
        &key,
    )
    .unwrap();
    let body = serde_json::to_vec(&signed).unwrap();
    let before = owner.committed_generation();
    let client = fixture.raw("/qos-peer/enrollment/complete", &headers, &body);
    let actor = async {
        let command = timeout(Duration::from_secs(2), requests.recv())
            .await
            .unwrap()
            .unwrap();
        let EnrollmentRequest::Complete(request) = command.request else {
            panic!("wrong native command")
        };
        let certificate = enrollment
            .complete(&mut owner, request, tilo_relay::clock::now_ns())
            .unwrap();
        command
            .reply
            .send(Ok(serde_json::to_vec(&certificate).unwrap()))
            .unwrap();
    };
    let (response, ()) = tokio::join!(client, actor);
    assert!(response.starts_with("HTTP/1.1 200"));
    assert!(owner.committed_generation() > before);
    assert!(
        owner
            .peer()
            .certificates()
            .iter()
            .any(|cert| cert.device_public_key == key.verifying_key().to_bytes())
    );
    assert!(
        wire.try_recv().is_none(),
        "enrollment is not a delivery proof request"
    );
    let denied = fixture
        .raw(
            "/qos-peer/enrollment/challenge",
            "Content-Type: application/json\r\n",
            b"{}",
        )
        .await;
    assert!(denied.starts_with("HTTP/1.1 403"));
    assert!(requests.try_recv().is_err());
    let valid = serde_json::to_vec(&qos_peer_state::enrollment::ChallengeRequest {
        device_public_key: hex::encode(key.verifying_key().to_bytes()),
        request_id: hex::encode([69; 32]),
    })
    .unwrap();
    for path in [
        "/qos-peer/enrollment/challenge/",
        "/qos-peer/enrollment/challenge?x=1",
        "/qos-peer/enrollment/%63hallenge",
    ] {
        let response = fixture.raw(path, &headers, &valid).await;
        assert!(!response.starts_with("HTTP/1.1 200"));
        assert!(requests.try_recv().is_err());
    }
    let response = fixture
        .raw(
            "/qos-peer/enrollment/challenge",
            &headers,
            &vec![b' '; local_enrollment::INPUT_BYTES + 1],
        )
        .await;
    assert!(response.starts_with("HTTP/1.1 400"));
    assert!(requests.try_recv().is_err());
    let duplicate = format!("{headers}Cookie: __Host-qos-local-owner={credential}\r\n");
    let response = fixture
        .raw("/qos-peer/enrollment/challenge", &duplicate, &valid)
        .await;
    assert!(response.starts_with("HTTP/1.1 400"));
    assert!(requests.try_recv().is_err());
    // The browser attaches its HttpOnly cookie to navigation/assets as well;
    // that must work without treating the cookie as delivery session authority.
    let mut socket = fixture.connect(false).await;
    socket.write_all(format!("GET / HTTP/1.1\r\nHost: localhost:{}\r\nCookie: __Host-qos-local-owner={credential}\r\nConnection: close\r\n\r\n",fixture.address.port()).as_bytes()).await.unwrap();
    let mut navigation = Vec::new();
    timeout(Duration::from_secs(3), socket.read_to_end(&mut navigation))
        .await
        .unwrap()
        .unwrap();
    let navigation = String::from_utf8(navigation).unwrap();
    assert!(navigation.starts_with("HTTP/1.1 200"));
    assert!(navigation.ends_with("real asset bytes"));
    assert!(!navigation.contains(&credential));
    fixture.finish().await;
}

#[tokio::test]
async fn local_enrollment_owner_wait_deadline_cancels_unconsumed_private_command() {
    use crate::enrollment_http::Authorization;
    let temp = tempfile::tempdir().unwrap();
    let authorization =
        Arc::new(Authorization::local(temp.path(), "127.0.0.1:4000".into()).unwrap());
    let credential = std::fs::read_to_string(temp.path().join("enrollment-capability")).unwrap();
    let (commands, mut requests) = mpsc::channel(64);
    let (fixture, receiver) = Fixture::start_with_enrollment(Some((authorization, commands))).await;
    let body = serde_json::to_vec(&qos_peer_state::enrollment::ChallengeRequest {
        device_public_key: hex::encode(
            ed25519_dalek::SigningKey::from_bytes(&[72; 32])
                .verifying_key()
                .to_bytes(),
        ),
        request_id: hex::encode([73; 32]),
    })
    .unwrap();
    let mut socket = fixture.connect(false).await;
    socket.write_all(format!("POST /qos-peer/enrollment/challenge HTTP/1.1\r\nHost: localhost:{}\r\nOrigin: https://localhost:{}\r\nCookie: __Host-qos-local-owner={credential}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",fixture.address.port(),fixture.address.port(),body.len()).as_bytes()).await.unwrap();
    socket.write_all(&body).await.unwrap();
    let command = timeout(Duration::from_secs(1), requests.recv())
        .await
        .unwrap()
        .unwrap();
    let mut response = Vec::new();
    timeout(Duration::from_secs(6), socket.read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();
    assert!(
        String::from_utf8(response)
            .unwrap()
            .starts_with("HTTP/1.1 503")
    );
    assert!(command.reply.is_closed());
    fixture.finish().await;
    assert_eq!(
        receiver.available_bytes(),
        receiver.initial_bytes - (2 * local_enrollment::INPUT_BYTES + BODY_BYTES + HEADER_BYTES),
        "timed-out queued native work must retain its original request charge"
    );
    drop(command);
    assert_eq!(receiver.available_bytes(), receiver.initial_bytes);
}

#[test]
fn queued_local_enrollment_rejects_elapsed_deadline_with_open_reply() {
    use crate::enrollment_http::{EnrollmentCommand, EnrollmentRequest};
    let (ingress, receiver) = ingress_channel();
    let lease = ingress
        .reserve(local_enrollment::INPUT_BYTES, Lane::Control)
        .unwrap();
    let (reply, completed) = oneshot::channel();
    let command = EnrollmentCommand::from_https(
        EnrollmentRequest::Challenge(qos_peer_state::enrollment::ChallengeRequest {
            device_public_key: "01".repeat(32),
            request_id: "02".repeat(32),
        }),
        reply,
        lease,
        Instant::now() - Duration::from_secs(1),
    );
    assert!(
        !command.reply.is_closed(),
        "timeout task has not closed the receiver"
    );
    assert!(
        command.is_cancelled(),
        "owner must check the original trusted deadline itself"
    );
    drop(command);
    drop(completed);
    assert_eq!(receiver.available_bytes(), receiver.initial_bytes);
}

#[test]
fn cancelled_local_enrollment_queue_retains_charge_and_control_slot_until_drop() {
    use crate::enrollment_http::{EnrollmentCommand, EnrollmentRequest};
    let (ingress, receiver) = ingress_channel();
    let lease = ingress
        .reserve(local_enrollment::INPUT_BYTES, Lane::Control)
        .unwrap();
    let (reply, completed) = oneshot::channel();
    let command = EnrollmentCommand::from_https(
        EnrollmentRequest::Challenge(qos_peer_state::enrollment::ChallengeRequest {
            device_public_key: "03".repeat(32),
            request_id: "04".repeat(32),
        }),
        reply,
        lease,
        Instant::now() + DEADLINE,
    );
    assert!(!command.is_cancelled());
    let (commands, queued) = mpsc::channel(64);
    assert!(commands.try_send(command).is_ok());
    drop(completed);
    assert_eq!(
        receiver.available_bytes(),
        receiver.initial_bytes - (2 * local_enrollment::INPUT_BYTES + BODY_BYTES + HEADER_BYTES)
    );
    let other_controls: Vec<_> = (0..7)
        .map(|_| {
            ingress
                .reserve(local_enrollment::INPUT_BYTES, Lane::Control)
                .unwrap()
        })
        .collect();
    assert!(
        ingress
            .reserve(local_enrollment::INPUT_BYTES, Lane::Control)
            .is_err()
    );
    drop(other_controls);
    drop(queued); // Queue retirement drops owned decoded request before its credit.
    assert_eq!(receiver.available_bytes(), receiver.initial_bytes);
    let controls: Vec<_> = (0..8)
        .map(|_| {
            ingress
                .reserve(local_enrollment::INPUT_BYTES, Lane::Control)
                .unwrap()
        })
        .collect();
    drop(controls);
    assert_eq!(receiver.available_bytes(), receiver.initial_bytes);
}

#[test]
fn descriptor_discovery_is_exact_public_get_with_no_body_or_proof() {
    let origin = Origin::parse("https://localhost:9443").unwrap();
    let make = |method: &str, target: &str, proof: bool| {
        let mut r = Request::builder()
            .method(method)
            .uri(target)
            .header("host", "localhost:9443");
        if proof {
            r = r.header(PROOF_HEADER, "00");
        }
        r.body(()).unwrap().into_parts().0
    };
    let parsed = parse_head(&make("GET", "/peer/v3/descriptor", false), &origin)
        .expect("public bounded descriptor discovery route");
    assert_eq!(parsed.route.body_limit(), 0);
    assert!(parsed.route.control());
    assert!(parsed.proof.is_none());
    for (method, target, proof) in [
        ("POST", "/peer/v3/descriptor", false),
        ("GET", "/peer/v3/descriptor?x=1", false),
        ("GET", "/peer/v3/descriptor/", false),
        ("GET", "/peer/v3/%64escriptor", false),
        ("GET", "/peer/v3/descriptor", true),
    ] {
        assert!(parse_head(&make(method, target, proof), &origin).is_err());
    }
}

#[test]
fn descriptor_discovery_reply_rejects_capacity_over_24_kib() {
    let (ingress, receiver) = ingress_channel();
    let (response, _received) = oneshot::channel();
    let (_cancel, cancelled) = watch::channel(false);
    let request = BufferedRequest {
        method: Method::GET,
        target: "/peer/v3/descriptor".into(),
        body: Bytes::new(),
        proof: None,
        route: Route::Descriptor,
        deadline: Instant::now() + DEADLINE,
        cancelled,
        response,
        lease: ingress.reserve(0, Lane::Control).unwrap(),
    };
    let mut body = Vec::with_capacity(tilo_relay::delivery_v3::descriptor::MAX_HINT_BYTES + 1);
    body.push(0);
    assert_eq!(
        request.respond(StatusCode::OK, body),
        Err("invalid bounded response")
    );
    assert_eq!(receiver.available_bytes(), receiver.initial_bytes);
}

/// Sibling owner tests use actual parser and reservations from their attached ingress.
pub(crate) fn data_test_request(
    ingress: &Ingress,
    method: &str,
    target: &str,
    body: &[u8],
    proof: &[u8],
    deadline: Instant,
) -> (BufferedRequest, HeldPollTestClient) {
    let proof_hex = proof
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let parts = Request::builder()
        .method(method)
        .uri(target)
        .header("host", "localhost:9443")
        .header("content-length", body.len())
        .header(PROOF_HEADER, proof_hex)
        .body(())
        .unwrap()
        .into_parts()
        .0;
    let parsed = parse_head(&parts, &Origin::parse("https://localhost:9443").unwrap()).unwrap();
    assert!(matches!(
        parsed.route,
        Route::State | Route::Acks | Route::EventsPoll | Route::Private(_) | Route::PrivatePoll
    ));
    assert!(body.len() <= parsed.route.body_limit());
    let lane = if parsed.route.control() {
        Lane::Control
    } else {
        Lane::Data
    };
    let lease = ingress.reserve(body.len(), lane).unwrap();
    let (response, response_rx) = oneshot::channel();
    let (cancel, cancelled) = watch::channel(false);
    (
        BufferedRequest {
            method: parts.method,
            target: parsed.target,
            body: Bytes::copy_from_slice(body),
            proof: parsed.proof,
            route: parsed.route,
            deadline,
            cancelled,
            response,
            lease,
        },
        HeldPollTestClient {
            _response: response_rx,
            _cancel: cancel,
        },
    )
}
impl HeldPollTestClient {
    pub(crate) async fn receive_body(
        self,
    ) -> Result<(StatusCode, Bytes), oneshot::error::RecvError> {
        let reply = self._response.await?;
        Ok((reply.status, reply.body))
    }
    pub(crate) fn cancel(&self) {
        self._cancel.send(true).unwrap();
    }
}

#[tokio::test]
async fn local_https_enrollment_renewal_may_only_request_governance_omission() {
    use crate::enrollment_http::{Authorization, EnrollmentRequest, GOVERNANCE_HEADER};
    let temp = tempfile::tempdir().unwrap();
    let owner = crate::SessionOwner::initialize(&temp.path().join("state")).unwrap();
    let authorization =
        Arc::new(Authorization::local(owner.root(), "127.0.0.1:4000".into()).unwrap());
    let credential = std::fs::read_to_string(owner.root().join("enrollment-capability")).unwrap();
    let (commands, mut requests) = mpsc::channel(64);
    let (fixture, _wire) = Fixture::start_with_enrollment(Some((authorization, commands))).await;
    let headers = format!(
        "Origin: https://localhost:{}\r\nContent-Type: application/json\r\nCookie: __Host-qos-local-owner={credential}\r\n",
        fixture.address.port()
    );
    let complete = serde_json::to_vec(&qos_peer_state::enrollment::CompleteRequest {
        version: 2,
        challenge: "00".into(),
        signature: "00".repeat(64),
    })
    .unwrap();
    // (path, extra header, expected flag); the owner answers 503 regardless.
    for (path, extra, expected) in [
        ("/qos-peer/enrollment/complete", String::new(), false),
        (
            "/qos-peer/enrollment/complete",
            format!("{GOVERNANCE_HEADER}: omit\r\n"),
            true,
        ),
    ] {
        let request_headers = format!("{headers}{extra}");
        let client = fixture.raw(path, &request_headers, &complete);
        let actor = async {
            let command = timeout(Duration::from_secs(2), requests.recv())
                .await
                .unwrap()
                .unwrap();
            assert!(command.is_https());
            assert_eq!(command.omits_governance(), expected);
            assert!(matches!(command.request, EnrollmentRequest::Complete(_)));
            command
                .reply
                .send(Err(crate::enrollment::EnrollmentError::new(503, "test")))
                .unwrap();
        };
        let (response, ()) = tokio::join!(client, actor);
        assert!(response.starts_with("HTTP/1.1 503"), "{response}");
    }
    // Any other value, or a duplicate header, is malformed rather than ignored.
    for extra in [
        format!("{GOVERNANCE_HEADER}: full\r\n"),
        format!("{GOVERNANCE_HEADER}: omit\r\n{GOVERNANCE_HEADER}: omit\r\n"),
    ] {
        let response = fixture
            .raw(
                "/qos-peer/enrollment/complete",
                &format!("{headers}{extra}"),
                &complete,
            )
            .await;
        assert!(response.starts_with("HTTP/1.1 400"), "{response}");
        assert!(requests.try_recv().is_err());
    }
    fixture.finish().await;
}

#[tokio::test]
async fn accepted_https_sockets_disable_nagle() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let client = tokio::spawn(async move { tokio::net::TcpStream::connect(address).await });
    let (socket, _) = listener.accept().await.unwrap();
    assert!(!socket.nodelay().unwrap());
    prepare_accepted(&socket);
    assert!(socket.nodelay().unwrap());
    drop(client.await.unwrap().unwrap());
}
