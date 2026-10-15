use super::*;
use std::{cell::Cell, task::Poll};
use ui::models::{ModelBackend, ModelResponse, StopReason, StreamSink, Usage};

struct Backend {
    text: &'static str,
    finish: Cell<bool>,
    error: Cell<Option<ui::models::ErrorKind>>,
    stop: Cell<StopReason>,
    requests: RefCell<Vec<ModelRequest>>,
}
impl ModelBackend for Backend {
    fn complete<'a>(&'a self, _: &'a ModelRequest) -> CompletionFuture<'a> {
        panic!("Chat must use the streaming host capability")
    }
    fn stream<'a>(
        &'a self,
        request: &'a ModelRequest,
        mut sink: StreamSink<'a>,
    ) -> CompletionFuture<'a> {
        self.requests.borrow_mut().push(request.clone());
        let mut emitted = false;
        Box::pin(std::future::poll_fn(move |_| {
            if !emitted {
                sink(StreamEvent::TextDelta {
                    text: self.text.into(),
                });
                sink(StreamEvent::ReasoningDelta {
                    text: "Provider summary".into(),
                });
                sink(StreamEvent::ToolCallDelta {
                    index: 0,
                    id: Some("inert".into()),
                    name: Some("delete".into()),
                    arguments: "{}".into(),
                });
                emitted = true;
            }
            if !self.finish.get() {
                return Poll::Pending;
            }
            if let Some(kind) = self.error.get() {
                return Poll::Ready(Err(ui::models::ModelError {
                    kind,
                    message: "Host refused model access".into(),
                }));
            }
            Poll::Ready(Ok(ModelResponse {
                message: ui::models::Message::text(Role::Assistant, self.text),
                stop_reason: self.stop.get(),
                usage: Usage::default(),
            }))
        }))
    }
}
fn backend(text: &'static str) -> Rc<Backend> {
    Rc::new(Backend {
        text,
        finish: Cell::new(false),
        error: Cell::new(None),
        stop: Cell::new(StopReason::OutputLimit),
        requests: RefCell::new(vec![]),
    })
}

fn chat(backend: Rc<Backend>) -> (ChatApp, automerge::AutoCommit) {
    let mut doc = automerge::AutoCommit::new();
    crate::testing::starter(&mut doc, crate::model::ME, "You", "Y");
    crate::testing::say(&mut doc, crate::model::ME, "c_general", "Question");
    let mut app = ChatApp::new_ephemeral().with_models(Models::new(backend));
    app.reload(&doc);
    app.ensure_selection();
    app.reload(&doc);
    (app, doc)
}

fn pump(app: &mut ChatApp, doc: &mut automerge::AutoCommit) {
    app.pump_model();
    app.land(doc);
    app.reload(doc);
}

fn reply(doc: &automerge::AutoCommit, id: &serde_json::Value) -> crate::model::Message {
    crate::projection::read(doc)
        .message_row(id.as_str().unwrap())
        .unwrap()
}

fn assert_reply_unchanged(actual: &crate::model::Message, original: &crate::model::Message) {
    assert_eq!(actual.text, original.text);
    assert_eq!(actual.work, original.work);
    assert_eq!(actual.author, original.author);
    assert_eq!(actual.ts, original.ts);
    assert_eq!(actual.edited, original.edited);
}

#[test]
fn completed_stream_is_not_duplicated_and_model_tool_calls_are_inert() {
    let backend = backend("Streamed answer");
    backend.finish.set(true);
    backend.stop.set(StopReason::EndTurn);
    let (mut app, mut doc) = chat(backend.clone());
    let start = app.model_command("c_general", "ask").unwrap();
    pump(&mut app, &mut doc);
    let message = reply(&doc, &start["reply"]);
    assert_eq!(message.text, "Streamed answer");
    assert_eq!(
        message
            .work
            .as_ref()
            .unwrap()
            .stream
            .as_ref()
            .unwrap()
            .reasoning,
        "Provider summary"
    );
    assert_eq!(
        message
            .work
            .as_ref()
            .unwrap()
            .stream
            .as_ref()
            .unwrap()
            .finish,
        Some(AgentFinish::Complete)
    );
    assert!(
        message.work.as_ref().unwrap().trace.is_empty(),
        "model-proposed tools are not actions"
    );
    let before = doc.save();
    assert!(!app.pump_model());
    assert_eq!(doc.save(), before);
    let requests = backend.requests.borrow();
    assert!(requests[0].tools.is_empty());
    assert_eq!(requests[0].tool_choice, ToolChoice::None);
}

#[test]
fn denial_retains_partial_output_and_retry_uses_a_fresh_reply() {
    let backend = backend("Partial");
    backend.finish.set(true);
    backend
        .error
        .set(Some(ui::models::ErrorKind::PermissionDenied));
    let (mut app, mut doc) = chat(backend.clone());
    let first = app.model_command("c_general", "ask").unwrap();
    pump(&mut app, &mut doc);
    let original = reply(&doc, &first["reply"]);
    let stream = original.work.as_ref().unwrap().stream.as_ref().unwrap();
    assert_eq!(stream.finish, Some(AgentFinish::Failed));
    assert!(!stream.detail.is_empty());
    backend.error.set(None);
    backend.stop.set(StopReason::EndTurn);
    let retry = app.model_command("c_general", "retry").unwrap();
    assert_ne!(retry["reply"], first["reply"]);
    pump(&mut app, &mut doc);
    assert_reply_unchanged(&reply(&doc, &first["reply"]), &original);
    assert_eq!(reply(&doc, &retry["reply"]).text, "Partial");
}

#[test]
fn truncation_continue_keeps_the_original_answer_and_sends_explicit_context() {
    let backend = backend("Partial answer");
    backend.finish.set(true);
    let (mut app, mut doc) = chat(backend.clone());
    let first = app.model_command("c_general", "ask").unwrap();
    pump(&mut app, &mut doc);
    let original = reply(&doc, &first["reply"]);
    assert_eq!(
        original
            .work
            .as_ref()
            .unwrap()
            .stream
            .as_ref()
            .unwrap()
            .finish,
        Some(AgentFinish::Truncated)
    );
    assert!(app.model_controls("c_general").contains(&"continue"));
    let continuation = app.model_command("c_general", "continue").unwrap();
    pump(&mut app, &mut doc);
    assert_ne!(first["reply"], continuation["reply"]);
    assert_reply_unchanged(&reply(&doc, &first["reply"]), &original);
    let requests = backend.requests.borrow();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].messages.len(), 3);
    assert_eq!(requests[1].messages[1].role, Role::Assistant);
    assert!(requests[1].tools.is_empty());
}

#[test]
fn stop_preserves_partial_output_and_blocks_replacement_until_terminal_publish() {
    let backend = backend("Keep this");
    let (mut app, mut doc) = chat(backend);
    let first = app.model_command("c_general", "ask").unwrap();
    pump(&mut app, &mut doc);
    assert!(app.host_model.running());
    assert_eq!(app.model_controls("c_general"), vec!["stop"]);
    assert_eq!(
        app.model_command("c_general", "stop").unwrap()["stopped"],
        true
    );
    assert!(app.model_command("c_general", "retry").is_err());
    assert!(app.host_model.controls("other-channel").is_empty());
    pump(&mut app, &mut doc);
    let message = reply(&doc, &first["reply"]);
    assert_eq!(message.text, "Keep this");
    assert_eq!(
        message.work.unwrap().stream.unwrap().finish,
        Some(AgentFinish::Cancelled)
    );
    assert!(app.model_controls("c_general").contains(&"retry"));
}

#[test]
fn stop_before_first_pump_still_records_cancellation() {
    let (mut app, mut doc) = chat(backend("Never polled"));
    let first = app.model_command("c_general", "ask").unwrap();
    app.model_command("c_general", "stop").unwrap();
    pump(&mut app, &mut doc);
    let message = reply(&doc, &first["reply"]);
    assert!(message.text.is_empty());
    assert_eq!(
        message.work.unwrap().stream.unwrap().finish,
        Some(AgentFinish::Cancelled)
    );
}

#[test]
fn empty_chat_hides_ask_and_stale_activation_reports_the_refusal() {
    let backend = backend("Answer");
    let mut doc = automerge::AutoCommit::new();
    crate::testing::starter(&mut doc, crate::model::ME, "You", "Y");
    let mut app = ChatApp::new_ephemeral().with_models(Models::new(backend.clone()));
    app.reload(&doc);
    assert!(app.model_controls("c_general").is_empty());
    assert!(!app.activate_model_control("ask"));
    assert!(
        app.attachment_status
            .as_ref()
            .is_some_and(|message| !message.is_empty())
    );
    assert!(backend.requests.borrow().is_empty());
}

#[test]
fn mounted_host_effect_is_device_local_and_model_capabilities_are_mount_local() {
    use crate::{ChatShell, record::chat, resource::Chat};
    use ui::framework::testing::TestApp;
    use ui::tilo_resource::{Id, Resource as _};
    let first = backend("First mount");
    first.finish.set(true);
    first.stop.set(StopReason::EndTurn);
    let second = backend("Second mount");
    let mut a =
        TestApp::<ChatShell>::for_person_with_lent(crate::model::ME, Models::new(first.clone()))
            .unwrap();
    let b =
        TestApp::<ChatShell>::for_person_with_lent("other", Models::new(second.clone())).unwrap();
    a.invoke(Chat::POST.with(chat::Post {
        channel: Id::from_raw("c_general"),
        text: "Question".into(),
        parent: None,
        body: None,
        attachment: None,
    }))
    .unwrap();
    let channel = crate::testing::item("channels", "c_general");
    let acknowledged = a
        .call_on(
            &channel,
            "model-control",
            serde_json::json!({"command": "ask"}),
        )
        .unwrap();
    let acknowledged = acknowledged.unwrap();
    assert_eq!(acknowledged["started"], true);
    let reply = acknowledged["reply"].as_str().unwrap();
    let message_row = crate::testing::item("messages", reply);
    let surface = a.agent_surface();
    for (resource, action) in [
        (channel.as_str(), "model-control"),
        (message_row.as_str(), "show-agent-reasoning"),
        (Chat::KIND, "stream-output"),
        (Chat::KIND, "record-work"),
    ] {
        assert!(
            surface
                .as_array()
                .unwrap()
                .iter()
                .any(|row| { row["resource"] == resource && row["action"] == action }),
            "missing {resource}#{action}"
        );
    }
    a.with_surface::<ChatApp, _>(|surface| {
        let message = surface
            .search_messages
            .iter()
            .find(|m| m.id == reply)
            .unwrap();
        assert!(
            surface
                .model_message_label(message)
                .contains(&crate::i18n::t("chat-agent-complete"))
        );
        assert!(
            !surface
                .model_message_label(message)
                .contains("Provider summary")
        );
    })
    .unwrap();
    a.call_on(
        &message_row,
        "show-agent-reasoning",
        serde_json::json!({"expanded": true}),
    )
    .unwrap();
    a.with_surface::<ChatApp, _>(|surface| {
        let message = surface
            .search_messages
            .iter()
            .find(|m| m.id == reply)
            .unwrap();
        assert!(
            surface
                .model_message_label(message)
                .contains("Provider summary")
        );
    })
    .unwrap();
    a.with_surface::<ChatApp, _>(|surface| assert!(surface.host_model.models.is_available()))
        .unwrap();
    assert_eq!(first.requests.borrow().len(), 1);
    assert!(second.requests.borrow().is_empty());
    assert!(b.state().message_rows("c_general").is_empty());
    assert!(
        a.call_by(
            ui::tilo_actions::ActionInvoker::actor("remote-agent"),
            &channel,
            "model-control",
            serde_json::json!({"command": "ask"}),
        )
        .is_err(),
        "a collaborator cannot adopt the device's model capability"
    );
}
