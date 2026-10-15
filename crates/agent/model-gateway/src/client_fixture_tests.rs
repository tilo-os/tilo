//! The same decoder serves synchronous completion, native and fetch clients.
use super::*;
use agent_provider_config::protocol::{self as neutral, ModelBackend, StreamState, StreamStatus};
use serde_json::json;

struct Fixture {
    events: Vec<Response>,
    final_frame: Response,
}

fn frame(event: Option<&str>, output: &serde_json::Value) -> Response {
    Response {
        ok: true,
        output: Some(output.to_string()),
        configured: None,
        error: None,
        model_error: None,
        event: event.map(str::to_owned),
    }
}

impl Transport for Fixture {
    fn call<'a>(
        &'a self,
        request: Request,
        event: &'a mut (dyn FnMut(Response) -> bool + Send),
    ) -> CallFuture<'a> {
        Box::pin(async move {
            if matches!(request, Request::Complete { stream: true, .. }) {
                for item in &self.events {
                    if !event(item.clone()) {
                        return Err(ModelError::new(ErrorKind::Cancelled));
                    }
                }
            }
            Ok(self.final_frame.clone())
        })
    }
}

fn client(final_frame: Response, events: Vec<Response>) -> ModelClient<Fixture> {
    ModelClient::new(
        Fixture {
            events,
            final_frame,
        },
        "profile".into(),
        "model".into(),
        Duration::from_secs(5),
    )
}

fn answer(ids: &[&str]) -> serde_json::Value {
    let calls: Vec<_> = ids
        .iter()
        .map(|id| {
            json!({
                "type": "tool_call", "id": id, "name": "invoke", "input": {}
            })
        })
        .collect();
    json!({"message": {"role": "assistant", "content": calls}, "stop_reason": "tool_calls"})
}

#[tokio::test]
async fn malformed_final_and_duplicate_ids_never_authorize_partial_tools() {
    for final_body in [json!({}), answer(&["same", "same"]), answer(&[""])] {
        let client = client(
            frame(None, &final_body),
            vec![
                frame(Some("text_delta"), &json!("retained answer")),
                frame(
                    Some("tool_call_delta"),
                    &json!({
                        "index": 0, "id": "partial", "name": "invoke", "delta": "{}"
                    }),
                ),
                // A legacy completed-call frame must never dispatch or duplicate a call.
                frame(
                    Some("tool_call"),
                    &json!({"id": "partial", "name": "invoke", "arguments": "{}"}),
                ),
            ],
        );
        let request = neutral::Conversation::new("system", "question").request(vec![], 64);
        assert_eq!(
            ModelBackend::complete(&client, &request)
                .await
                .unwrap_err()
                .kind,
            ErrorKind::InvalidResponse
        );
        let mut state = StreamState::default();
        let mut terminals = 0;
        let error = client
            .complete_streaming(&request, &mut |event| {
                if matches!(
                    event,
                    StreamEvent::Finished { .. } | StreamEvent::Failed { .. }
                ) {
                    terminals += 1;
                }
                state.apply(event);
                true
            })
            .await
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::InvalidResponse);
        assert_eq!(terminals, 1);
        assert_eq!(state.status, StreamStatus::Failed);
        assert_eq!(state.text, "retained answer");
        assert!(state.response().is_none());
    }
}

#[tokio::test]
async fn malformed_event_poisoning_is_not_reported_as_cancellation() {
    for bad in [
        frame(Some("text_delta"), &json!({"not": "text"})),
        frame(
            Some("tool_call_delta"),
            &json!({"index": 0, "delta": false}),
        ),
        frame(Some("unknown"), &json!("ignored?")),
    ] {
        let client = client(
            frame(None, &answer(&["a"])),
            vec![
                frame(Some("text_delta"), &json!("retained")),
                bad,
                frame(Some("text_delta"), &json!("must not appear")),
            ],
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
        assert_eq!(error.kind, ErrorKind::InvalidResponse);
        assert_eq!(state.text, "retained");
        assert!(state.response().is_none());
    }
}

#[tokio::test]
async fn refused_and_limited_finals_keep_prose_without_exposing_calls() {
    for (reason, status) in [
        ("refusal", StreamStatus::Refused),
        ("output_limit", StreamStatus::Truncated),
    ] {
        let mut final_body = answer(&["partial"]);
        final_body["stop_reason"] = json!(reason);
        final_body["message"]["content"]
            .as_array_mut()
            .unwrap()
            .push(json!({"type": "text", "text": "final partial prose"}));
        let client = client(frame(None, &final_body), vec![]);
        let request = neutral::Conversation::new("system", "question").request(vec![], 64);
        let mut state = StreamState::default();
        client
            .complete_streaming(&request, &mut |event| {
                state.apply(event);
                true
            })
            .await
            .unwrap();
        assert_eq!(state.status, status);
        assert_eq!(state.text, "final partial prose");
        assert!(state.response().is_none());
    }
}

#[test]
fn provider_controlled_messages_and_codes_never_become_diagnostics() {
    let protocol = crate::ClientError::Protocol("malformed sk-secret".into());
    assert!(!protocol.to_string().contains("sk-secret"));
    assert!(!format!("{protocol:?}").contains("sk-secret"));
    for (status, code, kind) in [
        (401, "invalid_api_key", ErrorKind::Authentication),
        (403, "permission_denied", ErrorKind::PermissionDenied),
        (429, "insufficient_quota", ErrorKind::CreditsExhausted),
        (429, "rate_limit", ErrorKind::RateLimited),
        (400, "context_length_exceeded", ErrorKind::ContextLimit),
        (503, "sk-secret", ErrorKind::Provider),
    ] {
        for body in [
            json!({"error": "sk-secret", "code": code}),
            json!({"error": {"message": "sk-secret", "code": code}}),
        ] {
            let failure = crate::openai::Failure::from_http(status, &body.to_string(), None);
            assert_eq!(failure.model_error().kind, kind);
            assert!(!failure.to_string().contains("sk-secret"));
            assert!(!format!("{failure:?}").contains("sk-secret"));
        }
    }
    for (text, kind) in [
        ("AUTHENTICATION sk-secret", ErrorKind::Authentication),
        ("credits exhausted sk-secret", ErrorKind::CreditsExhausted),
        ("rate limit sk-secret", ErrorKind::RateLimited),
        ("context limit sk-secret", ErrorKind::ContextLimit),
        ("request cancelled sk-secret", ErrorKind::Cancelled),
        ("provider refusal sk-secret", ErrorKind::Refused),
        ("network interrupted sk-secret", ErrorKind::Transport),
        ("unknown sk-secret", ErrorKind::Provider),
    ] {
        let error = crate::ClientError::Rejected(text.into());
        assert_eq!(error.model_error().kind, kind);
        assert!(!error.to_string().contains("sk-secret"));
        assert!(!format!("{error:?}").contains("sk-secret"));
    }
}
