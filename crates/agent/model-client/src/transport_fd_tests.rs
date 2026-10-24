use super::*;

fn request(timeout_ms: u64) -> Request {
    Request::Complete {
        profile_id: "profile".into(),
        model_id: "model".into(),
        request: agent_provider_config::protocol::Conversation::new("system", "question")
            .request(vec![], 64),
        timeout_ms,
        stream: true,
    }
}

fn response(output: &str, event: Option<&str>) -> Response {
    Response {
        ok: true,
        output: Some(output.into()),
        configured: None,
        error: None,
        model_error: None,
        event: event.map(str::to_owned),
    }
}

fn pair() -> (UnixStream, FdTransport, tokio::net::UnixStream) {
    let (original, peer) = UnixStream::pair().unwrap();
    let transport = FdTransport::new(original.as_fd()).unwrap();
    peer.set_nonblocking(true).unwrap();
    (
        original,
        transport,
        tokio::net::UnixStream::from_std(peer).unwrap(),
    )
}

async fn received_request(peer: &mut tokio::net::UnixStream) -> (u64, Request) {
    // The first bytes must be the framed RPC, not a caller-forged identity.
    let body = crate::FRAMING.read_async(peer).await.unwrap().unwrap();
    let frame: crate::Frame<Request> = serde_json::from_slice(&body).unwrap();
    assert_eq!(frame.version, crate::RPC_VERSION);
    assert!(matches!(frame.value, Request::CancellationSupported));
    crate::FRAMING
        .write_async(
            peer,
            &serde_json::to_vec(&response("cancel-v1", None)).unwrap(),
        )
        .await
        .unwrap();
    let body = crate::FRAMING.read_async(peer).await.unwrap().unwrap();
    let frame: crate::Frame<Request> = serde_json::from_slice(&body).unwrap();
    let Request::Cancellable { id, request } = frame.value else {
        panic!("missing operation identity")
    };
    (id, *request)
}

async fn received(peer: &mut tokio::net::UnixStream) -> u64 {
    let (id, request) = received_request(peer).await;
    assert!(matches!(request, Request::Complete { .. }));
    id
}

#[tokio::test(flavor = "current_thread")]
async fn abandoned_administration_drains_committed_response_and_cancel_barrier() {
    use agent_provider_config::management::{CredentialStatus, ProviderAdmin};
    let (_original, transport, mut peer) = pair();
    let admin = crate::transport::AdministrationClient::new(transport);
    let (started, received) = tokio::sync::oneshot::channel();
    let server = async {
        let (_, request) = received_request(&mut peer).await;
        assert!(matches!(request, Request::SetApiKey { .. }));
        started.send(()).unwrap();
        let body = crate::FRAMING.read_async(&mut peer).await.unwrap().unwrap();
        let frame: crate::Frame<Request> = serde_json::from_slice(&body).unwrap();
        let Request::Cancel { id } = frame.value else {
            panic!("missing Stop")
        };
        // A durable write finishes before the barrier; its abandoned success
        // must not become the next credential status response.
        for output in ["committed".to_string(), format!("cancelled:{id}")] {
            crate::FRAMING
                .write_async(
                    &mut peer,
                    &serde_json::to_vec(&response(&output, None)).unwrap(),
                )
                .await
                .unwrap();
        }
        let (_, request) = received_request(&mut peer).await;
        assert!(matches!(request, Request::CredentialStatus { .. }));
        crate::FRAMING
            .write_async(
                &mut peer,
                &serde_json::to_vec(&response("\"configured\"", None)).unwrap(),
            )
            .await
            .unwrap();
    };
    let client = async {
        let mut operation = admin.set_api_key("profile".into(), "write-only-key".into());
        tokio::select! {
            result = &mut operation => panic!("completed before Stop: {result:?}"),
            result = received => result.unwrap(),
        }
        drop(operation);
        assert_eq!(
            admin.credential_status("profile".into()).await.unwrap(),
            CredentialStatus::Configured
        );
    };
    tokio::time::timeout(tilo_test_env::hang_bound(), async {
        tokio::join!(server, client);
    })
    .await
    .unwrap();
}

async fn acknowledge_cancel(peer: &mut tokio::net::UnixStream) {
    let body = crate::FRAMING.read_async(peer).await.unwrap().unwrap();
    let frame: crate::Frame<Request> = serde_json::from_slice(&body).unwrap();
    let Request::Cancel { id } = frame.value else {
        panic!("missing cancellation")
    };
    crate::FRAMING
        .write_async(
            peer,
            &serde_json::to_vec(&response(&format!("cancelled:{id}"), None)).unwrap(),
        )
        .await
        .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn native_instances_keep_their_channels_and_support_repeated_calls() {
    let (original_a, a, mut peer_a) = pair();
    let (original_b, b, mut peer_b) = pair();
    drop(original_a);
    drop(original_b);
    let server = async {
        for _ in 0..2 {
            received(&mut peer_a).await;
            crate::FRAMING
                .write_async(
                    &mut peer_a,
                    &serde_json::to_vec(&response("a", None)).unwrap(),
                )
                .await
                .unwrap();
            received(&mut peer_b).await;
            crate::FRAMING
                .write_async(
                    &mut peer_b,
                    &serde_json::to_vec(&response("b", None)).unwrap(),
                )
                .await
                .unwrap();
        }
    };
    let client = async {
        for _ in 0..2 {
            assert_eq!(
                a.call(request(1000), &mut |_| true)
                    .await
                    .unwrap()
                    .output
                    .as_deref(),
                Some("a")
            );
            assert_eq!(
                b.call(request(1000), &mut |_| true)
                    .await
                    .unwrap()
                    .output
                    .as_deref(),
                Some("b")
            );
        }
    };
    tokio::join!(server, client);
}

#[tokio::test(flavor = "current_thread")]
async fn eof_is_terminal_and_never_yields_a_success() {
    let (_original, transport, mut peer) = pair();
    let server = async {
        received(&mut peer).await;
        drop(peer);
    };
    let client = async {
        let error = transport
            .call(request(1000), &mut |_| true)
            .await
            .unwrap_err();
        assert_eq!(
            error.kind,
            agent_provider_config::protocol::ErrorKind::Truncated
        );
        assert_eq!(
            transport
                .call(request(1000), &mut |_| true)
                .await
                .unwrap_err()
                .kind,
            agent_provider_config::protocol::ErrorKind::Unavailable
        );
    };
    tokio::join!(server, client);
}

#[tokio::test(flavor = "current_thread")]
async fn callback_stop_then_retry_reuses_the_authenticated_capability() {
    let (_original, transport, mut peer) = pair();
    let server = async {
        received(&mut peer).await;
        crate::FRAMING
            .write_async(
                &mut peer,
                &serde_json::to_vec(&response("partial", Some("text_delta"))).unwrap(),
            )
            .await
            .unwrap();
        acknowledge_cancel(&mut peer).await;
        received(&mut peer).await;
        crate::FRAMING
            .write_async(
                &mut peer,
                &serde_json::to_vec(&response("retry", None)).unwrap(),
            )
            .await
            .unwrap();
    };
    let client = async {
        assert_eq!(
            transport
                .call(request(1000), &mut |_| false)
                .await
                .unwrap_err()
                .kind,
            agent_provider_config::protocol::ErrorKind::Cancelled
        );
        assert_eq!(
            transport
                .call(request(1000), &mut |_| true)
                .await
                .unwrap()
                .output
                .as_deref(),
            Some("retry")
        );
    };
    tokio::join!(server, client);
}

#[tokio::test(flavor = "current_thread")]
async fn dropping_active_future_cancels_and_retry_cannot_consume_old_reply() {
    let (_original, transport, mut peer) = pair();
    let mut callback = |_| true;
    let mut call = transport.call(request(1000), &mut callback);
    tokio::select! {
        result = &mut call => panic!("request unexpectedly finished: {result:?}"),
        _ = received(&mut peer) => {}
    }
    drop(call);
    let server = async {
        // A final response may already have raced Stop. It is not the barrier.
        crate::FRAMING
            .write_async(
                &mut peer,
                &serde_json::to_vec(&response("stale", None)).unwrap(),
            )
            .await
            .unwrap();
        acknowledge_cancel(&mut peer).await;
        received(&mut peer).await;
        crate::FRAMING
            .write_async(
                &mut peer,
                &serde_json::to_vec(&response("retry", None)).unwrap(),
            )
            .await
            .unwrap();
    };
    let client = async {
        assert_eq!(
            transport
                .call(request(1000), &mut |_| true)
                .await
                .unwrap()
                .output
                .as_deref(),
            Some("retry")
        );
    };
    tokio::join!(server, client);
}

#[tokio::test(flavor = "current_thread")]
async fn queued_deadline_does_not_cancel_the_active_call() {
    let (_original, transport, mut peer) = pair();
    let mut callback = |_| true;
    let mut active = transport.call(request(1000), &mut callback);
    tokio::select! {
        result = &mut active => panic!("request unexpectedly finished: {result:?}"),
        _ = received(&mut peer) => {}
    }
    assert_eq!(
        transport
            .call(request(10), &mut |_| true)
            .await
            .unwrap_err()
            .kind,
        agent_provider_config::protocol::ErrorKind::TimedOut
    );
    crate::FRAMING
        .write_async(
            &mut peer,
            &serde_json::to_vec(&response("active", None)).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(active.await.unwrap().output.as_deref(), Some("active"));
}

#[test]
fn a_regular_file_cannot_become_a_model_capability() {
    let file = tempfile::tempfile().unwrap();
    assert!(FdTransport::new(file.as_fd()).is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn older_gateway_is_rejected_before_any_inference() {
    use tokio::io::AsyncReadExt;
    let (_original, transport, mut peer) = pair();
    let server = async {
        let body = crate::FRAMING.read_async(&mut peer).await.unwrap().unwrap();
        let frame: crate::Frame<Request> = serde_json::from_slice(&body).unwrap();
        assert!(matches!(frame.value, Request::CancellationSupported));
        let mut unsupported = response("unsupported", None);
        unsupported.ok = false;
        crate::FRAMING
            .write_async(&mut peer, &serde_json::to_vec(&unsupported).unwrap())
            .await
            .unwrap();
        let mut byte = [0];
        assert_eq!(
            peer.read(&mut byte).await.unwrap(),
            0,
            "must not send paid inference"
        );
    };
    let client = async {
        assert_eq!(
            transport
                .call(request(1000), &mut |_| true)
                .await
                .unwrap_err()
                .kind,
            agent_provider_config::protocol::ErrorKind::Unsupported
        );
        assert_eq!(
            transport
                .call(request(1000), &mut |_| true)
                .await
                .unwrap_err()
                .kind,
            agent_provider_config::protocol::ErrorKind::Unavailable
        );
    };
    tokio::join!(server, client);
}

#[tokio::test(flavor = "current_thread")]
async fn corrupt_response_invalidates_capability_instead_of_reusing_frames() {
    let (_original, transport, mut peer) = pair();
    let server = async {
        received(&mut peer).await;
        crate::FRAMING
            .write_async(&mut peer, b"not json")
            .await
            .unwrap();
    };
    let client = async {
        assert_eq!(
            transport
                .call(request(1000), &mut |_| true)
                .await
                .unwrap_err()
                .kind,
            agent_provider_config::protocol::ErrorKind::InvalidResponse
        );
        assert_eq!(
            transport
                .call(request(1000), &mut |_| true)
                .await
                .unwrap_err()
                .kind,
            agent_provider_config::protocol::ErrorKind::Unavailable
        );
    };
    tokio::join!(server, client);
}

#[tokio::test(flavor = "current_thread")]
async fn wrong_cancellation_barrier_never_authorizes_reuse() {
    let (_original, transport, mut peer) = pair();
    let server = async {
        let id = received(&mut peer).await;
        crate::FRAMING
            .write_async(
                &mut peer,
                &serde_json::to_vec(&response("partial", Some("text_delta"))).unwrap(),
            )
            .await
            .unwrap();
        let body = crate::FRAMING.read_async(&mut peer).await.unwrap().unwrap();
        let frame: crate::Frame<Request> = serde_json::from_slice(&body).unwrap();
        assert!(matches!(frame.value, Request::Cancel { id: cancel } if cancel == id));
        crate::FRAMING
            .write_async(
                &mut peer,
                &serde_json::to_vec(&response(&format!("cancelled:{}", id + 1), None)).unwrap(),
            )
            .await
            .unwrap();
        drop(peer);
    };
    let client = async {
        assert_eq!(
            transport
                .call(request(1000), &mut |_| false)
                .await
                .unwrap_err()
                .kind,
            agent_provider_config::protocol::ErrorKind::Cancelled
        );
        assert_eq!(
            transport
                .call(request(1000), &mut |_| true)
                .await
                .unwrap_err()
                .kind,
            agent_provider_config::protocol::ErrorKind::Unavailable
        );
    };
    tokio::join!(server, client);
}

#[tokio::test(flavor = "current_thread")]
async fn stop_during_partial_response_preserves_the_frame_reader() {
    use tokio::io::AsyncWriteExt;
    let (_original, transport, mut peer) = pair();
    let (partial, partial_read) = tokio::sync::oneshot::channel();
    let server = async {
        received(&mut peer).await;
        let body = serde_json::to_vec(&response("stale", Some("text_delta"))).unwrap();
        let prefix = u32::try_from(body.len()).unwrap().to_le_bytes();
        peer.write_all(&prefix[..2]).await.unwrap();
        partial.send(()).unwrap();
        let cancel = crate::FRAMING.read_async(&mut peer).await.unwrap().unwrap();
        let frame: crate::Frame<Request> = serde_json::from_slice(&cancel).unwrap();
        let Request::Cancel { id } = frame.value else {
            panic!("missing cancel")
        };
        peer.write_all(&prefix[2..]).await.unwrap();
        peer.write_all(&body).await.unwrap();
        crate::FRAMING
            .write_async(
                &mut peer,
                &serde_json::to_vec(&response(&format!("cancelled:{id}"), None)).unwrap(),
            )
            .await
            .unwrap();
        received(&mut peer).await;
        crate::FRAMING
            .write_async(
                &mut peer,
                &serde_json::to_vec(&response("retry", None)).unwrap(),
            )
            .await
            .unwrap();
    };
    let client = async {
        let mut callback = |_| true;
        let mut call = transport.call(request(1000), &mut callback);
        tokio::select! {
            result = &mut call => panic!("unexpected result: {result:?}"),
            _ = partial_read => {}
        }
        drop(call);
        assert_eq!(
            transport
                .call(request(1000), &mut |_| true)
                .await
                .unwrap()
                .output
                .as_deref(),
            Some("retry")
        );
    };
    tokio::join!(server, client);
}

#[tokio::test(flavor = "current_thread")]
async fn deadline_cancels_without_blocking_the_runtime() {
    let (_original, transport, mut peer) = pair();
    let server = async {
        received(&mut peer).await;
        acknowledge_cancel(&mut peer).await;
    };
    let client = async {
        assert_eq!(
            transport
                .call(request(25), &mut |_| true)
                .await
                .unwrap_err()
                .kind,
            agent_provider_config::protocol::ErrorKind::TimedOut
        );
    };
    tokio::join!(server, client);
}
