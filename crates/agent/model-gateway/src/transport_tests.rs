use super::*;
use agent_provider_config::protocol::{self as neutral, ModelBackend, StreamState, StreamStatus};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::task::Poll;

#[test]
fn completion_uses_typed_kind_before_legacy_error_text() {
    for kind in [
        neutral::ErrorKind::InvalidRequest,
        neutral::ErrorKind::InvalidResponse,
        neutral::ErrorKind::CreditsExhausted,
        neutral::ErrorKind::Transport,
    ] {
        let mut response = Response::failure(neutral::ModelError::new(kind));
        response.error = Some("rate limit secret-never-echo".into());
        let error = completion(&response).unwrap_err();
        assert_eq!(error.kind, kind);
        assert!(!error.message.contains("secret-never-echo"));
    }
}

struct BurstStream {
    count: usize,
    fail: bool,
    dropped: Arc<AtomicBool>,
}

struct Dropped(Arc<AtomicBool>);
impl Drop for Dropped {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl Transport for BurstStream {
    fn call<'a>(
        &'a self,
        request: Request,
        event: &'a mut (dyn FnMut(Response) -> bool + Send),
    ) -> CallFuture<'a> {
        Box::pin(async move {
            let _guard = Dropped(self.dropped.clone());
            assert!(matches!(request, Request::Complete { stream: true, .. }));
            for _ in 0..self.count {
                if !event(Response {
                    ok: true,
                    output: Some(serde_json::to_string("x").unwrap()),
                    configured: None,
                    error: None,
                    model_error: None,
                    event: Some("text_delta".into()),
                }) {
                    return Err(neutral::ModelError::new(neutral::ErrorKind::Cancelled));
                }
            }
            // A single poll delivers the whole burst, but not the completion.
            let mut yielded = false;
            std::future::poll_fn(|cx| {
                if yielded {
                    Poll::Ready(())
                } else {
                    yielded = true;
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
            })
            .await;
            if self.fail {
                return Err(neutral::ModelError::new(neutral::ErrorKind::Truncated));
            }
            Ok(Response {
                ok: true,
                output: Some(
                    serde_json::to_string(&neutral::ModelResponse {
                        message: neutral::Message::text(
                            neutral::Role::Assistant,
                            "x".repeat(self.count),
                        ),
                        stop_reason: neutral::StopReason::EndTurn,
                        usage: neutral::Usage::default(),
                    })
                    .unwrap(),
                ),
                configured: None,
                error: None,
                model_error: None,
                event: None,
            })
        })
    }
}

#[tokio::test(flavor = "current_thread")]
async fn backend_stream_delivers_bursts_to_local_sink_before_completion() {
    for fail in [false, true] {
        let dropped = Arc::new(AtomicBool::new(false));
        let client = ModelClient::new(
            BurstStream {
                count: 128,
                fail,
                dropped: dropped.clone(),
            },
            "profile".into(),
            "model".into(),
            Duration::from_secs(5),
        );
        let request = neutral::Conversation::new("system", "question").request(vec![], 64);
        // Rc proves that the trait sink need not be Send.
        let events = Rc::new(RefCell::new(Vec::new()));
        let captured = events.clone();
        let backend: &dyn ModelBackend = &client;
        let mut operation = backend.stream(
            &request,
            Box::new(move |event| captured.borrow_mut().push(event)),
        );
        std::future::poll_fn(|cx| {
            assert!(operation.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        assert_eq!(events.borrow().len(), 128);
        assert!(!dropped.load(Ordering::SeqCst));
        let result = operation.await;
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(events.borrow().len(), 129);
        let mut state = StreamState::default();
        for event in events.borrow().iter() {
            state.apply(event);
        }
        assert_eq!(state.text, "x".repeat(128));
        if fail {
            assert_eq!(result.unwrap_err().kind, neutral::ErrorKind::Truncated);
            assert_eq!(state.status, StreamStatus::Truncated);
            assert!(matches!(
                events.borrow().last(),
                Some(neutral::StreamEvent::Failed { .. })
            ));
            assert!(state.response().is_none());
        } else {
            assert_eq!(state.response(), Some(&result.unwrap()));
            assert!(matches!(
                events.borrow().last(),
                Some(neutral::StreamEvent::Finished { .. })
            ));
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn dropping_backend_stream_drops_in_flight_transport() {
    let dropped = Arc::new(AtomicBool::new(false));
    let client = ModelClient::new(
        BurstStream {
            count: 1,
            fail: false,
            dropped: dropped.clone(),
        },
        "profile".into(),
        "model".into(),
        Duration::from_secs(5),
    );
    let request = neutral::Conversation::new("system", "question").request(vec![], 64);
    let events = Rc::new(RefCell::new(Vec::new()));
    let captured = events.clone();
    let mut operation = ModelBackend::stream(
        &client,
        &request,
        Box::new(move |event| captured.borrow_mut().push(event)),
    );
    std::future::poll_fn(|cx| {
        assert!(operation.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    drop(operation);
    assert!(dropped.load(Ordering::SeqCst));
    assert_eq!(events.borrow().len(), 1);
    assert!(matches!(
        events.borrow()[0],
        neutral::StreamEvent::TextDelta { .. }
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn backend_stream_queue_overflow_fails_explicitly_without_success() {
    let capacity = crate::MAX_STREAM_EVENTS + crate::MAX_DELTA_EVENTS;
    let client = ModelClient::new(
        BurstStream {
            count: capacity + 1,
            fail: false,
            dropped: Arc::default(),
        },
        "profile".into(),
        "model".into(),
        Duration::from_secs(5),
    );
    let request = neutral::Conversation::new("system", "question").request(vec![], 64);
    let events = Rc::new(RefCell::new(Vec::new()));
    let captured = events.clone();
    let error = ModelBackend::stream(
        &client,
        &request,
        Box::new(move |event| captured.borrow_mut().push(event)),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind, neutral::ErrorKind::Transport);
    assert!(error.message.contains("queue capacity"));
    assert_eq!(events.borrow().len(), capacity + 1);
    assert!(
        events.borrow()[..capacity]
            .iter()
            .all(|event| matches!(event, neutral::StreamEvent::TextDelta { .. }))
    );
    assert_eq!(
        events.borrow().last(),
        Some(&neutral::StreamEvent::Failed { error })
    );
}

struct CutStream;
impl Transport for CutStream {
    fn call<'a>(
        &'a self,
        _: Request,
        event: &'a mut (dyn FnMut(Response) -> bool + Send),
    ) -> CallFuture<'a> {
        Box::pin(async move {
            for (kind, text) in [
                ("text_delta", "partial text"),
                ("reasoning_delta", "provider summary"),
            ] {
                if !event(Response {
                    ok: true,
                    output: Some(serde_json::to_string(text).unwrap()),
                    configured: None,
                    error: None,
                    model_error: None,
                    event: Some(kind.into()),
                }) {
                    return Err(neutral::ModelError::new(neutral::ErrorKind::Cancelled));
                }
            }
            Err(neutral::ModelError::new(neutral::ErrorKind::Truncated))
        })
    }
}

#[tokio::test(flavor = "current_thread")]
async fn failed_stream_retains_presentation_without_dispatchable_response() {
    let client = ModelClient::new(
        CutStream,
        "profile".into(),
        "model".into(),
        Duration::from_secs(5),
    );
    let request = neutral::Conversation::new("system", "question").request(vec![], 64);
    let mut state = StreamState::default();
    let error = client
        .complete_streaming(&request, &mut |event| {
            state.apply(event);
            true
        })
        .await
        .unwrap_err();
    assert_eq!(error.kind, neutral::ErrorKind::Truncated);
    assert_eq!(state.status, StreamStatus::Truncated);
    assert_eq!(state.text, "partial text");
    assert_eq!(state.reasoning, "provider summary");
    assert!(state.response().is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn cancelled_stream_is_not_finished_and_retains_partial_text() {
    let client = ModelClient::new(
        CutStream,
        "profile".into(),
        "model".into(),
        Duration::from_secs(5),
    );
    let request = neutral::Conversation::new("system", "question").request(vec![], 64);
    let mut state = StreamState::default();
    let error = client
        .complete_streaming(&request, &mut |event| {
            state.apply(event);
            matches!(event, neutral::StreamEvent::Failed { .. })
        })
        .await
        .unwrap_err();
    assert_eq!(error.kind, neutral::ErrorKind::Cancelled);
    assert_eq!(state.status, StreamStatus::Cancelled);
    assert_eq!(state.text, "partial text");
    assert!(state.response().is_none());
}
