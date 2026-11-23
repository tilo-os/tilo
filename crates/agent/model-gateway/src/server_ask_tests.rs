//! An app's turn through the gateway is the person's model on their
//! workspace's server, through the API this computer serves its programs:
//! here a stand-in for that API at the socket a Tilo desktop names
//! (`TILO_API_SOCKET`), answering the ask as the server streams it.
use super::*;
use crate::transport::{FdTransport, Transport};
use http_body_util::BodyExt as _;
use std::os::fd::AsFd;
use tilo_test_env::SpawnUnderEnvLock as _;

/// How the stand-in API answers an ask.
#[derive(Clone, Copy)]
enum Answering {
    /// `words` text events, then the whole turn: a tool call.
    Words(usize),
    /// One text event, then nothing until the reader leaves; then the next
    /// ask is answered with one word.
    HoldFirst,
}

/// The whole turn the stand-in answers, `{"Ok": …}`.
fn whole_turn(text: &str) -> serde_json::Value {
    serde_json::json!({"Ok": {
        "message": {"role": "assistant", "content": [
            {"type": "text", "text": text},
            {"type": "tool_call", "id": "call_1", "name": "lookup", "input": {"count": 2}},
        ]},
        "stop_reason": "tool_calls",
    }})
}

fn event(kind: &str, data: &serde_json::Value) -> bytes::Bytes {
    bytes::Bytes::from(format!("event: {kind}\ndata: {data}\n\n"))
}

/// Serve the stand-in API at `socket`; `left` hears when a held ask's reader
/// went away.
fn serve_api(
    socket: &Path,
    answering: Answering,
    left: tokio::sync::mpsc::UnboundedSender<()>,
) -> tokio::task::JoinHandle<()> {
    let listener = tokio::net::UnixListener::bind(socket).unwrap();
    let asks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let (asks, left) = (Arc::clone(&asks), left.clone());
            tokio::spawn(async move {
                let service = hyper::service::service_fn(
                    move |request: hyper::Request<hyper::body::Incoming>| {
                        let (asks, left) = (Arc::clone(&asks), left.clone());
                        async move {
                            let path = request.uri().path().to_owned();
                            let body = request.into_body().collect().await.unwrap().to_bytes();
                            let (sender, receiver) =
                                tokio::sync::mpsc::channel::<bytes::Bytes>(512);
                            if path == "/v1/resources/account/workspaces/actions/list" {
                                let listed = serde_json::json!({"result": {"workspaces": [
                                    {"id": "theirs", "role": "member"},
                                    {"id": "own", "role": "owner"},
                                ]}});
                                sender.send(listed.to_string().into()).await.unwrap();
                            } else {
                                assert_eq!(path, "/v1/workspaces/own/model/ask");
                                let asked: serde_json::Value =
                                    serde_json::from_slice(&body).unwrap();
                                assert_eq!(asked["max_output_tokens"], 64, "{asked}");
                                let first =
                                    asks.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
                                tokio::spawn(async move {
                                    match answering {
                                        Answering::HoldFirst if first => {
                                            let word = serde_json::json!({"text": "partial"});
                                            sender.send(event("text", &word)).await.unwrap();
                                            sender.closed().await;
                                            let _ = left.send(());
                                        }
                                        Answering::HoldFirst => {
                                            let word = serde_json::json!({"text": "retry answer"});
                                            let _ = sender.send(event("text", &word)).await;
                                            let turn = whole_turn("retry answer");
                                            let _ = sender.send(event("answer", &turn)).await;
                                        }
                                        Answering::Words(words) => {
                                            for _ in 0..words {
                                                let word = serde_json::json!({"text": "word "});
                                                let _ = sender.send(event("text", &word)).await;
                                            }
                                            let turn = whole_turn(&"word ".repeat(words));
                                            let _ = sender.send(event("answer", &turn)).await;
                                        }
                                    }
                                });
                            }
                            let frames =
                                futures_util::stream::unfold(receiver, |mut receiver| async move {
                                    receiver.recv().await.map(|bytes| {
                                        (
                                            Ok::<_, std::convert::Infallible>(
                                                hyper::body::Frame::data(bytes),
                                            ),
                                            receiver,
                                        )
                                    })
                                });
                            Ok::<_, std::convert::Infallible>(hyper::Response::new(
                                http_body_util::StreamBody::new(frames),
                            ))
                        }
                    },
                );
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                    .await;
            });
        }
    })
}

/// A connection of `app` to the gateway, as the launcher hands one over.
fn connect(app: &str) -> (FdTransport, UnixStream, tokio::net::UnixStream) {
    let (mut original, peer) = UnixStream::pair().unwrap();
    write_frame(
        &mut original,
        &serde_json::json!({"protocol":1,"app_id":app}),
    )
    .unwrap();
    let transport = FdTransport::new(original.as_fd()).unwrap();
    peer.set_nonblocking(true).unwrap();
    (
        transport,
        original,
        tokio::net::UnixStream::from_std(peer).unwrap(),
    )
}

fn turn(stream: bool) -> Request {
    let tool = agent_provider_config::protocol::ToolDefinition {
        name: "lookup".into(),
        description: "Look something up.".into(),
        input_schema: serde_json::json!({
            "type":"object","properties":{"count":{"type":"integer"}},"required":["count"]
        }),
    };
    Request::CompleteDefault {
        request: agent_provider_config::protocol::Conversation::new("system", "question")
            .request(vec![tool], 64),
        timeout_ms: 5000,
        stream,
    }
}

/// Every word the model writes reaches the agent as its own delta, while
/// the model writes, and the whole turn after them.
async fn word_by_word() {
    const WORDS: usize = 200;
    let socket = PathBuf::from(std::env::var("TILO_API_SOCKET").unwrap());
    let (left, _) = tokio::sync::mpsc::unbounded_channel();
    let _api = serve_api(&socket, Answering::Words(WORDS), left);
    let (transport, original, peer) = connect(AGENT_APP);
    let http = provider_client().unwrap();
    let gateway = serve_connection(peer, &http);
    let client = async move {
        let mut deltas = 0_usize;
        let answer = transport
            .call(turn(true), &mut |event| {
                if event.event.as_deref() == Some("text_delta") {
                    assert_eq!(event.output.as_deref(), Some("\"word \""));
                    deltas = deltas.saturating_add(1);
                }
                true
            })
            .await
            .unwrap();
        assert!(answer.ok, "{:?}", answer.error);
        assert_eq!(deltas, WORDS);
        let answer: agent_provider_config::protocol::ModelResponse =
            serde_json::from_str(answer.output.as_deref().unwrap()).unwrap();
        assert_eq!(answer.tool_calls().unwrap()[0].input["count"], 2);
        drop(transport);
        drop(original);
    };
    Box::pin(tokio::time::timeout(tilo_test_env::hang_bound(), async {
        tokio::join!(gateway, client);
    }))
    .await
    .unwrap();
}

/// Stopping a turn lets go of its ask at once, and the next turn on the
/// same connection is answered to its end.
async fn stop_then_again() {
    let socket = PathBuf::from(std::env::var("TILO_API_SOCKET").unwrap());
    let (left, mut gone) = tokio::sync::mpsc::unbounded_channel();
    let _api = serve_api(&socket, Answering::HoldFirst, left);
    let (transport, original, peer) = connect(CHAT_APP);
    let http = provider_client().unwrap();
    let gateway = serve_connection(peer, &http);
    let client = async move {
        let chat = || Request::CompleteDefault {
            request: agent_provider_config::protocol::Conversation::new("system", "question")
                .request(vec![], 64),
            timeout_ms: 5000,
            stream: true,
        };
        let mut saw_delta = false;
        let stopped = transport
            .call(chat(), &mut |event| {
                saw_delta |= event.event.as_deref() == Some("text_delta");
                false
            })
            .await
            .unwrap_err();
        assert!(saw_delta);
        assert_eq!(
            stopped.kind,
            agent_provider_config::protocol::ErrorKind::Cancelled
        );
        gone.recv().await.expect("the stopped ask's reader left");
        let again = transport.call(chat(), &mut |_| true).await.unwrap();
        assert!(again.ok, "{:?}", again.error);
        assert!(again.output.as_deref().unwrap().contains("retry answer"));
        drop(transport);
        drop(original);
    };
    Box::pin(tokio::time::timeout(tilo_test_env::hang_bound(), async {
        tokio::join!(gateway, client);
    }))
    .await
    .unwrap();
}

/// With no API on this computer, an app is told there is no model while
/// offline, and nothing else is tried.
async fn offline() {
    let answer = handle(AGENT_APP, turn(false), &provider_client().unwrap()).await;
    assert!(!answer.ok);
    let refused = answer.model_error.expect("a model error");
    assert_eq!(
        refused.kind,
        agent_provider_config::protocol::ErrorKind::Offline
    );
    assert!(
        refused.message.starts_with("No model while offline"),
        "{}",
        refused.message
    );
}

/// Run `name`'s `story` in a process of its own, whose only API is the
/// stand-in at a socket of its own (none with `api` false) and whose
/// computer never logged in.
fn alone(
    name: &str,
    marker: &str,
    api: bool,
    story: fn() -> std::pin::Pin<Box<dyn Future<Output = ()>>>,
) {
    if std::env::var_os(marker).is_some() {
        tilo_rt::block_on(story()).unwrap();
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    child
        .args(["--exact", name, "--nocapture"])
        .env_clear()
        .env("PATH", root.path())
        .env("HOME", root.path())
        .env("XDG_CONFIG_HOME", root.path())
        .env("XDG_STATE_HOME", root.path())
        .env(marker, "1");
    if api {
        child.env("TILO_API_SOCKET", root.path().join("api.sock"));
    }
    let output = child.output_locked().unwrap();
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_turn_reaches_the_agent_word_by_word_from_the_server() {
    alone(
        "service::server_ask_tests::a_turn_reaches_the_agent_word_by_word_from_the_server",
        "GATEWAY_WORD_BY_WORD_CHILD",
        true,
        || Box::pin(word_by_word()),
    );
}

#[test]
fn a_stopped_turn_lets_go_of_its_ask_and_the_next_is_answered() {
    alone(
        "service::server_ask_tests::a_stopped_turn_lets_go_of_its_ask_and_the_next_is_answered",
        "GATEWAY_STOP_CHILD",
        true,
        || Box::pin(stop_then_again()),
    );
}

#[test]
fn with_no_server_to_ask_there_is_no_model_while_offline() {
    alone(
        "service::server_ask_tests::with_no_server_to_ask_there_is_no_model_while_offline",
        "GATEWAY_OFFLINE_CHILD",
        false,
        || Box::pin(offline()),
    );
}
