//! Native Session, canonical Chat and a real owner application over the typed broker.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "bounded integration assertions"
)]
use automerge::AutoCommit;
use chat::{
    actions::ChatAction,
    doc::ChatDoc,
    model::{AgentRoute, AgentStepOutcome, AgentWorkState},
};
use qos_actions::Action;
use qos_agent::{Config, Session};
use std::time::{Duration, Instant};
use ui::collaboration::{CollabEvent, CollabRequest, DocumentMutation};
mod typed_peer;
use typed_peer::{TypedPeer, send};
const CHAT: &str = "dev.qos.chat";
const WP: &str = "dev.qos.word-processor";

fn config(path: std::path::PathBuf) -> Config {
    use ui::permissions::{Actor, CapOverride, PermissionContext, Scope, Subject, Verb};
    Config {
        endpoint: Some(path),
        endpoint_identity: Some(typed_peer::endpoint_identity("Scribe")),
        app_id: CHAT.into(),
        cluster: typed_peer::room(),
        record: None,
        agent_id: "Scribe".into(),
        name: "Reader".into(),
        pace: Duration::ZERO,
        once: true,
        model: false,
        model_timeout: Duration::from_millis(10),
        await_document: Duration::ZERO,
        settle: Duration::ZERO,
        directory: None,
        permissions: PermissionContext::from_capabilities(
            Actor::agent("Scribe"),
            vec![
                CapOverride::grant(
                    Subject::agent("Scribe"),
                    Scope::App("word-processor".into()),
                    Verb::Write,
                )
                .with_id("reviewed"),
            ],
        ),
    }
}
fn resources(doc: &AutoCommit) -> Vec<qos_actions::Resource> {
    word_processor::actions::provide_resources(
        &word_processor::actions::WpResourceCtx {
            caret_block: qdoc::model::open(doc, &automerge::ROOT)
                .map(|c| {
                    qdoc::model::materialize(doc, &c)
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
            handle: Some("quinten".into()),
        },
    )
    .unwrap();
    p.set_read_timeout(Some(Duration::from_millis(20)));
    p
}
#[allow(
    clippy::too_many_arguments,
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
    disconnect: bool,
    wait_for_owner: bool,
    requests: usize,
) {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("chat.sock");
    let _broker = typed_peer::spawn_with_agent_actions(
        &socket,
        if allowed {
            &[(WP, "insert-table"), (WP, "zoom-in")][..]
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
            handle: Some("quinten".into()),
        },
    )
    .unwrap();
    chat_peer.set_read_timeout(Some(Duration::from_millis(20)));
    let mut owner = peer(&socket, WP);
    let mut owner_online = true;
    let mut chat = AutoCommit::new();
    ChatDoc::seed_starter(&mut chat, "human-author", "Human", "HU");
    let request = ChatAction::SendMessage {
        channel: "c_general".into(),
        id: "request-1".into(),
        author: "human-author".into(),
        ts: 1,
        text: text.into(),
        parent: None,
        attachment: None,
        reference: wait_for_owner.then(|| {
            qos_reference::ResourceRef::whole(
                "word-processor.document",
                "word-processor.document",
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
    };
    request.apply(&mut chat).unwrap();
    for number in 2..=requests {
        let mut additional = request.clone();
        if let ChatAction::SendMessage { id, ts, .. } = &mut additional {
            *id = format!("request-{number}");
            *ts = i64::try_from(number).unwrap();
        }
        additional.apply(&mut chat).unwrap();
    }
    let mut document = AutoCommit::new();
    let schema = qdoc::model::init(&mut document, &automerge::ROOT);
    let mut ids = qdoc::model::seeded_id_gen(&document, &schema);
    {
        let mut edit = qdoc::DocMut {
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
        typed_peer::publish_actions(&mut owner, WP, &resources(&document));
        let mut cfg = config(socket.clone());
        cfg.once = requests == 1;
        let (tx, rx) = std::sync::mpsc::channel();
        let runner = std::thread::spawn(move || {
            let mut s = Session::connect(cfg).unwrap();
            tx.send(()).unwrap();
            s.run_until(Instant::now() + Duration::from_secs(3))
                .unwrap()
        });
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
        if owner_released {
            send(
                &mut owner,
                &CollabRequest::DocumentSnapshot {
                    data: document.save(),
                },
            )
            .unwrap();
        }
        send(
            &mut chat_peer,
            &CollabRequest::DocumentSnapshot { data: chat.save() },
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(4);
        while Instant::now() < deadline && !runner.is_finished() {
            if owner_online && let Some(event) = owner.recv().unwrap() {
                match event {
                    CollabEvent::RemotePresenceUpdate {
                        key: ui::presence::PresenceKey::Workspace { id },
                        payload: ui::collaboration::PresencePayload::Arrived,
                        ..
                    } if id == WP && owner_released => {
                        send(
                            &mut owner,
                            &CollabRequest::DocumentSnapshot {
                                data: document.save(),
                            },
                        )
                        .unwrap();
                    }
                    CollabEvent::Invocation {
                        id,
                        resource,
                        action,
                        argument,
                        actor,
                        ..
                    } => {
                        invocations += 1;
                        assert_eq!(actor, "agent:Scribe");
                        let tree = resources(&document);
                        let before = document.get_heads();
                        let update = typed_peer::service_document_invocation(
                            &mut owner,
                            &mut document,
                            &tree,
                            word_processor::actions::register,
                            id,
                            resource,
                            action,
                            argument.as_deref(),
                            &actor,
                        );
                        assert!(
                            document
                                .get_changes(&before)
                                .iter()
                                .any(|change| { change.actor_id().to_bytes() == actor.as_bytes() })
                        );
                        if disconnect {
                            // The action changed its real owner document, then the
                            // actual endpoint disappears before a terminal receipt.
                            owner = TypedPeer::connect(&socket);
                            owner_online = false;
                        } else {
                            send(&mut owner, &CollabRequest::UpdateInvocation { id, update })
                                .unwrap();
                        }
                    }
                    _ => {}
                }
            }
            if let Some(event) = chat_peer.recv().unwrap() {
                if let Some(DocumentMutation::Incremental(data)) =
                    typed_peer::document_mutation(&event)
                {
                    chat.load_incremental(&data).unwrap();
                    for m in ChatDoc::list_messages(&chat, "c_general") {
                        if let Some(w) = m.work {
                            observed.push(w);
                        }
                    }
                }
            }
            if !owner_released
                && observed
                    .iter()
                    .any(|work| matches!(work.state, AgentWorkState::Waiting { .. }))
            {
                owner_released = true;
                typed_peer::publish_actions(&mut owner, WP, &resources(&document));
                send(
                    &mut owner,
                    &CollabRequest::DocumentSnapshot {
                        data: document.save(),
                    },
                )
                .unwrap();
            }
        }
        let drain = Instant::now() + Duration::from_millis(250);
        while Instant::now() < drain {
            if let Some(event) = chat_peer.recv().unwrap() {
                if let Some(DocumentMutation::Incremental(data)) =
                    typed_peer::document_mutation(&event)
                {
                    chat.load_incremental(&data).unwrap();
                    for message in ChatDoc::list_messages(&chat, "c_general") {
                        if let Some(work) = message.work {
                            observed.push(work);
                        }
                    }
                }
            }
        }
        assert_eq!(
            runner.join().unwrap(),
            !restart && expected_reply,
            "only initial addressed summons acts"
        );
    }
    assert_eq!(
        invocations,
        requests * usize::from(expected_reply && allowed),
        "one actual owner dispatch across restart"
    );
    let schema = qdoc::model::open(&document, &automerge::ROOT).unwrap();
    assert_eq!(
        qdoc::model::materialize(&document, &schema)
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
    let replies: Vec<_> = ChatDoc::list_messages(&chat, "c_general")
        .into_iter()
        .filter(|m| m.parent.as_deref() == Some("request-1"))
        .collect();
    assert_eq!(replies.len(), usize::from(expected_reply));
    if !expected_reply {
        return;
    }
    let reply = &replies[0];
    assert_eq!(reply.author, "Scribe");
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
    if disconnect {
        assert!(
            !work.finished(),
            "lost receipt cannot claim completion or refusal: {work:?}"
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
    scenario("@Scribe insert a table", Some("Scribe"), true, None, true);
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
    scenario("@Scribe insert a table", Some("Scribe"), false, None, true);
}
#[test]
fn record_scoped_chat_cannot_be_conflated_with_package_primary() {
    scenario(
        "@Scribe insert a table",
        Some("Scribe"),
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
        false,
        false,
        1,
    );
}
#[test]
fn owner_disconnect_after_mutation_is_unconfirmed_and_restart_never_repeats() {
    scenario_outcome(
        "@Scribe insert a table",
        Some("Scribe"),
        true,
        None,
        true,
        true,
        false,
        1,
    );
}

#[test]
fn missing_owner_state_waits_then_reuses_same_reply_and_executes() {
    scenario_outcome(
        "@Scribe insert a table",
        Some("Scribe"),
        true,
        None,
        true,
        false,
        true,
        1,
    );
}

#[test]
fn multiple_addressed_messages_are_pumped_oldest_first_without_new_traffic() {
    scenario_outcome(
        "@Scribe insert a table",
        Some("Scribe"),
        true,
        None,
        true,
        false,
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
        Some("Scribe"),
        true,
        None,
        true,
        false,
        false,
        0,
    );
}
