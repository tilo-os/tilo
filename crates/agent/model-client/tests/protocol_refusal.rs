#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    reason = "fixture and protocol failures must fail the test"
)]

use agent_provider_config::{
    management::ProviderAdmin,
    protocol::{
        Content, Conversation, ErrorKind, Message, ModelResponse, Role, StopReason, StreamState,
        StreamStatus, ToolCall, Usage,
    },
};
use std::{os::fd::AsFd, os::unix::net::UnixStream, time::Duration};
use tilo_model_client::{
    FRAMING, Frame, RPC_VERSION, Request, Response,
    transport::{AdministrationClient, FdTransport, ModelClient},
};

#[test]
fn cancellation_envelope_keeps_rpc3_wire_format() {
    let frame = Frame {
        version: RPC_VERSION,
        value: Request::Cancellable {
            id: 7,
            request: Box::new(Request::GetProviderConfig),
        },
    };
    let wire = serde_json::to_string(&frame).unwrap();
    assert_eq!(
        wire,
        r#"{"version":3,"verb":"cancellable","id":7,"request":{"verb":"get_provider_config"}}"#
    );
    let decoded: Frame<Request> = serde_json::from_str(&wire).unwrap();
    assert!(
        matches!(decoded.value, Request::Cancellable { id: 7, request } if matches!(*request, Request::GetProviderConfig))
    );
}

async fn receive(peer: &mut tokio::net::UnixStream) -> Request {
    let bytes = FRAMING.read_async(peer).await.unwrap().unwrap();
    let frame: Frame<Request> = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(frame.version, 3);
    frame.value
}

async fn send(peer: &mut tokio::net::UnixStream, response: Response) {
    FRAMING
        .write_async(peer, &serde_json::to_vec(&response).unwrap())
        .await
        .unwrap();
}

fn success(output: String) -> Response {
    Response {
        ok: true,
        output: Some(output),
        configured: None,
        error: None,
        model_error: None,
        event: None,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn administration_refusal_and_inference_share_connection_without_sharing_authority() {
    let (app, peer) = UnixStream::pair().unwrap();
    let transport = FdTransport::new(app.as_fd()).unwrap();
    drop(app);
    peer.set_nonblocking(true).unwrap();
    let mut peer = tokio::net::UnixStream::from_std(peer).unwrap();
    let administration = AdministrationClient::new(transport.clone());
    let models = ModelClient::default_model(transport, Duration::from_secs(1));
    let server = async {
        for id in [1, 2] {
            assert!(matches!(
                receive(&mut peer).await,
                Request::CancellationSupported
            ));
            send(&mut peer, success("cancel-v1".into())).await;
            let Request::Cancellable {
                id: actual,
                request,
            } = receive(&mut peer).await
            else {
                panic!("missing cancellation envelope")
            };
            assert_eq!(actual, id);
            if id == 1 {
                assert!(matches!(*request, Request::GetProviderConfig));
                send(
                    &mut peer,
                    Response::failure(agent_provider_config::protocol::ModelError::new(
                        ErrorKind::PermissionDenied,
                    )),
                )
                .await;
            } else {
                assert!(matches!(
                    *request,
                    Request::CompleteDefault { stream: true, .. }
                ));
                let refused = ModelResponse {
                    message: Message {
                        role: Role::Assistant,
                        content: vec![
                            Content::Text {
                                text: "Not permitted".into(),
                            },
                            Content::ToolCall(ToolCall {
                                id: "inert".into(),
                                name: "invoke".into(),
                                input: serde_json::json!({}),
                                extra_content: None,
                            }),
                        ],
                    },
                    stop_reason: StopReason::Refusal,
                    usage: Usage::default(),
                };
                send(&mut peer, success(serde_json::to_string(&refused).unwrap())).await;
            }
        }
    };
    let client = async {
        assert_eq!(
            administration.get_config().await.unwrap_err().kind,
            ErrorKind::PermissionDenied
        );
        let request = Conversation::new("system", "question").request(vec![], 64);
        let mut state = StreamState::default();
        let response = models
            .complete_streaming(&request, &mut |event| {
                state.apply(event);
                true
            })
            .await
            .unwrap();
        assert_eq!(response.tool_calls().unwrap_err().kind, ErrorKind::Refused);
        assert_eq!(state.status, StreamStatus::Refused);
        assert_eq!(state.text, "Not permitted");
        assert!(state.response().is_none());
    };
    tokio::time::timeout(tilo_test_env::hang_bound(), async {
        tokio::join!(server, client);
    })
    .await
    .unwrap();
}
