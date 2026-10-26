use super::*;
use std::{cell::Cell, task::Poll};
use ui::models::{ModelBackend, ModelResponse, StopReason, StreamSink, Usage};

struct Backend {
    text: String,
    reasoning: RefCell<String>,
    final_text: RefCell<Option<String>>,
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
                    text: self.text.clone(),
                });
                sink(StreamEvent::ReasoningDelta {
                    text: self.reasoning.borrow().clone(),
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
                message: ui::models::Message::text(
                    Role::Assistant,
                    self.final_text.borrow().as_deref().unwrap_or(&self.text),
                ),
                stop_reason: self.stop.get(),
                usage: Usage::default(),
            }))
        }))
    }
}
fn backend(text: &str) -> Rc<Backend> {
    Rc::new(Backend {
        text: text.into(),
        reasoning: RefCell::new("Provider summary".into()),
        final_text: RefCell::new(None),
        finish: Cell::new(false),
        error: Cell::new(None),
        stop: Cell::new(StopReason::OutputLimit),
        requests: RefCell::new(vec![]),
    })
}

fn chat(backend: Rc<Backend>) -> (ChatApp, crate::testing::Room) {
    let mut doc = crate::testing::Room::default();
    crate::testing::starter(&mut doc, crate::model::ME, "You", "Y");
    crate::testing::say(&mut doc, crate::model::ME, "c_chat", "Question");
    let mut app = ChatApp::new_ephemeral().with_models(Models::new(backend));
    app.attach_room(&doc);
    app.reload(&doc);
    app.ensure_selection();
    app.reload(&doc);
    (app, doc)
}

fn pump(app: &mut ChatApp, doc: &mut crate::testing::Room) {
    app.pump_model();
    app.land(doc);
    app.reload(doc);
}

fn reply(doc: &crate::testing::Room, id: &serde_json::Value) -> crate::model::Message {
    doc.message_row(id.as_str().unwrap()).unwrap()
}

fn mounted_reply(
    backend: Rc<Backend>,
) -> (ui::framework::testing::TestApp<crate::ChatShell>, String) {
    let mut app = ui::framework::testing::TestApp::<crate::ChatShell>::for_person_with_lent(
        crate::model::ME,
        Models::new(backend),
    )
    .unwrap();
    crate::testing::mounted_post(&mut app, "c_chat", "Question");
    let result = app
        .call_on(
            &crate::testing::item("channels", "c_chat"),
            "model-control",
            serde_json::json!({"command": "ask"}),
        )
        .unwrap()
        .unwrap();
    (app, result["reply"].as_str().unwrap().into())
}

#[test]
fn mounted_oversized_stream_and_final_persist_utf8_truncation_and_safe_continuation() {
    for final_only in [false, true] {
        // Two-byte characters after a one-byte lead put the byte limit in the
        // middle of a character, which is what the truncation must respect.
        // Emoji would too, but laying out sixteen thousand of them made this
        // the slowest test in the app without testing anything more.
        let oversized = format!("a{}", "é".repeat(crate::record::MAX_MESSAGE_TEXT_BYTES));
        let backend = backend(if final_only { "" } else { &oversized });
        if final_only {
            *backend.final_text.borrow_mut() = Some(oversized.clone());
            backend.finish.set(true);
            backend.stop.set(StopReason::EndTurn);
        }
        let (mut app, id) = mounted_reply(backend.clone());
        let original = crate::testing::mounted_rows(&mut app, "c_chat")
            .into_iter()
            .find(|row| row.id == id)
            .unwrap();
        assert_eq!(
            original.text,
            utf8_prefix(&oversized, crate::record::MAX_MESSAGE_TEXT_BYTES)
        );
        let work = original.work.as_ref().unwrap();
        let stream = work.stream.as_ref().unwrap();
        assert_eq!(stream.finish, Some(AgentFinish::Truncated));
        assert!(!stream.detail.is_empty());
        assert!(work.trace.is_empty());
        assert!(!original.text.contains("inert"));
        app.with_surface::<ChatApp, _>(|surface| {
            assert!(!surface.host_model.running());
            assert!(!surface.host_model.needs_frame());
            assert_eq!(
                surface.model_controls("c_chat"),
                vec!["ask", "retry", "continue"]
            );
        })
        .unwrap();
        app.call_on(
            &crate::testing::item("channels", "c_chat"),
            "model-control",
            serde_json::json!({"command": "continue"}),
        )
        .unwrap();
        let requests = backend.requests.borrow();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[1].messages[1],
            ui::models::Message::text(Role::Assistant, &original.text)
        );
        assert!(requests[1].tools.is_empty());
        assert_eq!(requests[1].tool_choice, ToolChoice::None);
        let retained = crate::testing::mounted_rows(&mut app, "c_chat")
            .into_iter()
            .find(|row| row.id == id)
            .unwrap();
        assert_reply_unchanged(&retained, &original);
    }
}

#[test]
fn mounted_reasoning_overflow_splits_deltas_and_persists_terminal_without_continue() {
    let backend = backend("");
    let oversized = "🦀".repeat(MAX_REASONING_BYTES);
    *backend.reasoning.borrow_mut() = oversized.clone();
    let (mut app, id) = mounted_reply(backend);
    let message = crate::testing::mounted_rows(&mut app, "c_chat")
        .into_iter()
        .find(|row| row.id == id)
        .unwrap();
    assert!(message.text.is_empty());
    let work = message.work.unwrap();
    let stream = work.stream.unwrap();
    assert_eq!(
        stream.reasoning,
        utf8_prefix(&oversized, MAX_REASONING_BYTES)
    );
    assert_eq!(stream.finish, Some(AgentFinish::Truncated));
    assert_eq!(
        stream.sequence,
        (MAX_REASONING_BYTES / crate::record::MAX_MESSAGE_TEXT_BYTES + 1) as u64
    );
    assert!(work.trace.is_empty());
    app.with_surface::<ChatApp, _>(|surface| {
        assert!(!surface.host_model.running());
        assert_eq!(surface.model_controls("c_chat"), vec!["ask", "retry"]);
    })
    .unwrap();
}

#[test]
fn mounted_contradictory_final_fails_and_keeps_persisted_partial() {
    for final_text in ["", "Partial", "Replacement answer"] {
        let backend = backend("Partial answer");
        let (mut app, id) = mounted_reply(backend.clone());
        *backend.final_text.borrow_mut() = Some(final_text.into());
        backend.finish.set(true);
        backend.stop.set(StopReason::EndTurn);
        app.settle();
        let message = crate::testing::mounted_rows(&mut app, "c_chat")
            .into_iter()
            .find(|row| row.id == id)
            .unwrap();
        assert_eq!(message.text, "Partial answer");
        let work = message.work.unwrap();
        let stream = work.stream.unwrap();
        assert_eq!(stream.finish, Some(AgentFinish::Failed));
        assert!(stream.detail.contains("previously published"));
        assert!(work.trace.is_empty());
        app.with_surface::<ChatApp, _>(|surface| {
            let attempt = surface.host_model.attempt.as_ref().unwrap();
            assert_eq!(
                attempt.state.error.as_ref().unwrap().kind,
                ui::models::ErrorKind::InvalidResponse
            );
            assert!(!surface.host_model.needs_frame());
            assert_eq!(surface.model_controls("c_chat"), vec!["ask", "retry"]);
        })
        .unwrap();
    }
}

fn assert_reply_unchanged(actual: &crate::model::Message, original: &crate::model::Message) {
    assert_eq!(actual.text, original.text);
    assert_eq!(actual.work, original.work);
    assert_eq!(actual.author, original.author);
    assert_eq!(actual.ts, original.ts);
    assert_eq!(actual.edited, original.edited);
}

#[test]
fn stream_display_names_do_not_change_person_bot_or_agent_identity() {
    let (mut app, mut doc) = chat(backend("Answer"));
    let start = app.model_command("c_chat", "ask").unwrap();
    pump(&mut app, &mut doc);
    let mut message = reply(&doc, &start["reply"]);
    message.appearance = Some(tilo_actions::Appearance {
        name: "Another person".into(),
        avatar: None,
        persona: None,
    });
    let initiator = app.user_name(&message.author);
    let name = app.message_author_name(&message);
    assert_eq!(name, "Another person");
    assert!(
        matches!(app.message_author(&message, &name, &initiator, None),
        ui::author::Author::Agent { for_person: Some(person), .. } if person == initiator)
    );
    assert!(
        app.message_author_label(&message)
            .contains("model output, requested by")
    );
    message.appearance = None;
    assert_eq!(app.message_author_name(&message), "Assistant");
    message.appearance = Some(tilo_actions::Appearance {
        name: "Another person".into(),
        avatar: None,
        persona: None,
    });
    let stream_work = message.work.take();
    let name = app.message_author_name(&message);
    assert_eq!(
        name, initiator,
        "ordinary people cannot adopt appearance names"
    );
    assert!(
        matches!(app.message_author(&message, &name, &initiator, None),
        ui::author::Author::Person { identity, .. } if identity == crate::model::ME)
    );
    assert!(!app.model_message_label(&message).contains("Another person"));
    message.work = stream_work;
    message.author = "bot:installed".into();
    let name = app.message_author_name(&message);
    assert!(matches!(app.message_author(&message, &name, "", None),
        ui::author::Author::Bot { appearance: Some(shown), .. } if shown.name == "Another person"));
    assert!(!app.is_provider_output(&message));
    let label = app.model_message_label(&message);
    assert!(label.starts_with("Another person (bot:"));
    assert!(!label.contains("model output"));
    message.author = "agent-task:actual".into();
    app.agent_authors
        .insert(message.author.clone(), "Actual agent".into());
    let name = app.message_author_name(&message);
    assert_eq!(name, app.user_name(&message.author));
    assert!(name.starts_with("Actual agent"));
    assert!(matches!(
        app.message_author(&message, &name, "", None),
        ui::author::Author::Agent {
            for_person: None,
            ..
        }
    ));
    assert!(!app.is_provider_output(&message));
}

#[test]
fn a_bridged_author_reads_via_their_service_and_the_bot_after_it() {
    let (mut app, mut doc) = chat(backend("Answer"));
    let start = app.model_command("c_chat", "ask").unwrap();
    pump(&mut app, &mut doc);
    let mut message = reply(&doc, &start["reply"]);
    message.work = None;
    message.author = "bot:bridge-installation".into();
    message.appearance = Some(tilo_actions::Appearance {
        name: "Dana".into(),
        avatar: None,
        persona: Some(tilo_actions::Persona {
            service: "discord".into(),
            account: "3001".into(),
            linked: None,
        }),
    });
    assert_eq!(app.message_author_name(&message), "Discord bridge");
    let label = app.model_message_label(&message);
    assert!(label.starts_with("Dana (via Discord): "), "{label}");
    assert!(!label.contains("Someone"), "{label}");

    // A member named alike: an unlinked persona says where it is from, one
    // linked to that member is them.
    app.users.push(crate::model::User {
        id: "u_dana".into(),
        name: "Dana".into(),
        initials: "D".into(),
        color_idx: 0,
        status: crate::model::Presence::Online,
        is_agent: false,
    });
    let label = app.model_message_label(&message);
    assert!(
        label.starts_with("Dana · Discord (via Discord): "),
        "{label}"
    );
    if let Some(persona) = message
        .appearance
        .as_mut()
        .and_then(|appearance| appearance.persona.as_mut())
    {
        persona.linked = Some("u_dana".into());
    }
    let label = app.model_message_label(&message);
    assert!(label.starts_with("Dana (via Discord): "), "{label}");
    // One person, one colour: the bridged tile wears the linked member's.
    let name = app.message_author_name(&message);
    let ui::author::Author::Bot { linked_color, .. } =
        app.message_author(&message, &name, "", None)
    else {
        panic!("a bridged message is the bot's");
    };
    assert_eq!(linked_color, Some(app.identity_colour("u_dana")));
    assert_ne!(
        app.identity_colour("u_dana"),
        app.identity_colour(&message.author),
        "not the bridge's own colour"
    );
}

/// A member who has only ever written from their linked Discord account is
/// still one of the room's people: their bridged message is theirs, by
/// their name with Discord's mark, never "Dana · Discord" as if a namesake
/// wrote it.
#[test]
fn a_member_writing_only_from_a_linked_account_reads_as_themselves() {
    let (mut app, mut doc) = chat(backend("Answer"));
    let start = app.model_command("c_chat", "ask").unwrap();
    pump(&mut app, &mut doc);
    let mut base = reply(&doc, &start["reply"]);
    base.work = None;
    let channel = base.channel.clone();
    let mut record = crate::record::Chat::default();
    for (id, name) in [("u_dana", "Dana"), ("u_other_dana", "Dana")] {
        record.people.push(crate::record::Person {
            id: ui::tilo_resource::Atom(id.to_owned()),
            name: name.into(),
            ..crate::record::Person::default()
        });
    }
    let mut other = base.clone();
    other.author = "u_other_dana".into();
    let mut bridged = base;
    bridged.author = "bot:bridge-installation".into();
    bridged.appearance = Some(tilo_actions::Appearance {
        name: "Dana".into(),
        avatar: None,
        persona: Some(tilo_actions::Persona {
            service: "discord".into(),
            account: "3001".into(),
            linked: Some("u_dana".into()),
        }),
    });
    app.users = app.room_people_from(&record, &[(channel, vec![other, bridged.clone()])]);
    assert!(
        app.users.iter().any(|user| user.id == "u_dana"),
        "the linked member is one of the room's people"
    );
    let members: Vec<&str> = app.users.iter().map(|user| user.name.as_str()).collect();
    let name = app.message_author_name(&bridged);
    let author = app.message_author(&bridged, &name, "", None);
    assert_eq!(author.label(&members), "Dana");
    assert!(author.chip_label().is_none(), "no text tag");
    assert!(author.service_mark().is_some(), "Discord's mark instead");
    assert_eq!(
        author.linked_line().as_deref(),
        Some("Linked to Dana on Tilo")
    );
}

#[test]
fn a_messages_name_says_which_file_it_shares() {
    let (mut app, mut doc) = chat(backend("Answer"));
    let start = app.model_command("c_chat", "ask").unwrap();
    pump(&mut app, &mut doc);
    let mut message = reply(&doc, &start["reply"]);
    message.work = None;
    message.text = String::new();
    message.attachment = Some(crate::model::Attachment::File {
        name: "plan.txt".into(),
        blob: "ff".into(),
        size: 5,
    });
    assert!(app.model_message_label(&message).ends_with(": plan.txt"));
    message.text = "see this".into();
    assert!(
        app.model_message_label(&message)
            .ends_with(": see this\nplan.txt")
    );
}

#[test]
fn completed_stream_is_not_duplicated_and_model_tool_calls_are_inert() {
    let backend = backend("Streamed answer");
    backend.finish.set(true);
    backend.stop.set(StopReason::EndTurn);
    let (mut app, mut doc) = chat(backend.clone());
    let start = app.model_command("c_chat", "ask").unwrap();
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
    let first = app.model_command("c_chat", "ask").unwrap();
    pump(&mut app, &mut doc);
    let original = reply(&doc, &first["reply"]);
    let stream = original.work.as_ref().unwrap().stream.as_ref().unwrap();
    assert_eq!(stream.finish, Some(AgentFinish::Failed));
    assert!(!stream.detail.is_empty());
    backend.error.set(None);
    backend.stop.set(StopReason::EndTurn);
    let retry = app.model_command("c_chat", "retry").unwrap();
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
    let first = app.model_command("c_chat", "ask").unwrap();
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
    assert!(app.model_controls("c_chat").contains(&"continue"));
    let continuation = app.model_command("c_chat", "continue").unwrap();
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
    let first = app.model_command("c_chat", "ask").unwrap();
    pump(&mut app, &mut doc);
    assert!(app.host_model.running());
    assert_eq!(app.model_controls("c_chat"), vec!["stop"]);
    assert_eq!(
        app.model_command("c_chat", "stop").unwrap()["stopped"],
        true
    );
    assert!(app.model_command("c_chat", "retry").is_err());
    assert!(app.host_model.controls("other-channel").is_empty());
    pump(&mut app, &mut doc);
    let message = reply(&doc, &first["reply"]);
    assert_eq!(message.text, "Keep this");
    assert_eq!(
        message.work.unwrap().stream.unwrap().finish,
        Some(AgentFinish::Cancelled)
    );
    assert!(app.model_controls("c_chat").contains(&"retry"));
}

#[test]
fn stop_before_first_pump_still_records_cancellation() {
    let (mut app, mut doc) = chat(backend("Never polled"));
    let first = app.model_command("c_chat", "ask").unwrap();
    app.model_command("c_chat", "stop").unwrap();
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
    assert!(app.model_controls("c_chat").is_empty());
    assert!(!app.activate_model_control("ask"));
    assert!(
        app.attachment_status
            .as_ref()
            .is_some_and(|message| !message.is_empty())
    );
    assert!(backend.requests.borrow().is_empty());
}

fn message_resource<'a>(
    rows: &'a [tilo_actions::Resource],
    id: &str,
) -> Option<&'a tilo_actions::Resource> {
    rows.iter().find_map(|row| {
        if row.id.as_str() == id {
            Some(row)
        } else {
            message_resource(&row.children, id)
        }
    })
}

fn assert_mounted_model_attribution(
    a: &mut ui::framework::testing::TestApp<crate::ChatShell>,
    reply: &str,
) {
    a.with_surface::<ChatApp, _>(|surface| {
        let message = surface
            .search_messages
            .iter()
            .find(|m| m.id == reply)
            .unwrap();
        assert_eq!(
            message.author,
            crate::model::ME,
            "custody stays with the invoker"
        );
        assert_eq!(surface.message_author_name(message), "Assistant");
        let initiator = surface.user_name(&message.author);
        assert!(matches!(
            surface.message_author(message, "Assistant", &initiator, None),
            ui::author::Author::Agent { name: "Assistant", for_person: Some(person), .. }
                if person == initiator
        ));
        let label = surface.model_message_label(message);
        assert!(label.starts_with("Assistant (model output, requested by "));
        assert!(label.contains(&initiator));
        assert!(label.contains(&crate::i18n::t("chat-agent-complete")));
        assert!(
            !surface
                .model_message_label(message)
                .contains("Provider summary")
        );
    })
    .unwrap();
    let assistive_label = a
        .with_surface::<ChatApp, _>(|surface| {
            let message = surface
                .search_messages
                .iter()
                .find(|m| m.id == reply)
                .unwrap();
            surface.model_message_label(message)
        })
        .unwrap();
    assert!(
        a.harness().a11y_node_named(&assistive_label).is_some(),
        "the mounted message exports its model attribution to screen readers"
    );
    let resources = a.harness().bound_resources(None).unwrap();
    let row = message_resource(&resources, &crate::testing::item("messages", reply)).unwrap();
    // The document owns the primary label (the message text); the live
    // surface contributes attribution in the resource's presentation.
    a.with_surface::<ChatApp, _>(|surface| {
        let live = surface
            .published_rows()
            .into_iter()
            .find(|row| row.id() == crate::testing::item("messages", reply))
            .unwrap()
            .into_resource();
        assert!(
            live.label
                .starts_with("Assistant (model output, requested by ")
        );
    })
    .unwrap();
    let presentation = row.presentation.as_ref().unwrap();
    assert_eq!(presentation.badge.as_deref(), Some("agent"));
    assert!(
        presentation
            .detail
            .as_ref()
            .unwrap()
            .contains("requested by")
    );
}

#[test]
fn captured_provider_stream_keeps_the_real_invoker_and_model_attribution() {
    let mut capture = ui::framework::testing::TestApp::<crate::ChatShell>::capture_at(
        Some("agent-reasoning"),
        (420, 800),
    )
    .unwrap();
    let label = capture
        .with_surface::<ChatApp, _>(|surface| {
            let message = surface
                .search_messages
                .iter()
                .find(|m| m.id == "shot_agent_stream")
                .unwrap();
            assert_eq!(message.author, crate::model::ME);
            assert_eq!(surface.message_author_name(message), "Assistant");
            let initiator = surface.user_name(&message.author);
            assert!(
                matches!(surface.message_author(message, "Assistant", &initiator, None),
            ui::author::Author::Agent { for_person: Some(person), .. } if person == initiator)
            );
            surface.model_message_label(message)
        })
        .unwrap();
    assert!(label.starts_with("Assistant (model output, requested by "));
    assert!(capture.harness().a11y_node_named(&label).is_some());
}

#[test]
fn model_capabilities_are_mount_local() {
    use crate::ChatShell;
    use ui::framework::testing::TestApp;
    let first = backend("First mount");
    first.finish.set(true);
    first.stop.set(StopReason::EndTurn);
    let second = backend("Second mount");
    let mut a =
        TestApp::<ChatShell>::for_person_with_lent(crate::model::ME, Models::new(first.clone()))
            .unwrap();
    let b =
        TestApp::<ChatShell>::for_person_with_lent("other", Models::new(second.clone())).unwrap();
    crate::testing::mounted_post(&mut a, "c_chat", "Question");
    let channel = crate::testing::item("channels", "c_chat");
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
    let chain = crate::testing::chain();
    let surface = a.agent_surface();
    for (resource, action) in [
        (channel.as_str(), "model-control"),
        (message_row.as_str(), "show-agent-reasoning"),
        (chain.as_str(), "stream-output"),
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
    assert_mounted_model_attribution(&mut a, reply);
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
    let mut b = b;
    assert!(crate::testing::mounted_rows(&mut b, "c_chat").is_empty());
}
