use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

struct Grant {
    active: AtomicBool,
    caller: Caller,
    management: bool,
    authenticated: tokio::sync::Notify,
}
impl Session for Grant {
    fn caller(&self) -> Caller {
        self.caller
    }
    fn may_manage_credentials(&self) -> bool {
        self.management
    }
    fn active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }
}
struct Auth(Arc<Grant>);
impl SessionAuthorizer for Auth {
    fn authorize(&self, headers: &hyper::HeaderMap) -> Option<Arc<dyn Session>> {
        (headers.get("authorization")?.as_bytes() == b"Bearer host-session").then(|| {
            self.0.authenticated.notify_one();
            self.0.clone() as Arc<dyn Session>
        })
    }
}
async fn start(caller: Caller) -> (String, Arc<Grant>, HttpService, tokio::task::JoinHandle<()>) {
    let grant = Arc::new(Grant {
        active: AtomicBool::new(true),
        caller,
        management: false,
        authenticated: tokio::sync::Notify::new(),
    });
    let service = HttpService::new(Arc::new(Auth(grant.clone())), 1).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/model/rpc", listener.local_addr().unwrap());
    let serving = service.clone();
    let task = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let service = serving.clone();
            tokio::spawn(async move {
                let _ = service.serve_io(stream).await;
            });
        }
    });
    (url, grant, service, task)
}
fn request(client: &reqwest::Client, url: &str) -> reqwest::RequestBuilder {
    client.post(url).header("content-type", "application/json")
}

#[tokio::test]
async fn unauthorized_completion_is_http_permission_denied() {
    let (url, _, _, task) = start(Caller::Photos).await;
    let client = crate::service::provider_client().unwrap();
    let body = serde_json::json!({
        "version": RPC_VERSION, "verb": "complete", "profile_id": "unconfigured",
        "model_id": "unconfigured",
        "request": agent_provider_config::protocol::Conversation::new("system", "hello").request(Vec::new(), 64),
        "timeout_ms": 1000, "stream": false
    });
    let response = request(&client, &url)
        .bearer_auth("host-session")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 403);
    task.abort();
}

#[tokio::test]
async fn forged_identity_and_key_are_not_reflected() {
    let (url, _, _, task) = start(Caller::Agent).await;
    let client = crate::service::provider_client().unwrap();
    let body = serde_json::json!({"version": RPC_VERSION, "verb":"set_api_key", "profile_id":"x", "api_key":"never-reflect-this", "app_id":"so.tilo.settings", "may_manage_credentials":true});
    let unauthenticated = request(&client, &url).json(&body).send().await.unwrap();
    assert_eq!(unauthenticated.status(), 401);
    assert!(
        !unauthenticated
            .text()
            .await
            .unwrap()
            .contains("never-reflect-this")
    );
    let forged = request(&client, &url)
        .bearer_auth("host-session")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(forged.status(), 403);
    assert!(!forged.text().await.unwrap().contains("never-reflect-this"));
    task.abort();
}

#[tokio::test]
async fn settings_identity_requires_explicit_management_and_revocation_is_observed() {
    let (url, grant, _, task) = start(Caller::Settings).await;
    let client = crate::service::provider_client().unwrap();
    for verb in ["secret_status", "set_api_key", "remove_api_key"] {
        let body = serde_json::json!({"version": RPC_VERSION,"verb":verb,"profile_id":"x","api_key":"not-a-provider-key"});
        let response = request(&client, &url)
            .bearer_auth("host-session")
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 403, "{verb}");
    }
    grant.active.store(false, Ordering::SeqCst);
    let response = request(&client, &url)
        .bearer_auth("host-session")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    task.abort();
}

#[tokio::test]
async fn request_bytes_and_concurrency_are_bounded() {
    let (url, _, service, task) = start(Caller::Agent).await;
    let client = crate::service::provider_client().unwrap();
    let response = request(&client, &url)
        .bearer_auth("host-session")
        .body(vec![b' '; MAX_REQUEST_BYTES + 1])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 413);
    let slot = service.slots.acquire().await.unwrap();
    let response = request(&client, &url)
        .bearer_auth("host-session")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 429);
    drop(slot);
    task.abort();
}

#[test]
fn oversized_response_frame_is_refused() {
    let response = Response {
        ok: true,
        output: Some("x".repeat(MAX_EVENT_BYTES)),
        configured: None,
        error: None,
        model_error: None,
        event: None,
    };
    assert!(encode(&response).is_none());
}

fn stream_response(index: usize) -> Response {
    Response {
        ok: true,
        output: Some(index.to_string()),
        configured: None,
        error: None,
        model_error: None,
        event: Some(
            [
                "text_delta",
                "reasoning_delta",
                crate::EVENT_TOOL_CALL_DELTA,
                crate::EVENT_TOOL_CALL,
            ][index % 4]
                .into(),
        ),
    }
}
fn final_response() -> Response {
    Response {
        event: None,
        ..stream_response(0)
    }
}

#[tokio::test]
async fn provider_burst_in_one_write_preserves_every_frame_and_final_flush() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    // All provider events arrive in one write/read, so emit runs more than QUEUE
    // times in one producer poll, including interleaved tool/reasoning frames.
    let expected: Vec<_> = (0..QUEUE * 4).map(stream_response).collect();
    let wire: Vec<_> = expected
        .iter()
        .flat_map(|item| encode(item).unwrap())
        .collect();
    let (mut provider, mut input) = tokio::io::duplex(wire.len() + 1);
    provider.write_all(&wire).await.unwrap();
    drop(provider);
    let events = EventBuffer::default();
    let (sender, mut receiver) = mpsc::channel(QUEUE);
    let producer = async {
        let mut bytes = Vec::new();
        input.read_to_end(&mut bytes).await.unwrap();
        for line in bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            let response = serde_json::from_slice(line).unwrap();
            assert!(
                events.emit(&response),
                "a burst must not look like cancellation"
            );
        }
        final_response()
    };
    // Do not consume anything until production completes: the entire burst fits
    // in one bounded batch rather than requiring one queue slot per event.
    forward(producer, &events, &sender).await;
    drop(sender);
    let mut collected = Vec::new();
    while let Some(batch) = receiver.recv().await {
        collected.extend_from_slice(&batch);
    }
    let mut expected_wire = wire;
    expected_wire.extend_from_slice(&encode(&final_response()).unwrap());
    assert_eq!(collected, expected_wire);
}

struct OnDrop<'a>(&'a AtomicBool);
impl Drop for OnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn slow_consumer_applies_backpressure_and_disconnect_drops_producer() {
    let events = EventBuffer::default();
    let (sender, mut receiver) = mpsc::channel(QUEUE);
    let polls = std::sync::atomic::AtomicUsize::new(0);
    let dropped = AtomicBool::new(false);
    let producer = async {
        let _guard = OnDrop(&dropped);
        std::future::poll_fn(|cx| {
            let index = polls.fetch_add(1, Ordering::SeqCst);
            assert!(events.emit(&stream_response(index)));
            cx.waker().wake_by_ref();
            Poll::<Response>::Pending
        })
        .await
    };
    let forwarding = forward(producer, &events, &sender);
    tokio::pin!(forwarding);
    assert!(futures_util::poll!(&mut forwarding).is_pending());
    assert_eq!(polls.load(Ordering::SeqCst), QUEUE + 1);
    assert_eq!(receiver.len(), QUEUE);
    assert!(events.state.lock().unwrap().1.is_empty());
    // Polling again cannot advance the provider while the consumer is stalled.
    assert!(futures_util::poll!(&mut forwarding).is_pending());
    assert_eq!(polls.load(Ordering::SeqCst), QUEUE + 1);
    receiver.recv().await.unwrap();
    assert!(futures_util::poll!(&mut forwarding).is_pending());
    assert_eq!(polls.load(Ordering::SeqCst), QUEUE + 2);
    drop(receiver);
    tokio::time::timeout(tilo_test_env::hang_bound(), forwarding)
        .await
        .unwrap();
    assert!(dropped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn slow_consumer_can_resume_and_receive_final_response() {
    let events = EventBuffer::default();
    let (sender, mut receiver) = mpsc::channel(QUEUE);
    let mut next = 0;
    let producer = std::future::poll_fn(|cx| {
        if next == QUEUE * 3 {
            return Poll::Ready(final_response());
        }
        assert!(events.emit(&stream_response(next)));
        next += 1;
        cx.waker().wake_by_ref();
        Poll::Pending
    });
    let forwarding = forward(producer, &events, &sender);
    tokio::pin!(forwarding);
    assert!(futures_util::poll!(&mut forwarding).is_pending());
    assert_eq!(receiver.len(), QUEUE);
    let mut collected = Vec::new();
    // Drain a single batch at a time, allowing the producer to fill the queue
    // again each time. Even the terminal frame must wait rather than be lost.
    let mut finished = false;
    for _ in 0..QUEUE * 4 {
        if let Ok(batch) = receiver.try_recv() {
            collected.extend_from_slice(&batch);
        }
        if futures_util::poll!(&mut forwarding).is_ready() {
            finished = true;
            break;
        }
    }
    assert!(finished, "resuming the consumer must finish the producer");
    while let Ok(batch) = receiver.try_recv() {
        collected.extend_from_slice(&batch);
    }
    let mut expected: Vec<_> = (0..QUEUE * 3)
        .flat_map(|index| encode(&stream_response(index)).unwrap())
        .collect();
    expected.extend_from_slice(&encode(&final_response()).unwrap());
    assert_eq!(collected, expected);
}

#[test]
fn synchronous_burst_has_byte_and_request_event_bounds() {
    let events = EventBuffer::default();
    let mut large = stream_response(0);
    large.output = Some("x".repeat(MAX_EVENT_BYTES / 2));
    let encoded = encode(&large).unwrap().len();
    for _ in 0..MAX_BATCH_BYTES / encoded {
        assert!(events.emit(&large));
    }
    assert!(!events.emit(&large));
    assert!(events.state.lock().unwrap().1.len() <= MAX_BATCH_BYTES);

    let events = EventBuffer::default();
    for index in 0..crate::MAX_STREAM_EVENTS + crate::MAX_DELTA_EVENTS {
        assert!(events.emit(&stream_response(index)));
        // Flushing a batch must not reset the per-request event bound.
        events.state.lock().unwrap().1.clear();
    }
    assert!(!events.emit(&stream_response(0)));
}

#[tokio::test]
async fn revocation_during_request_upload_prevents_dispatch() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (url, grant, _, task) = start(Caller::Agent).await;
    let url = reqwest::Url::parse(&url).unwrap();
    let mut stream = tokio::net::TcpStream::connect(url.socket_addrs(|| None).unwrap()[0])
        .await
        .unwrap();
    let body = serde_json::json!({"version": RPC_VERSION, "verb":"trace", "trace":{"id":1,"turn":0}, "event":"plan_ready"}).to_string();
    let header = format!(
        "POST /model/rpc HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer host-session\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes()).await.unwrap();
    tokio::time::timeout(tilo_test_env::hang_bound(), grant.authenticated.notified())
        .await
        .unwrap();
    grant.active.store(false, Ordering::SeqCst);
    stream.write_all(body.as_bytes()).await.unwrap();
    let mut response = String::new();
    tokio::time::timeout(
        tilo_test_env::hang_bound(),
        stream.read_to_string(&mut response),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(response.starts_with("HTTP/1.1 403"), "{response}");
    task.abort();
}
