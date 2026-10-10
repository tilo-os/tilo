//! Round-trips for the content, action and invocation verbs.
//!
//! These carry things the earlier verbs could not express — the bytes
//! behind a document's reference, an instruction addressed to another
//! app's document, and the operations one app exports to the session —
//! so each test drives the client wrapper against an in-memory broker and
//! asserts on what actually crossed the socket.
//!
//! Everything runs against [`crate::fake_broker`] over a bounded in-memory
//! byte stream.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::cell::RefCell;
use std::rc::Rc;

use tokio::task::LocalSet;

use crate::fake_broker::{
    Captures, FakeBroker, PublishedInput, SeedInvocation, SharedCaptures, stand_up_client,
};
use runtime_collab::{ExportedAction, ExportedInput};

// tests

#[tokio::test(flavor = "current_thread")]
async fn blob_offers_wants_and_streams_reach_the_broker_verbatim() {
    // phase: offer_and_want_blob_reach_the_broker_verbatim
    {
        let local = LocalSet::new();
        local
            .run_until(async {
                let captures: SharedCaptures = Rc::new(RefCell::new(Captures::default()));
                let broker = FakeBroker {
                    captures: captures.clone(),
                    ..Default::default()
                };
                let collab = stand_up_client(broker);
                let channel = collab.join_channel("chan-blob", b"identity").await.unwrap();

                channel
                    .offer_blob("b3-abc", b"\x00\x01\x02binary")
                    .await
                    .unwrap();
                channel.want_blob("b3-def").await.unwrap();

                let c = captures.borrow();
                assert_eq!(
                    c.offered_blobs,
                    vec![("b3-abc".to_string(), b"\x00\x01\x02binary".to_vec())]
                );
                assert_eq!(c.wanted_blobs, vec!["b3-def".to_string()]);
            })
            .await;
    }
    // phase: subscribe_blobs_yields_hash_and_bytes
    {
        let local = LocalSet::new();
        local
            .run_until(async {
                let broker = FakeBroker {
                    blob_backlog: vec![
                        ("b3-one".into(), b"first".to_vec()),
                        ("b3-two".into(), vec![0xff, 0x00, 0xff]),
                    ],
                    ..Default::default()
                };
                let collab = stand_up_client(broker);
                let channel = collab
                    .join_channel("chan-blob-rx", b"identity")
                    .await
                    .unwrap();
                let stream = channel.subscribe_blobs().await.unwrap();

                let first = stream.read().await.unwrap();
                assert_eq!(first.hash, "b3-one");
                assert_eq!(first.bytes, b"first");

                // Non-UTF8 content survives: `bytes` is Data, and the only
                // text on this stream is the hash.
                let second = stream.read().await.unwrap();
                assert_eq!(second.hash, "b3-two");
                assert_eq!(second.bytes, vec![0xff, 0x00, 0xff]);
            })
            .await;
    }
}

#[tokio::test(flavor = "current_thread")]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
async fn actions_and_remarks_carry_type_attribution_and_opaque_payloads() {
    // phase: send_action_carries_type_id_and_opaque_payload
    {
        let local = LocalSet::new();
        local
            .run_until(async {
                let captures: SharedCaptures = Rc::new(RefCell::new(Captures::default()));
                let broker = FakeBroker {
                    captures: captures.clone(),
                    ..Default::default()
                };
                let collab = stand_up_client(broker);
                let channel = collab.join_channel("chan-act", b"identity").await.unwrap();

                channel
                    .send_action("word-processor.splice", b"\x01\x02opaque")
                    .await
                    .unwrap();

                let c = captures.borrow();
                assert_eq!(
                    c.sent_actions,
                    vec![(
                        "word-processor.splice".to_string(),
                        b"\x01\x02opaque".to_vec()
                    )]
                );
            })
            .await;
    }
    // phase: subscribe_actions_keeps_sender_attribution_separate_from_payload
    {
        let local = LocalSet::new();
        local
            .run_until(async {
                let broker = FakeBroker {
                    action_backlog: vec![(
                        "chat.remark".into(),
                        b"payload".to_vec(),
                        "peer-3".into(),
                    )],
                    ..Default::default()
                };
                let collab = stand_up_client(broker);
                let channel = collab
                    .join_channel("chan-act-rx", b"identity")
                    .await
                    .unwrap();
                let stream = channel.subscribe_actions().await.unwrap();

                let ev = stream.read().await.unwrap();
                assert_eq!(ev.type_id, "chat.remark");
                assert_eq!(ev.payload, b"payload");
                // The broker's claim about who sent the frame, which the
                // payload never gets to overwrite.
                assert_eq!(ev.from_user_id, "peer-3");
            })
            .await;
    }
    // phase: publish_actions_round_trips_both_input_shapes
    {
        let local = LocalSet::new();
        local
            .run_until(async {
                let captures: SharedCaptures = Rc::new(RefCell::new(Captures::default()));
                let broker = FakeBroker {
                    captures: captures.clone(),
                    ..Default::default()
                };
                let collab = stand_up_client(broker);
                let channel = collab.join_channel("chan-pub", b"identity").await.unwrap();

                channel
                    .publish_actions(
                        "word-processor",
                        &[
                            ExportedAction {
                                semantic_identity: Some(
                                    runtime_collab::ExportedResourceSemanticIdentity {
                                        kind: "document".into(),
                                        key: "source-document:plan".into(),
                                    },
                                ),
                                resource: "word-processor.document".into(),
                                action: "save".into(),
                                resource_kind: "document".into(),
                                resource_label: "Plan".into(),
                                action_label: "Save".into(),
                                description: None,
                                operation_kind: tilo_actions::OperationKind::Effect,
                                params: Vec::new(),
                                contract: tilo_actions::ActionContract::default(),
                                input: ExportedInput::None,
                                view: None,
                            },
                            ExportedAction {
                                semantic_identity: None,
                                resource: "word-processor.document".into(),
                                action: "insert-image".into(),
                                resource_kind: "document".into(),
                                resource_label: "Plan".into(),
                                action_label: "Insert image".into(),
                                description: Some("Place an image at the caret".into()),
                                operation_kind: tilo_actions::OperationKind::Document,
                                params: Vec::new(),
                                contract: tilo_actions::ActionContract::default(),
                                input: ExportedInput::Text {
                                    prompt: "Which image?".into(),
                                    placeholder: "content hash".into(),
                                    wants_content: true,
                                },
                                view: None,
                            },
                        ],
                    )
                    .await
                    .unwrap();

                let c = captures.borrow();
                assert_eq!(c.published.len(), 1);
                let (app_id, rows) = &c.published[0];
                assert_eq!(app_id, "word-processor");
                assert_eq!(rows.len(), 2);

                assert_eq!(rows[0].action, "save");
                assert_eq!(rows[0].resource, "word-processor.document");
                assert_eq!(
                    rows[0].semantic_identity,
                    Some(("document".into(), "source-document:plan".into()))
                );
                assert!(rows[1].semantic_identity.is_none());
                // An absent description and an empty one are the same fact,
                // so `None` has to arrive as the empty string.
                assert_eq!(rows[0].description, "");
                assert_eq!(rows[0].operation_kind, "effect");
                assert_eq!(rows[0].params_json, "[]");
                assert_eq!(
                    serde_json::from_str::<tilo_actions::ActionContract>(&rows[0].contract_json)
                        .unwrap(),
                    tilo_actions::ActionContract::default()
                );
                assert_eq!(rows[0].input, PublishedInput::None);

                assert_eq!(rows[1].action, "insert-image");
                assert_eq!(rows[1].resource_kind, "document");
                assert_eq!(rows[1].action_label, "Insert image");
                assert_eq!(rows[1].description, "Place an image at the caret");
                assert_eq!(rows[1].operation_kind, "document");
                assert_eq!(rows[1].params_json, "[]");
                assert_eq!(
                    rows[1].input,
                    PublishedInput::Text {
                        prompt: "Which image?".into(),
                        placeholder: "content hash".into(),
                        wants_content: true,
                    }
                );
            })
            .await;
    }
    // phase: compose_remark_carries_the_reference_and_the_window_local_rect
    {
        let local = LocalSet::new();
        local
            .run_until(async {
                let captures: SharedCaptures = Rc::new(RefCell::new(Captures::default()));
                let broker = FakeBroker {
                    captures: captures.clone(),
                    ..Default::default()
                };
                let collab = stand_up_client(broker);
                let channel = collab
                    .join_channel("chan-remark", b"identity")
                    .await
                    .unwrap();

                let reference = r#"{"referent":{"Doc":{"id":"plan"}}}"#;
                channel
                    .compose_remark(reference, (12.0, 34.0, 200.0, 24.0))
                    .await
                    .unwrap();

                let c = captures.borrow();
                assert_eq!(c.remarks.len(), 1);
                assert_eq!(c.remarks[0].0, reference);
                assert_eq!(c.remarks[0].1, (12.0, 34.0, 200.0, 24.0));
            })
            .await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn invocations_distinguish_empty_arguments_and_report_every_outcome() {
    // phase: invocation_distinguishes_an_empty_argument_from_no_argument
    {
        let local = LocalSet::new();
        local
            .run_until(async {
                let broker = FakeBroker {
                    invocation_backlog: vec![
                        SeedInvocation {
                            invocation_uuid: [0xa5; 16],
                            id: 1,
                            resource: "word-processor.document".into(),
                            action: "save".into(),
                            argument: None,
                            actor: "alice".into(),
                        },
                        SeedInvocation {
                            invocation_uuid: [0x5a; 16],
                            id: 2,
                            resource: "word-processor.document".into(),
                            action: "set-title".into(),
                            argument: Some(String::new()),
                            actor: "agent-7".into(),
                        },
                    ],
                    ..Default::default()
                };
                let collab = stand_up_client(broker);
                let channel = collab.join_channel("chan-inv", b"identity").await.unwrap();
                let stream = channel.subscribe_invocations().await.unwrap();

                let first = stream.read().await.unwrap();
                assert_eq!(first.id, 1);
                assert_eq!(
                    first.invocation_uuid, [0xa5; 16],
                    "high UUID bits survive native SDK delivery"
                );
                assert_eq!(first.action, "save");
                assert_eq!(first.actor, "alice");
                assert_eq!(first.argument, None, "an action that takes nothing");

                // The pair of fields exists for exactly this case: an action legitimately
                // given `""` must not read as one given no argument at all.
                let second = stream.read().await.unwrap();
                assert_eq!(second.id, 2);
                assert_eq!(second.invocation_uuid, [0x5a; 16]);
                assert_eq!(second.argument, Some(String::new()));
                assert_eq!(second.actor, "agent-7");
            })
            .await;
    }
    // phase: complete_invocation_reports_both_success_and_refusal
    {
        let local = LocalSet::new();
        local
            .run_until(async {
                let captures: SharedCaptures = Rc::new(RefCell::new(Captures::default()));
                let broker = FakeBroker {
                    captures: captures.clone(),
                    ..Default::default()
                };
                let collab = stand_up_client(broker);
                let channel = collab.join_channel("chan-done", b"identity").await.unwrap();

                channel.complete_invocation(1, "", "").await.unwrap();
                channel
                    .complete_invocation(2, "unknown_action", "no such action on this resource")
                    .await
                    .unwrap();

                let c = captures.borrow();
                assert_eq!(
                    c.completions,
                    vec![
                        // Empty code is the "it ran" answer.
                        (1, String::new(), String::new()),
                        (
                            2,
                            "unknown_action".to_string(),
                            "no such action on this resource".to_string()
                        ),
                    ]
                );
            })
            .await;
    }
}
