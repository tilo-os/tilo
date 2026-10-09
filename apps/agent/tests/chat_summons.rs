//! Native Session, canonical Chat and a real owner application over the typed broker.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "bounded integration assertions"
)]
use crate::typed_peer;
use automerge::AutoCommit;
use chat::{
    doc::{ChatDoc, NewMessage},
    model::{AgentRoute, AgentStepOutcome, AgentWorkState},
};
use tilo_actions::ActionAddress;
use tilo_agent::{Config, Session};
use tilo_automerge::ApplyIncoming as _;
use typed_peer::{TypedPeer, send};
use ui::collaboration::{CollabEvent, CollabRequest, DocumentMutation};
use ui::time::Duration;
const CHAT: &str = "so.tilo.chat";
const WP: &str = "so.tilo.word-processor";

fn config(path: std::path::PathBuf) -> Config {
    use ui::permissions::{Actor, CapOverride, PermissionContext, Scope, Subject, Verb};
    Config {
        endpoint: Some(path),
        endpoint_identity: Some(typed_peer::endpoint_identity("Scribe")),
        app_id: CHAT.into(),
        cluster: typed_peer::room(),
        record: None,
        agent_id: "scribe".into(),
        name: "Reader".into(),
        pace: Duration::ZERO,
        once: true,
        model: false,
        model_timeout: Duration::from_millis(10),
        await_document: Duration::ZERO,
        settle: Duration::ZERO,
        permissions: PermissionContext::from_capabilities(
            Actor::agent("scribe"),
            vec![
                CapOverride::grant(
                    Subject::agent("scribe"),
                    Scope::App("so.tilo.word-processor".into()),
                    Verb::Write,
                )
                .with_id("reviewed"),
                // Invited to the conversation, as the launcher grants it: the
                // agent's replies and work state are its own Chat edits.
                CapOverride::grant(
                    Subject::agent("scribe"),
                    Scope::App(CHAT.into()),
                    Verb::Write,
                )
                .with_id("invited"),
            ],
        ),
        content: tilo_content::ContentStore::memory(),
    }
}
fn resources(doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
    word_processor::actions::published_for(
        &word_processor::actions::WpResourceCtx {
            caret_block: tilo_doc::model::open(doc, &automerge::ROOT)
                .map(|c| {
                    tilo_doc::model::materialize(doc, &c)
                        .blocks
                        .first()
                        .map(|b| b.id.clone())
                        .unwrap_or_default()
                })
                .unwrap_or_default(),
            caret_offset: 0,
            target_blocks: vec![],
            format_spans: vec![],
            selection: None,
            region: word_processor::actions::RegionKind::Body,
            search_hits: vec![],
            content: tilo_content::ContentStore::memory(),
        },
        doc,
    )
}
fn peer(path: &std::path::Path, app: &str) -> TypedPeer {
    let mut p = TypedPeer::connect(path);
    send(
        &mut p,
        &CollabRequest::Hello {
            app_id: app.into(),
            cluster_id: typed_peer::room(),
            record: None,
            record_kind: None,
            handle: Some("quinten".into()),
        },
    )
    .unwrap();
    p.set_read_timeout(Some(Duration::from_millis(20)));
    p
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum OwnerOutcome {
    Complete,
    Disconnect,
    CancelAfterMutation,
}

fn scenario_resources(doc: &AutoCommit, outcome: OwnerOutcome) -> Vec<tilo_actions::Resource> {
    fn allow_cancellation(resources: &mut [tilo_actions::Resource]) {
        for resource in resources {
            for action in &mut resource.actions {
                if action.id == "insert-table" {
                    action.contract.execution =
                        tilo_actions::ExecutionMode::Background { cancellable: true };
                    action.contract.result = tilo_actions::ResultSpec::Job;
                    assert!(action.contract.validate_declaration().is_empty());
                }
            }
            allow_cancellation(&mut resource.children);
        }
    }
    let mut resources = resources(doc);
    if outcome == OwnerOutcome::CancelAfterMutation {
        allow_cancellation(&mut resources);
    }
    resources
}

#[allow(
    clippy::fn_params_excessive_bools,
    clippy::too_many_lines,
    reason = "one real typed endpoint scenario owns seed, action, disconnect, and restart assertions"
)]
fn scenario_outcome(
    text: &str,
    mention: Option<&str>,
    allowed: bool,
    record: Option<&str>,
    expected_reply: bool,
    outcome: OwnerOutcome,
    wait_for_owner: bool,
    requests: usize,
) {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("chat.sock");
    let _broker = typed_peer::spawn_with_agent_actions(
        &socket,
        if allowed {
            &[
                (WP, typed_peer::WP_DOCUMENT, "insert-table"),
                (WP, typed_peer::WP_DOCUMENT, "zoom-in"),
            ][..]
        } else {
            &[]
        },
    );
    let mut chat_peer = TypedPeer::connect(&socket);
    send(
        &mut chat_peer,
        &CollabRequest::Hello {
            app_id: CHAT.into(),
            cluster_id: typed_peer::room(),
            record: record.map(str::to_string),
            record_kind: None,
            handle: Some("quinten".into()),
        },
    )
    .unwrap();
    chat_peer.set_read_timeout(Some(Duration::from_millis(20)));
    let mut owner = peer(&socket, WP);
    let mut owner_online = true;
    let mut chat = AutoCommit::new();
    ChatDoc::seed_starter(&mut chat, "human-author", "Human", "HU");
    let mut request = |number: usize| {
        ChatDoc::push_message_with_id(
            &mut chat,
            "c_general",
            &format!("request-{number}"),
            NewMessage {
                author: "human-author".into(),
                ts: i64::try_from(number).unwrap(),
                text: text.into(),
                reference: wait_for_owner.then(|| {
                    tilo_reference::ResourceRef::whole(
                        "tilo.rich-text",
                        "tilo.rich-text",
                        "Owner document",
                    )
                }),
                mentions: mention
                    .map(|user| chat::mention::Mention {
                        user: user.into(),
                        start: 0,
                        len: 7,
                        is_agent: true,
                    })
                    .into_iter()
                    .collect(),
                ..Default::default()
            },
        )
    };
    for number in 1..=requests.max(1) {
        request(number);
    }
    let mut document = AutoCommit::new();
    let schema = tilo_doc::model::init(&mut document, &automerge::ROOT);
    let mut ids = tilo_doc::model::seeded_id_gen(&document, &schema);
    {
        let mut edit = tilo_doc::DocMut {
            doc: &mut document,
            c: &schema,
            ids: &mut ids,
        };
        let p = edit.insert_block(None, "paragraph");
        edit.splice_text(&p, 0, 0, "Owner document");
    }
    document.commit();
    let mut invocations = 0;
    let mut observed = Vec::new();
    for restart in [false, true] {
        if !owner_online {
            owner = peer(&socket, WP);
            owner_online = true;
        }
        let mut owner_released = !wait_for_owner || restart;
        // A live action catalogue is not the owner's document snapshot.
        // The waiting case advertises the real action before releasing state.
        typed_peer::publish_actions(&mut owner, WP, &scenario_resources(&document, outcome));
        let mut cfg = config(socket.clone());
        cfg.once = requests == 1;
        let (tx, rx) = std::sync::mpsc::channel();
        let runner = std::thread::spawn(move || {
            let mut s = Session::connect(cfg).unwrap();
            tx.send(()).unwrap();
            s.run_until(ui::time::now() + Duration::from_secs(3))
                .unwrap()
        });
        rx.recv_timeout(tilo_test_env::hang_bound()).unwrap();
        if owner_released {
            send(
                &mut owner,
                &CollabRequest::document_snapshot(document.save()),
            )
            .unwrap();
        }
        send(
            &mut chat_peer,
            &CollabRequest::document_snapshot(chat.save()),
        )
        .unwrap();
        let deadline = ui::time::now() + Duration::from_secs(4);
        while ui::time::now() < deadline && !runner.is_finished() {
            if owner_online && let Some(event) = owner.recv().unwrap() {
                match event {
                    CollabEvent::RemotePresenceUpdate {
                        key: ui::presence::PresenceKey::Workspace { id },
                        payload: ui::collaboration::PresencePayload::Arrived,
                        ..
                    } if id == WP && owner_released => {
                        send(
                            &mut owner,
                            &CollabRequest::document_snapshot(document.save()),
                        )
                        .unwrap();
                    }
                    CollabEvent::Invocation {
                        id,
                        invocation,
                        resource,
                        action,
                        argument,
                        actor,
                        context,
                        ..
                    } => {
                        invocations += 1;
                        assert_eq!(actor, "agent:scribe");
                        let tree = scenario_resources(&document, outcome);
                        let before = document.get_heads();
                        let update = typed_peer::service_document_invocation(
                            &mut owner,
                            &mut document,
                            &tree,
                            word_processor::actions::register,
                            typed_peer::DocumentInvocation {
                                invocation,
                                address: ActionAddress::new(resource, action),
                                argument: argument.as_deref(),
                                actor: &actor,
                                context: context.as_deref(),
                            },
                        );
                        assert!(
                            document
                                .get_changes(&before)
                                .iter()
                                .any(|change| { change.actor_id().to_bytes() == actor.as_bytes() })
                        );
                        if outcome == OwnerOutcome::Disconnect {
                            // The action changed its real owner document, then the
                            // actual endpoint disappears before a terminal receipt.
                            owner = TypedPeer::connect(&socket);
                            owner_online = false;
                        } else {
                            let update = if outcome == OwnerOutcome::CancelAfterMutation {
                                tilo_actions::InvocationUpdate::Cancelled {
                                    invocation: update.invocation(),
                                    message: Some(
                                        "Owner stopped after changing the document".into(),
                                    ),
                                }
                            } else {
                                update
                            };
                            send(&mut owner, &CollabRequest::UpdateInvocation { id, update })
                                .unwrap();
                        }
                    }
                    _ => {}
                }
            }
            if let Some(event) = chat_peer.recv().unwrap()
                && let Some(DocumentMutation::Incremental(data)) =
                    typed_peer::document_mutation(&event)
            {
                chat.load_incoming(&data).unwrap();
                for m in ChatDoc::list_messages(&chat, "c_general") {
                    if let Some(w) = m.work {
                        observed.push(w);
                    }
                }
            }
            if !owner_released
                && observed
                    .iter()
                    .any(|work| matches!(work.state, AgentWorkState::Waiting { .. }))
            {
                owner_released = true;
                typed_peer::publish_actions(
                    &mut owner,
                    WP,
                    &scenario_resources(&document, outcome),
                );
                send(
                    &mut owner,
                    &CollabRequest::document_snapshot(document.save()),
                )
                .unwrap();
            }
        }
        let acted = runner.join().unwrap();
        let drain = ui::time::now()
            + if !restart && expected_reply {
                Duration::from_secs(4)
            } else {
                Duration::from_millis(250)
            };
        let expected_requests = requests.max(1);
        let mut durable_reply = false;
        while ui::time::now() < drain {
            if let Some(event) = chat_peer.recv().unwrap()
                && let Some(DocumentMutation::Incremental(data)) =
                    typed_peer::document_mutation(&event)
            {
                chat.load_incoming(&data).unwrap();
                for message in ChatDoc::list_messages(&chat, "c_general") {
                    if let Some(work) = message.work {
                        observed.push(work);
                    }
                }
            }
            let messages = ChatDoc::list_messages(&chat, "c_general");
            durable_reply = (1..=expected_requests).all(|number| {
                let parent = format!("request-{number}");
                let mut replies = messages.iter().filter(|message| {
                    message.author == "scribe" && message.parent.as_deref() == Some(&parent)
                });
                replies.next().is_some_and(|message| {
                    message.work.as_ref().is_some_and(|work| match outcome {
                        OwnerOutcome::Complete => work.finished(),
                        OwnerOutcome::Disconnect | OwnerOutcome::CancelAfterMutation => {
                            !matches!(&work.state, AgentWorkState::Waiting { .. })
                        }
                    })
                }) && replies.next().is_none()
            });
            if !restart && expected_reply && durable_reply {
                break;
            }
        }
        if !restart && expected_reply {
            assert!(
                durable_reply,
                "the canonical answered state must replicate before restart"
            );
        }
        assert_eq!(
            acted,
            !restart && expected_reply,
            "only initial addressed summons acts"
        );
    }
    assert_eq!(
        invocations,
        requests * usize::from(expected_reply && allowed),
        "one actual owner dispatch across restart"
    );
    let schema = tilo_doc::model::open(&document, &automerge::ROOT).unwrap();
    assert_eq!(
        tilo_doc::model::materialize(&document, &schema)
            .blocks
            .iter()
            .filter(|b| b.kind == "cell")
            .count(),
        if expected_reply && allowed {
            4 * requests
        } else {
            0
        }
    );
    let messages = ChatDoc::list_messages(&chat, "c_general");
    for number in 1..=requests.max(1) {
        let parent = format!("request-{number}");
        assert_eq!(
            messages
                .iter()
                .filter(|message| message.parent.as_deref() == Some(&parent))
                .count(),
            usize::from(expected_reply),
            "each summons has exactly one canonical durable reply"
        );
    }
    let replies: Vec<_> = messages
        .into_iter()
        .filter(|message| message.parent.as_deref() == Some("request-1"))
        .collect();
    if !expected_reply {
        return;
    }
    let reply = &replies[0];
    assert_eq!(reply.author, "scribe");
    assert_eq!(reply.channel, "c_general");
    let work = reply.work.as_ref().unwrap();
    if requests == 0 {
        assert_eq!(work.state, AgentWorkState::Refused);
        assert!(work.trace.is_empty());
        assert!(
            reply.text.contains("view operations are unavailable"),
            "{}",
            reply.text
        );
        return;
    }
    if !allowed {
        assert!(
            !observed
                .iter()
                .any(|work| matches!(work.state, AgentWorkState::Doing { .. })),
            "broker-denied operations must be absent from the Chat decision catalogue"
        );
        assert_eq!(work.state, AgentWorkState::Refused);
        assert!(work.trace.is_empty());
        assert!(!reply.text.is_empty());
        return;
    }
    if outcome != OwnerOutcome::Complete {
        assert!(
            !work.finished(),
            "mutation with an inconclusive terminal outcome cannot claim completion or refusal: {work:?}"
        );
        assert_eq!(work.trace.len(), 1);
        assert_eq!(work.trace[0].outcome, AgentStepOutcome::Asked);
        assert!(reply.text.contains("unconfirmed"));
        return;
    }
    assert!(
        matches!(work.state, AgentWorkState::Done { .. }),
        "{work:?}; reply={}",
        reply.text
    );
    assert_eq!(work.reasoning.as_ref().unwrap().route, AgentRoute::Floor);
    assert_eq!(work.trace.len(), 1);
    assert_eq!(work.trace[0].outcome, AgentStepOutcome::Applied);
    assert!(
        observed
            .iter()
            .any(|w| matches!(w.state, AgentWorkState::Taken))
    );
    assert!(
        observed
            .iter()
            .any(|w| matches!(w.state, AgentWorkState::Doing { .. })
                && w.trace.iter().any(|s| s.outcome == AgentStepOutcome::Asked))
    );
}

#[test]
fn native_chat_summons_executes_owner_action_and_restart_does_not_repeat() {
    scenario("@Scribe insert a table", Some("scribe"), true, None, true);
}
#[test]
fn unrelated_message_does_not_dispatch_or_reply() {
    scenario("insert a table", None, true, None, false);
}
#[test]
fn another_agents_mention_does_not_dispatch_or_reply() {
    scenario(
        "@Elsewhere insert a table",
        Some("another-agent"),
        true,
        None,
        false,
    );
}
#[test]
fn display_name_addressing_uses_same_native_loop() {
    scenario("Reader: insert a table", None, true, None, true);
}
#[test]
fn denied_action_refuses_without_dispatch_and_survives_restart() {
    scenario("@Scribe insert a table", Some("scribe"), false, None, true);
}
#[test]
fn record_scoped_chat_cannot_be_conflated_with_package_primary() {
    scenario(
        "@Scribe insert a table",
        Some("scribe"),
        true,
        Some("other-chat"),
        false,
    );
}

fn scenario(
    text: &str,
    mention: Option<&str>,
    allowed: bool,
    record: Option<&str>,
    expected_reply: bool,
) {
    scenario_outcome(
        text,
        mention,
        allowed,
        record,
        expected_reply,
        OwnerOutcome::Complete,
        false,
        1,
    );
}
#[test]
fn owner_disconnect_after_mutation_is_unconfirmed_and_restart_never_repeats() {
    scenario_outcome(
        "@Scribe insert a table",
        Some("scribe"),
        true,
        None,
        true,
        OwnerOutcome::Disconnect,
        false,
        1,
    );
}

#[test]
fn missing_owner_state_waits_then_reuses_same_reply_and_executes() {
    scenario_outcome(
        "@Scribe insert a table",
        Some("scribe"),
        true,
        None,
        true,
        OwnerOutcome::Complete,
        true,
        1,
    );
}

#[test]
fn multiple_addressed_messages_are_pumped_oldest_first_without_new_traffic() {
    scenario_outcome(
        "@Scribe insert a table",
        Some("scribe"),
        true,
        None,
        true,
        OwnerOutcome::Complete,
        false,
        2,
    );
}
#[test]
fn record_scoped_chat_configuration_is_rejected_before_connecting() {
    let mut cfg = config(std::path::PathBuf::from("/not-a-test-endpoint"));
    cfg.record = Some("other-chat".into());
    let error = Session::connect(cfg)
        .err()
        .expect("record-scoped Chat must be rejected");
    assert!(error.to_string().contains("package-primary"), "{error}");
}

#[test]
fn permitted_view_operation_is_explicitly_refused_before_dispatch() {
    scenario_outcome(
        "@Scribe zoom in",
        Some("scribe"),
        true,
        None,
        true,
        OwnerOutcome::Complete,
        false,
        0,
    );
}

#[test]
fn owner_mutation_then_cancelled_is_unconfirmed_and_restart_never_repeats() {
    scenario_outcome(
        "@Scribe insert a table",
        Some("scribe"),
        true,
        None,
        true,
        OwnerOutcome::CancelAfterMutation,
        false,
        1,
    );
}
