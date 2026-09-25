use super::*;

/// The bus and registry are process globals; tests that touch them serialize.
pub(crate) static SERIAL: Mutex<()> = Mutex::new(());

fn message(json: &str) -> String {
    format!("{LIVE_PREFIX}{json}")
}

#[test]
fn live_updates_parse_strictly_and_bounded() {
    let full = message(
        r#"{"v":1,"seq":7,"state":"writing","label":"Writing in Writer","tool":"append-paragraph",
            "reply":{"text":"Sure — writing it now","done":false},
            "preview":{"app":"so.tilo.word-processor","record":"","text":"The ocean covers"},
            "future":{"ignored":true}}"#,
    );
    let update = parse_update(&full).expect("valid update");
    assert_eq!(update.seq, 7);
    assert_eq!(update.state, "writing");
    assert_eq!(update.tool.as_deref(), Some("append-paragraph"));
    assert_eq!(update.reply.as_ref().unwrap().text, "Sure — writing it now");
    assert_eq!(update.preview.as_ref().unwrap().text, "The ocean covers");
    // Minimal.
    assert!(parse_update(&message(r#"{"v":1,"seq":1,"state":"thinking"}"#)).is_some());
    // Ordinary progress text and malformed or out-of-bounds updates are refused.
    let long = |n: usize| "x".repeat(n);
    let refused = [
        "checking the document".to_owned(),
        message("not json"),
        message(r#"{"v":2,"seq":1,"state":"thinking"}"#),
        message(r#"{"v":1,"seq":1,"state":"dreaming"}"#),
        message(r#"{"v":1,"state":"thinking"}"#),
        message(r#"{"v":1,"seq":-1,"state":"thinking"}"#),
        message(r#"{"v":1,"seq":1,"state":"thinking","label":7}"#),
        message(&format!(
            r#"{{"v":1,"seq":1,"state":"thinking","label":"{}"}}"#,
            long(121)
        )),
        message(r#"{"v":1,"seq":1,"state":"thinking","label":"a\nb"}"#),
        message(r#"{"v":1,"seq":1,"state":"tool","tool":""}"#),
        message(&format!(
            r#"{{"v":1,"seq":1,"state":"tool","tool":"{}"}}"#,
            long(65)
        )),
        message(&format!(
            r#"{{"v":1,"seq":1,"state":"replying","reply":{{"text":"{}"}}}}"#,
            long(4097)
        )),
        message(&format!(
            r#"{{"v":1,"seq":1,"state":"writing","preview":{{"app":"so.tilo.word-processor","text":"{}"}}}}"#,
            long(1025)
        )),
        message(&format!(
            r#"{{"v":1,"seq":1,"state":"thinking","pad":"{}"}}"#,
            long(MAX_LIVE_MESSAGE_BYTES)
        )),
    ];
    for text in refused {
        assert!(parse_update(&text).is_none(), "{text:.80}");
    }
    // A preview for another app is dropped; the update still counts.
    let chat = parse_update(&message(
        r#"{"v":1,"seq":1,"state":"writing","label":"Writing","preview":{"app":"so.tilo.chat","record":"","text":"hi"}}"#,
    ))
    .unwrap();
    assert!(chat.preview.is_none());
    assert_eq!(chat.label, "Writing");
    // Exactly the H-agent encoding (absent options omitted, fields in order).
    let agent = parse_update(&message(
        r#"{"v":1,"seq":4,"state":"tool","label":"Looking up operations","tool":"list_operations","reply":{"text":"","done":false},"preview":{"app":"so.tilo.word-processor","record":"tab:0000000000000001","text":"x"}}"#,
    ))
    .unwrap();
    assert_eq!(agent.preview.unwrap().record, "tab:0000000000000001");
    // Reply at the bound passes.
    assert!(
        parse_update(&message(&format!(
            r#"{{"v":1,"seq":1,"state":"replying","reply":{{"text":"{}","done":true}}}}"#,
            long(4096)
        )))
        .is_some()
    );
}

#[test]
fn bus_is_bounded_non_blocking_and_token_scoped() {
    let _serial = SERIAL.lock().unwrap_or_else(|poison| poison.into_inner());
    let ended = || AgentLiveEvent::Ended {
        agent_id: "a".into(),
        state: "done".into(),
    };
    let (_old, old_token) = install();
    let (mut receiver, token) = install();
    // An older token never uninstalls the newer bus.
    uninstall(old_token);
    assert!(installed());
    assert!(emit(ended()));
    assert_eq!(receiver.try_recv().unwrap(), ended());
    for _ in 0..BUS_CAPACITY {
        assert!(emit(ended()));
    }
    assert!(!emit(ended()), "a full queue drops instead of blocking");
    uninstall(token);
    assert!(!installed());
    assert!(!emit(ended()), "no bus drops");
}

#[test]
fn ingest_updates_registry_emits_changes_and_ignores_stale_sequences() {
    let _serial = SERIAL.lock().unwrap_or_else(|poison| poison.into_inner());
    let (mut receiver, token) = install();
    let agent = "agent-ingest";
    let invocation = Uuid::from_u128(11);
    let update = |seq: u64, state: &str, reply: &str, preview: Option<&str>| AgentLiveUpdate {
        v: 1,
        seq,
        state: state.into(),
        label: "Writing in Writer".into(),
        tool: None,
        reply: Some(LiveReply {
            text: reply.into(),
            done: false,
        }),
        preview: preview.map(|text| LivePreview {
            app: WRITER_APP.into(),
            record: String::new(),
            text: text.into(),
        }),
    };
    let caret = Caret {
        block: "b1".into(),
        offset: 4,
    };
    let mut asked = 0;
    ingest(
        agent,
        "Scribe",
        invocation,
        update(1, "writing", "Sure", Some("The")),
        |_, _| {
            asked += 1;
            Some(caret.clone())
        },
    );
    assert_eq!(asked, 1);
    assert!(
        matches!(receiver.try_recv().unwrap(), AgentLiveEvent::Activity { state, name, .. } if state == "writing" && name == "Scribe")
    );
    assert_eq!(
        receiver.try_recv().unwrap(),
        AgentLiveEvent::Preview {
            agent_id: agent.into(),
            name: "Scribe".into(),
            app: WRITER_APP.into(),
            record: String::new(),
            pending_text: "The".into(),
            caret: Some(caret.clone()),
        }
    );
    // Same activity: no Activity event; the known caret is reused.
    ingest(
        agent,
        "Scribe",
        invocation,
        update(2, "writing", "Sure, writing", Some("The ocean")),
        |_, _| panic!("caret already known"),
    );
    assert!(
        matches!(receiver.try_recv().unwrap(), AgentLiveEvent::Preview { pending_text, .. } if pending_text == "The ocean")
    );
    assert_eq!(
        live_reply(agent, &invocation.to_string()).unwrap().0,
        "Sure, writing"
    );
    assert!(
        live_reply(agent, &Uuid::from_u128(12).to_string()).is_none(),
        "other task"
    );
    // Stale sequence: ignored entirely.
    ingest(
        agent,
        "Scribe",
        invocation,
        update(2, "done", "stale", None),
        |_, _| None,
    );
    assert!(receiver.try_recv().is_err());
    assert_eq!(
        live_reply(agent, &invocation.to_string()).unwrap().0,
        "Sure, writing"
    );
    // A commit moves the caret; the terminal update ends the live reply.
    let moved = Caret {
        block: "b2".into(),
        offset: 9,
    };
    committed(
        agent,
        "Scribe",
        WRITER_APP,
        "",
        b"canonical".to_vec(),
        Some(moved.clone()),
    );
    assert!(
        matches!(receiver.try_recv().unwrap(), AgentLiveEvent::Committed { canonical, .. } if canonical == b"canonical")
    );
    assert_eq!(caret_for(agent, WRITER_APP, ""), Some(moved));
    ended(agent, invocation, true);
    assert_eq!(
        receiver.try_recv().unwrap(),
        AgentLiveEvent::Ended {
            agent_id: agent.into(),
            state: "done".into()
        }
    );
    assert!(live_reply(agent, &invocation.to_string()).is_none());
    uninstall(token);
}

#[test]
fn display_names_are_bounded_and_never_empty() {
    assert_eq!(display_name("  Scribe "), "Scribe");
    assert_eq!(display_name(""), "Agent");
    assert_eq!(display_name("\n"), "Agent");
    assert_eq!(display_name(&"n".repeat(100)).len(), MAX_AGENT_NAME_BYTES);
}

#[test]
fn capture_records_only_inside_a_capturing_scope() {
    let made = std::cell::Cell::new(0);
    let make = || {
        made.set(made.get() + 1);
        Some(CommitCapture {
            app: "a".into(),
            record: String::new(),
            canonical: vec![1],
            before: None,
            after: None,
        })
    };
    capture(make);
    assert_eq!(made.get(), 0, "no scope: nothing built");
    let ((), captured) = capturing(|| capture(make));
    assert_eq!(captured.unwrap().canonical, vec![1]);
    let ((), captured) = capturing(|| ());
    assert!(captured.is_none());
    capture(make);
    assert_eq!(made.get(), 1, "scope restored");
}

fn writer(paragraphs: &[&str]) -> (qdoc::Document, Vec<String>) {
    let mut doc = qdoc::Document::new();
    let mut ids = Vec::new();
    for text in paragraphs {
        let id = doc.insert_block(ids.last().map(String::as_str), "paragraph");
        doc.splice_text(&id, 0, 0, text);
        ids.push(id);
    }
    (doc, ids)
}

#[test]
fn writer_caret_is_the_utf8_end_of_the_grown_range() {
    let (mut doc, ids) = writer(&["Hello", "Wörld"]);
    let before = doc.save();
    assert_eq!(
        document_end_caret(&before),
        Some(Caret {
            block: ids[1].clone(),
            offset: "Wörld".len()
        })
    );
    // Continue the last paragraph (multibyte text): byte offset of the end.
    doc.splice_text(&ids[1], 5, 0, " — ok");
    let after = doc.save();
    assert_eq!(
        commit_caret(Some(&before), &after),
        Some(Caret {
            block: ids[1].clone(),
            offset: "Wörld — ok".len()
        })
    );
    // Insert into the middle of the first paragraph: end of the inserted run.
    let mut middle = qdoc::Document::load(&after).unwrap();
    middle.splice_text(&ids[0], 2, 0, "XY");
    let grown = middle.save();
    assert_eq!(
        commit_caret(Some(&after), &grown),
        Some(Caret {
            block: ids[0].clone(),
            offset: 4
        })
    );
    // A new paragraph: end of its text.
    let mut appended = qdoc::Document::load(&grown).unwrap();
    let new = appended.insert_block(Some(&ids[1]), "paragraph");
    appended.splice_text(&new, 0, 0, "Neu");
    let appended_bytes = appended.save();
    assert_eq!(
        commit_caret(Some(&grown), &appended_bytes),
        Some(Caret {
            block: new,
            offset: 3
        })
    );
    // Pure deletion or no change: no caret.
    let mut deleted = qdoc::Document::load(&appended_bytes).unwrap();
    deleted.splice_text(&ids[0], 0, 1, "");
    assert_eq!(commit_caret(Some(&appended_bytes), &deleted.save()), None);
    assert_eq!(commit_caret(Some(&appended_bytes), &appended_bytes), None);
    assert!(writer_blocks(b"not a document").is_empty());
}

#[test]
fn worker_commit_capture_keeps_exact_canonical_bytes_and_the_writer_caret() {
    const RECORD: &str = "tab:0000000000000001";
    let (dir, mut store, device) = crate::agent_owner_tests::enrolled();
    store
        .activate_delivery_storage(qos_relay::clock::now_ns())
        .unwrap();
    drop(store);
    let canonical =
        crate::delivery::admission::tests::browser_delta(dir.path(), "agent-admission", "Hi ");
    let message: qos_peer_state::NeighbourMsg = postcard::from_bytes(&canonical).unwrap();
    let mut store = crate::Store::open(dir.path(), "agent-admission").unwrap();
    let before = store.peer_at(store.policy_now_ns().unwrap()).unwrap();
    store
        .admit_delivery_item(device, [7; 16], &canonical, 1024 * 1024)
        .unwrap();
    let after = store.peer_at(store.policy_now_ns().unwrap()).unwrap();
    let route = ui::collaboration::RouteSpec {
        app_id: WRITER_APP.into(),
        cluster_id: store.state.room.parse().unwrap(),
        record: Some(RECORD.into()),
    };
    // Outside a capturing scope nothing is recorded (ordinary commits pay nothing).
    capture_worker_commit(&route, RECORD, &message, &before, &after);
    let ((), captured) =
        capturing(|| capture_worker_commit(&route, RECORD, &message, &before, &after));
    let captured = captured.expect("captured inside the scope");
    assert_eq!(captured.canonical, canonical, "exact canonical item");
    assert_eq!(
        (captured.app.as_str(), captured.record.as_str()),
        (WRITER_APP, RECORD)
    );
    let caret = commit_caret(
        captured.before.as_deref(),
        captured.after.as_deref().unwrap(),
    )
    .unwrap();
    assert_eq!(caret.offset, "Hi ".len());
    let blocks = writer_blocks(captured.after.as_deref().unwrap());
    assert!(
        blocks
            .iter()
            .any(|(id, text)| *id == caret.block && text.starts_with("Hi "))
    );
}

/// Compile-shape check of the proposed one-line hooks in the lane G files
/// (`agent_documents.rs`, `agent_runtime_adapter.rs`); never run.
#[allow(
    dead_code,
    clippy::needless_pass_by_value,
    reason = "type-checks the proposed hook shapes without calling them"
)]
fn proposed_hook_shapes(
    owner: &crate::agent_runtime_adapter::RuntimeOwner,
    attempt: std::sync::Arc<crate::agent_runtime_adapter::LiveAttempt>,
    update: &qos_action_broker::enrolled_dispatch::VerifiedTaskUpdate,
    route: &ui::collaboration::RouteSpec,
    certificate: &qos_user_identity::DeviceCert,
    mutation: collab_types::DocumentMutation,
    authorization: Option<&ui::invocation_scope::DocumentAuthorization>,
    message: qos_peer_state::NeighbourMsg,
    before: qos_peer_state::StatePeer,
    mut candidate: qos_peer_state::StatePeer,
) -> Result<
    (
        collab_types::SignedOp,
        qos_peer_state::owner_commit::OwnerCommitReceipt,
    ),
    qos_action_broker::host_documents::HostDocumentTurnError,
> {
    use qos_action_broker::host_documents::HostDocumentTurnError;
    let record = route.record.as_deref().unwrap_or("");
    capture_worker_commit(route, record, &message, &before, &candidate);
    let _ = &mut candidate;
    on_progress(owner, &attempt, update.invocation(), update.update());
    on_update(&attempt, update.invocation(), update.update());
    let mut store = owner.store().map_err(HostDocumentTurnError::Refused)?;
    on_worker_commit(
        owner,
        authorization,
        &mut store,
        |store| match authorization {
            Some(authorization) => {
                store.commit_scoped_agent_document(route, certificate, mutation, authorization)
            }
            None => store
                .commit_agent_document(route, certificate, mutation)
                .map_err(HostDocumentTurnError::Refused),
        },
    )
}

#[test]
fn worker_participant_presence_becomes_the_agents_real_caret() {
    use ui::{
        collaboration::PresencePayload,
        host_worker::ParticipantPresenceUpdate,
        presence::{PathSegment, PresenceKey},
        widget::ParticipantPresence,
    };
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (mut receiver, token) = install();
    let agent = "agent-presence";
    let key = |block: &str| PresenceKey::Widget {
        path: vec![
            PathSegment::Field("wp_block".into()),
            PathSegment::Field(block.into()),
        ],
    };
    let participant = qos_actions::ParticipantId::for_agent_task("task-7").unwrap();
    let human = qos_actions::ParticipantId::new("person-1").unwrap();
    let resolve = |task: &str| (task == "task-7").then(|| agent.to_owned());
    on_participant_presence(
        WRITER_APP,
        "",
        vec![
            // A human participant and a non-Writer key are never published.
            ParticipantPresenceUpdate::Changed(ParticipantPresence {
                participant: human,
                label: Some("Ada".into()),
                key: key("b1"),
                payload: PresencePayload::Caret { pos: 1 },
            }),
            ParticipantPresenceUpdate::Changed(ParticipantPresence {
                participant: participant.clone(),
                label: Some("Scribe".into()),
                key: PresenceKey::View { data: vec![1] },
                payload: PresencePayload::Caret { pos: 1 },
            }),
            ParticipantPresenceUpdate::Changed(ParticipantPresence {
                participant: participant.clone(),
                label: Some("Scribe".into()),
                key: key("b3"),
                payload: PresencePayload::Selection { start: 2, end: 7 },
            }),
        ],
        &resolve,
    );
    assert_eq!(
        receiver.try_recv().unwrap(),
        AgentLiveEvent::Presence {
            agent_id: agent.into(),
            name: "Scribe".into(),
            app: WRITER_APP.into(),
            record: String::new(),
            caret: Some(Caret {
                block: "b3".into(),
                offset: 2
            }),
            head: Some(7),
        }
    );
    assert!(receiver.try_recv().is_err());
    // Latest wins: a later commit-derived caret moves it again.
    committed(
        agent,
        "Scribe",
        WRITER_APP,
        "",
        b"c".to_vec(),
        Some(Caret {
            block: "b9".into(),
            offset: 1,
        }),
    );
    let _ = receiver.try_recv();
    assert_eq!(
        caret_for(agent, WRITER_APP, ""),
        Some(Caret {
            block: "b9".into(),
            offset: 1
        })
    );
    // Removed clears the caret.
    on_participant_presence(
        WRITER_APP,
        "",
        vec![ParticipantPresenceUpdate::Removed(participant)],
        &resolve,
    );
    assert!(matches!(
        receiver.try_recv().unwrap(),
        AgentLiveEvent::Presence {
            caret: None,
            head: None,
            ..
        }
    ));
    assert_eq!(caret_for(agent, WRITER_APP, ""), None);
    uninstall(token);
}
