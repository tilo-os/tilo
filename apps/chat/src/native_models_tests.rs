//! Actual native Chat backend and descriptor transport, with a wire-level peer.
//! The peer below is not the gateway service or an upstream provider.
#![allow(
    clippy::expect_used,
    reason = "wire-level test fixture helpers fail immediately on malformed frames and broken descriptors"
)]
use crate::{ChatApp, identity::LocalIdentity};
use std::{
    io::{Read, Write},
    os::{fd::AsFd, unix::net::UnixStream},
    time::{Duration, Instant},
};
use ui::models::{Message, ModelResponse, Models, Role, StopReason, Usage};

fn read_frame(peer: &mut UnixStream) -> serde_json::Value {
    let mut length = [0; 4];
    peer.read_exact(&mut length).expect("read frame length");
    let length = u32::from_le_bytes(length) as usize;
    assert!(length < 2 * 1024 * 1024);
    let mut body = vec![0; length];
    peer.read_exact(&mut body).expect("read frame body");
    serde_json::from_slice(&body).expect("decode gateway frame")
}

fn write_frame(peer: &mut UnixStream, frame: &serde_json::Value) {
    let body = serde_json::to_vec(frame).expect("encode gateway frame");
    peer.write_all(&(body.len() as u32).to_le_bytes())
        .expect("write frame length");
    peer.write_all(&body).expect("write frame body");
}

fn complete_request(peer: &mut UnixStream) -> u64 {
    let negotiation = read_frame(peer);
    assert_eq!(
        negotiation.get("verb").expect("negotiation verb"),
        "cancellation_supported"
    );
    write_frame(
        peer,
        &serde_json::json!({"ok": true, "output": "cancel-v1"}),
    );
    let frame = read_frame(peer);
    assert_eq!(frame.get("verb").expect("request verb"), "cancellable");
    let request = frame.get("request").expect("completion request");
    assert_eq!(request.get("verb").expect("completion verb"), "complete");
    assert_eq!(request.get("profile_id").expect("profile"), "chat-profile");
    assert_eq!(request.get("model_id").expect("model"), "chat-model");
    assert_eq!(request.get("stream").expect("stream flag"), true);
    assert_eq!(
        request
            .pointer("/request/tool_choice")
            .expect("tool choice"),
        "none"
    );
    assert_eq!(
        request.pointer("/request/tools").expect("tools"),
        &serde_json::json!([])
    );
    assert_eq!(
        request
            .pointer("/request/messages/0/content/0/text")
            .expect("prompt text"),
        "Question"
    );
    assert!(request.get("caller").is_none());
    assert!(request.get("app_id").is_none());
    frame
        .get("id")
        .and_then(serde_json::Value::as_u64)
        .expect("request id")
}

fn answer(peer: &mut UnixStream, text: &str) {
    let answer = ModelResponse {
        message: Message::text(Role::Assistant, text),
        stop_reason: StopReason::EndTurn,
        usage: Usage::default(),
    };
    write_frame(
        peer,
        &serde_json::json!({
            "ok": true, "output": serde_json::to_string(&answer).expect("encode answer")
        }),
    );
}

fn chat(client: &UnixStream) -> (ChatApp, automerge::AutoCommit) {
    let backend = tilo_model_gateway::runtime_client::from_descriptor_selection(
        client.as_fd(),
        "chat-profile".into(),
        "chat-model".into(),
    )
    .expect("bind Chat model descriptor");
    let models = Models::new(backend);
    let identity = LocalIdentity::demo();
    let mut doc = automerge::AutoCommit::new();
    crate::testing::starter(&mut doc, &identity.id, &identity.name, &identity.initials);
    crate::testing::say(&mut doc, &identity.id, "c_general", "Question");
    let mut app = ChatApp::new(
        identity.clone(),
        crate::app::ChatRuntime::Unavailable,
        tilo_content::ContentStore::memory(),
    )
    .with_models(models);
    app.attach_framework(ui::framework::Dispatcher::detached(
        "so.tilo.chat",
        &identity.id,
    ));
    app.reload(&doc);
    app.ensure_selection();
    app.reload(&doc);
    (app, doc)
}

fn pump_until(
    app: &mut ChatApp,
    doc: &mut automerge::AutoCommit,
    done: impl Fn(&ChatApp, &automerge::AutoCommit) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.pump_model();
        app.land(doc);
        app.reload(doc);
        if done(app, doc) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "native model attempt must make progress"
        );
        std::thread::yield_now();
    }
}

#[test]
fn native_chat_descriptor_setup_completes_actual_conversation() {
    let (client, mut gateway) = UnixStream::pair().unwrap();
    gateway
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let server = std::thread::spawn(move || {
        complete_request(&mut gateway);
        answer(&mut gateway, "Native gateway answer");
    });
    let (mut app, mut doc) = chat(&client);
    drop(client);
    assert_eq!(
        app.model_command("c_general", "ask").unwrap()["started"],
        true
    );
    pump_until(&mut app, &mut doc, |app, _| !app.host_model.needs_frame());
    server.join().unwrap();
    let messages = crate::testing::messages(&doc, "c_general");
    let reply = messages.last().unwrap();
    assert_eq!(reply.text, "Native gateway answer");
    assert_eq!(
        reply.work.as_ref().unwrap().stream.as_ref().unwrap().finish,
        Some(crate::agent_stream::AgentFinish::Complete)
    );
}

#[test]
fn native_chat_stop_retains_partial_and_retries_on_same_descriptor() {
    let (client, mut gateway) = UnixStream::pair().unwrap();
    gateway
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let server = std::thread::spawn(move || {
        let id = complete_request(&mut gateway);
        write_frame(
            &mut gateway,
            &serde_json::json!({
                "ok": true, "event": "text_delta", "output": serde_json::to_string("partial").unwrap()
            }),
        );
        let cancel = read_frame(&mut gateway);
        assert_eq!(cancel["verb"], "cancel");
        assert_eq!(cancel["id"], id);
        // An in-flight delta must be drained, not rendered or given to retry.
        write_frame(
            &mut gateway,
            &serde_json::json!({
                "ok": true, "event": "text_delta", "output": serde_json::to_string("discarded").unwrap()
            }),
        );
        write_frame(
            &mut gateway,
            &serde_json::json!({
                "ok": true, "output": format!("cancelled:{id}")
            }),
        );
        assert_ne!(complete_request(&mut gateway), id);
        answer(&mut gateway, "Retry answer");
    });
    let (mut app, mut doc) = chat(&client);
    drop(client);
    let first = app.model_command("c_general", "ask").unwrap();
    pump_until(&mut app, &mut doc, |_, doc| {
        crate::testing::messages(doc, "c_general")
            .last()
            .is_some_and(|reply| reply.text == "partial")
    });
    assert_eq!(
        app.model_command("c_general", "stop").unwrap()["stopped"],
        true
    );
    app.pump_model();
    app.land(&mut doc);
    app.reload(&doc);
    let retry = app.model_command("c_general", "retry").unwrap();
    assert_eq!(retry["started"], true);
    assert_ne!(retry["reply"], first["reply"]);
    pump_until(&mut app, &mut doc, |app, _| !app.host_model.needs_frame());
    server.join().unwrap();
    let messages = crate::testing::messages(&doc, "c_general");
    let first = messages
        .iter()
        .find(|message| message.id == first["reply"].as_str().unwrap())
        .unwrap();
    assert_eq!(first.text, "partial");
    assert_eq!(
        first.work.as_ref().unwrap().stream.as_ref().unwrap().finish,
        Some(crate::agent_stream::AgentFinish::Cancelled)
    );
    let second = messages.last().unwrap();
    assert_eq!(second.text, "Retry answer");
    assert_eq!(
        second
            .work
            .as_ref()
            .unwrap()
            .stream
            .as_ref()
            .unwrap()
            .finish,
        Some(crate::agent_stream::AgentFinish::Complete)
    );
}
