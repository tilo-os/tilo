//! Source-only regressions for the real generic Rig/HTTP admission boundary.
//! These tests deliberately use loopback peers, never provider accounts.
use super::*;
use agent_provider_config::ProviderProtocol;
use agent_provider_config::protocol as neutral;
use serde_json::json;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Default)]
struct Gate {
    frozen: Mutex<Option<(String, Vec<u8>)>>,
    owner_turn: Mutex<()>,
    stop_after_prepare: AtomicBool,
    stall_prepare: AtomicBool,
    stopped: AtomicBool,
    starts: AtomicUsize,
}

impl NativeModelDispatch for Gate {
    fn prepare<'a>(
        &'a self,
        invocation: &'a serde_json::Value,
        attempt: &'a FrozenNativeAttempt,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            assert_eq!(invocation, &json!({"child":"one"}));
            // A real custody wait precedes the final stop/fence turn.
            tokio::task::yield_now().await;
            *self.frozen.lock().unwrap() = Some((attempt.endpoint.clone(), attempt.body.clone()));
            if self.stall_prepare.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            if self.stop_after_prepare.load(Ordering::SeqCst) {
                self.stopped.store(true, Ordering::SeqCst);
            }
            Ok(())
        })
    }

    fn dispatch(
        &self,
        _: &serde_json::Value,
        attempt: &FrozenNativeAttempt,
        start: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        let _turn = self.owner_turn.lock().unwrap();
        if self.stopped.load(Ordering::SeqCst) {
            return Err("stopped during custody wait".into());
        }
        assert_eq!(
            self.frozen.lock().unwrap().as_ref().unwrap(),
            &(attempt.endpoint.clone(), attempt.body.clone()),
        );
        self.starts.fetch_add(1, Ordering::SeqCst);
        start()
    }
}

fn ask() -> neutral::ModelRequest {
    neutral::ModelRequest {
        messages: vec![neutral::Message::text(
            neutral::Role::User,
            "private question",
        )],
        tools: vec![],
        tool_choice: neutral::ToolChoice::None,
        max_output_tokens: 16,
    }
}

async fn read_http(socket: &mut tokio::net::TcpStream) -> Vec<u8> {
    let mut bytes = Vec::new();
    let body_start = loop {
        let mut byte = [0; 1];
        socket.read_exact(&mut byte).await.unwrap();
        bytes.push(byte[0]);
        if bytes.ends_with(b"\r\n\r\n") {
            break bytes.len();
        }
    };
    let head = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
    let length: usize = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    bytes.resize(body_start + length, 0);
    socket.read_exact(&mut bytes[body_start..]).await.unwrap();
    bytes[body_start..].to_vec()
}

#[tokio::test]
async fn generic_native_http_uses_exact_frozen_body_and_awaits_response_outside_gate() {
    for protocol in [
        ProviderProtocol::OpenAiCompatible,
        ProviderProtocol::Anthropic,
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut profile = ProviderProfile::hosted(protocol);
        profile.endpoint = format!("http://{}", listener.local_addr().unwrap());
        let gate = Arc::new(Gate::default());
        let peer_gate = gate.clone();
        let peer = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let body = read_http(&mut socket).await;
            assert_eq!(body, peer_gate.frozen.lock().unwrap().as_ref().unwrap().1);
            assert_eq!(peer_gate.starts.load(Ordering::SeqCst), 1);
            // Response completion must not await while retaining the owner
            // dispatch/fence mutation turn.
            assert!(peer_gate.owner_turn.try_lock().is_ok());
            let reply = if protocol == ProviderProtocol::Anthropic {
                r#"{"id":"answer","type":"message","role":"assistant","model":"selected","content":[{"type":"text","text":"done"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}}"#
            } else {
                r#"{"id":"answer","object":"chat.completion","created":1,"model":"selected","choices":[{"index":0,"message":{"role":"assistant","content":"done"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        let http = super::super::provider_client().unwrap();
        // OpenAI-compatible local accounts deliberately have no secret.
        let secret = (protocol == ProviderProtocol::Anthropic).then_some("fabricated-key");
        let response = CONTEXT
            .scope(
                NativeContext {
                    invocation: json!({"child":"one"}),
                    dispatch: gate.clone(),
                    completed: Arc::default(),
                    dispatched: Arc::default(),
                },
                super::super::rig_adapter::host_complete(
                    &http,
                    &profile,
                    secret,
                    "selected",
                    super::super::rig_adapter::neutral_request(ask()).unwrap(),
                    Duration::from_secs(3),
                    crate::openai::Target::Standard,
                ),
            )
            .await
            .map_err(|failure| failure.why)
            .unwrap();
        let decoded = super::super::rig_adapter::neutral_response(response).unwrap();
        assert_eq!(decoded.text, "done");
        tokio::time::timeout(Duration::from_secs(3), peer)
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn stop_after_async_custody_wait_prevents_real_http_post() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut profile = ProviderProfile::hosted(ProviderProtocol::OpenAiCompatible);
    profile.endpoint = format!("http://{}", listener.local_addr().unwrap());
    let gate = Arc::new(Gate::default());
    gate.stop_after_prepare.store(true, Ordering::SeqCst);
    let http = super::super::provider_client().unwrap();
    let result = CONTEXT.scope(
        NativeContext { invocation: json!({"child":"one"}), dispatch: gate.clone(), completed: Arc::default(), dispatched: Arc::default() },
        super::super::rig_adapter::legacy(
            &http, &profile, None,
            json!({"model":"selected", "messages":[{"role":"user","content":"private"}], "max_tokens":16}),
            Duration::from_secs(3), None, true,
        ),
    ).await;
    let failure = result.err().expect("stop must refuse the attempt");
    assert_eq!(failure.model_error.kind, ErrorKind::PermissionDenied);
    assert_eq!(gate.starts.load(Ordering::SeqCst), 0);
    // TCP readiness is deliberately prepared BEFORE admission; a refused
    // attempt closes that idle connection without writing any HTTP bytes.
    let (mut socket, _) = listener.accept().await.unwrap();
    let mut first_byte = [0; 1];
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), socket.read(&mut first_byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn native_transform_does_not_replay_an_http_busy_response() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut profile = ProviderProfile::hosted(ProviderProtocol::OpenAiCompatible);
    profile.endpoint = format!("http://{}", listener.local_addr().unwrap());
    let gate = Arc::new(Gate::default());
    let peer = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_http(&mut socket).await;
        let body = r#"{"error":{"type":"rate_limit_error","message":"busy"}}"#;
        socket.write_all(format!(
            "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nContent-Length: {}\r\nRetry-After: 0\r\nConnection: close\r\n\r\n{body}", body.len(),
        ).as_bytes()).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err()
        );
    });
    let http = super::super::provider_client().unwrap();
    let body = json!({"model":"selected", "messages":[{"role":"user","content":"private"}], "max_tokens":16});
    let result = CONTEXT
        .scope(
            NativeContext {
                invocation: json!({"child":"one"}),
                dispatch: gate.clone(),
                completed: Arc::default(),
                dispatched: Arc::default(),
            },
            super::super::unary_with_retries(&http, &profile, None, &body, Duration::from_secs(3)),
        )
        .await;
    assert!(result.is_err());
    assert_eq!(gate.starts.load(Ordering::SeqCst), 1);
    peer.await.unwrap();
}

#[tokio::test]
async fn native_deadline_bounds_profile_cache_wait_before_transport() {
    let gate = Arc::new(Gate::default());
    let source = NativeGatewaySource::new(gate.clone()).unwrap();
    // Production resolution runs on a blocking worker. Holding this cache
    // forces it to wait before it can resolve any profile or start transport.
    let (ready, locked) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let _cache = super::super::PLAN_PROFILES.state.lock().unwrap();
        let _ = ready.send(());
        let _ = released.recv();
    });
    locked.await.unwrap();
    let request = Request::NativeComplete {
        invocation: json!({"child":"one"}),
        profile_id: String::new(),
        model_id: "selected".into(),
        request: ask(),
        timeout_ms: 50,
        stream: false,
    };
    let response = tokio::time::timeout(
        Duration::from_secs(1),
        source.execute(request, &mut |_| true),
    )
    .await;
    drop(release);
    holder.join().unwrap();
    assert_eq!(
        response.unwrap().model_error.unwrap().kind,
        ErrorKind::TimedOut
    );
    assert_eq!(gate.starts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn native_deadline_cancels_stalled_custody_before_http_transfer() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut profile = ProviderProfile::hosted(ProviderProtocol::OpenAiCompatible);
    profile.endpoint = format!("http://{}", listener.local_addr().unwrap());
    let gate = Arc::new(Gate::default());
    gate.stall_prepare.store(true, Ordering::SeqCst);
    let http = super::super::provider_client().unwrap();
    let result = CONTEXT
        .scope(
            NativeContext {
                invocation: json!({"child":"one"}),
                dispatch: gate.clone(),
                completed: Arc::default(),
                dispatched: Arc::default(),
            },
            super::super::rig_adapter::host_complete(
                &http,
                &profile,
                None,
                "selected",
                super::super::rig_adapter::neutral_request(ask()).unwrap(),
                Duration::from_millis(100),
                crate::openai::Target::Standard,
            ),
        )
        .await;
    assert_eq!(result.err().unwrap().model_error.kind, ErrorKind::TimedOut);
    assert!(
        gate.frozen.lock().unwrap().is_some(),
        "must reach custody preparation"
    );
    assert_eq!(gate.starts.load(Ordering::SeqCst), 0);
    let (mut socket, _) = listener.accept().await.unwrap();
    let mut byte = [0];
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), socket.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn hyper_transfers_request_before_response_future_is_polled() {
    // Pin the library behavior relied on by NativeContext::send: the driver
    // may write later, but the task cannot be holding an unsubmitted request.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let tcp = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake::<_, Full<Bytes>>(TokioIo::new(tcp))
            .await
            .unwrap();
    let driver = Driver(Some(tokio::spawn(async move {
        let _ = connection.await;
    })));
    sender.ready().await.unwrap();
    let request = hyper::Request::builder()
        .method("POST")
        .uri("/attempt")
        .header("host", "localhost")
        .body(Full::new(Bytes::from_static(b"frozen")))
        .unwrap();
    let response = sender.send_request(request);
    // No poll of `response` occurs before the peer has received the POST.
    let (mut peer, _) = listener.accept().await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), read_http(&mut peer))
            .await
            .unwrap(),
        b"frozen"
    );
    drop(response);
    drop(driver);
}

#[tokio::test]
async fn protected_source_refuses_legacy_and_ordinary_source_refuses_native() {
    let source = NativeGatewaySource::new(Arc::new(Gate::default())).unwrap();
    let ordinary = Request::Complete {
        profile_id: "not-configured".into(),
        model_id: "selected".into(),
        request: ask(),
        timeout_ms: 100,
        stream: false,
    };
    let response = source.execute(ordinary, &mut |_| true).await;
    assert_eq!(
        response.model_error.unwrap().kind,
        ErrorKind::PermissionDenied
    );
    let native = Request::NativeComplete {
        invocation: json!({"child":"one"}),
        profile_id: "not-configured".into(),
        model_id: "selected".into(),
        request: ask(),
        timeout_ms: 100,
        stream: false,
    };
    let response =
        super::super::handle_streaming(super::super::AGENT_APP, native, &source.http, &mut |_| {
            true
        })
        .await;
    assert_eq!(
        response.model_error.unwrap().kind,
        ErrorKind::PermissionDenied
    );
}
