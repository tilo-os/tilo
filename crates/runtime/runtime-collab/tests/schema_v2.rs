//! Integration tests for schema v2 additions:
//!
//! - Extended presence: `publish_caret`, `publish_selection`,
//!   `publish_arrived`, `publish_left`, and the unified `subscribe_all`
//!   stream carrying every payload variant.
//! - Roster/context: `subscribe_context` yielding `ChannelContext` snapshots.
//! - Signed op stream attribution: `Stream.read()` returns `(op,
//!   fromUserId)`, decoded by the wrapper.
//! - Scrollback: `fetch_scrollback_for` decodes `ScrollbackBatch`.
//!
//! No real broker — everything runs against [`crate::fake_broker`] over a
//! bounded in-memory byte stream.

use std::cell::RefCell;
use std::rc::Rc;

use tokio::task::LocalSet;

use crate::fake_broker::{
    Captures, FakeBroker, PresencePayloadKind, SeedContext, SeedPresence, SeedScrollback,
    stand_up_client,
};
use runtime_collab::PresencePayload;

fn every_presence_payload() -> Vec<SeedPresence> {
    vec![
        SeedPresence {
            user_id: "alice".into(),
            key_bytes: b"key-1".to_vec(),
            payload: PresencePayloadKind::Cursor { x: 12.5, y: -3.0 },
        },
        SeedPresence {
            user_id: "alice-keyed".into(),
            key_bytes: b"key-pointer".to_vec(),
            payload: PresencePayloadKind::Pointer { x: 3.5, y: 8.0 },
        },
        SeedPresence {
            user_id: "bob".into(),
            key_bytes: b"key-2".to_vec(),
            payload: PresencePayloadKind::Caret { pos: 42 },
        },
        SeedPresence {
            user_id: "carol".into(),
            key_bytes: b"key-3".to_vec(),
            payload: PresencePayloadKind::Selection { start: 7, end: 11 },
        },
        SeedPresence {
            user_id: "carol-typing".into(),
            key_bytes: b"key-typing".to_vec(),
            payload: PresencePayloadKind::Typing { start: 12, end: 15 },
        },
        SeedPresence {
            user_id: "frank".into(),
            key_bytes: b"key-status".to_vec(),
            payload: PresencePayloadKind::Status {
                text: "Reconciling forecast".into(),
                automatic: true,
            },
        },
        SeedPresence {
            user_id: "dave".into(),
            key_bytes: b"key-4".to_vec(),
            payload: PresencePayloadKind::Arrived,
        },
        SeedPresence {
            user_id: "eve".into(),
            key_bytes: b"key-5".to_vec(),
            payload: PresencePayloadKind::Left,
        },
    ]
}

// tests

#[tokio::test(flavor = "current_thread")]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
async fn presence_and_cursor_publishes_round_trip() {
    // phase: presence_variants_publish_and_stream_round_trip
    {
        let local = LocalSet::new();
        local
            .run_until(async {
                let captures = Rc::new(RefCell::new(Captures::default()));

                let broker = FakeBroker {
                    captures: captures.clone(),
                    presence_backlog: every_presence_payload(),
                    ..Default::default()
                };
                let collab = stand_up_client(broker);
                let channel = collab.join_channel("c", b"id").await.unwrap();
                let presence = channel.presence().await.unwrap();

                presence.publish_cursor(1.5, 2.5).await.unwrap();
                presence
                    .publish_pointer(b"art-pointer", 4.0, 5.0)
                    .await
                    .unwrap();
                presence.publish_caret(b"art-a", 100).await.unwrap();
                presence.publish_selection(b"art-b", 3, 9).await.unwrap();
                presence.publish_typing(b"art-typing", 6, 10).await.unwrap();
                presence
                    .publish_status(b"room", "Reconciling forecast", true)
                    .await
                    .unwrap();
                presence.publish_arrived(b"art-c").await.unwrap();
                presence.publish_left(b"art-d").await.unwrap();

                {
                    let c = captures.borrow();
                    assert_eq!(c.presence_cursor, vec![(1.5, 2.5)]);
                    assert_eq!(
                        c.presence_pointer,
                        vec![(b"art-pointer".to_vec(), 4.0, 5.0)]
                    );
                    assert_eq!(c.presence_caret, vec![(b"art-a".to_vec(), 100)]);
                    assert_eq!(c.presence_selection, vec![(b"art-b".to_vec(), 3, 9)]);
                    assert_eq!(c.presence_typing, vec![(b"art-typing".to_vec(), 6, 10)]);
                    assert_eq!(
                        c.presence_status,
                        vec![(b"room".to_vec(), "Reconciling forecast".into(), true)]
                    );
                    assert_eq!(c.presence_arrived, vec![b"art-c".to_vec()]);
                    assert_eq!(c.presence_left, vec![b"art-d".to_vec()]);
                }

                let stream = presence.subscribe_all().await.unwrap();

                let e1 = stream.read().await.unwrap();
                assert_eq!(e1.user_id, "alice");
                assert_eq!(e1.key_bytes, b"key-1");
                assert_eq!(e1.payload, PresencePayload::Cursor { x: 12.5, y: -3.0 });

                let e2 = stream.read().await.unwrap();
                assert_eq!(e2.user_id, "alice-keyed");
                assert_eq!(e2.key_bytes, b"key-pointer");
                assert_eq!(e2.payload, PresencePayload::Pointer { x: 3.5, y: 8.0 });

                let e3 = stream.read().await.unwrap();
                assert_eq!(e3.user_id, "bob");
                assert_eq!(e3.payload, PresencePayload::Caret { pos: 42 });

                let e4 = stream.read().await.unwrap();
                assert_eq!(e4.user_id, "carol");
                assert_eq!(e4.payload, PresencePayload::Selection { start: 7, end: 11 });

                let e5 = stream.read().await.unwrap();
                assert_eq!(e5.user_id, "carol-typing");
                assert_eq!(e5.key_bytes, b"key-typing");
                assert_eq!(e5.payload, PresencePayload::Typing { start: 12, end: 15 });

                let e6 = stream.read().await.unwrap();
                assert_eq!(e6.user_id, "frank");
                assert_eq!(
                    e6.payload,
                    PresencePayload::Status {
                        text: "Reconciling forecast".into(),
                        automatic: true
                    }
                );

                let e7 = stream.read().await.unwrap();
                assert_eq!(e7.user_id, "dave");
                assert_eq!(e7.payload, PresencePayload::Arrived);

                let e8 = stream.read().await.unwrap();
                assert_eq!(e8.user_id, "eve");
                assert_eq!(e8.payload, PresencePayload::Left);
            })
            .await;
    }
    // phase: keyless_publish_cursor_still_works
    {
        let local = LocalSet::new();
        local
            .run_until(async {
                let captures = Rc::new(RefCell::new(Captures::default()));
                let broker = FakeBroker {
                    captures: captures.clone(),
                    ..Default::default()
                };
                let collab = stand_up_client(broker);
                let channel = collab.join_channel("c", b"id").await.unwrap();
                let presence = channel.presence().await.unwrap();

                presence.publish_cursor(11.0, 22.0).await.unwrap();
                assert_eq!(
                    captures.borrow().presence_cursor,
                    vec![(11.0, 22.0)],
                    "publish_cursor kept @0 semantics"
                );
            })
            .await;
    }
}

#[tokio::test(flavor = "current_thread")]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
async fn subscriptions_and_scrollback_decode_typed_batches() {
    // phase: subscribe_context_delivers_typed_snapshot
    {
        let local = LocalSet::new();
        local
            .run_until(async {
                let captures = Rc::new(RefCell::new(Captures::default()));
                let context_backlog = vec![
                    SeedContext {
                        collaborators: vec![
                            ("alice".into(), "Alice".into(), 0x00ff_0000),
                            ("bob".into(), "Bob".into(), 0x00_00ff00),
                        ],
                        permissions: vec!["write".into(), "share".into()],
                    },
                    SeedContext {
                        collaborators: vec![("alice".into(), "Alice".into(), 0x00ff_0000)],
                        permissions: vec!["write".into()],
                    },
                ];
                let broker = FakeBroker {
                    captures: captures.clone(),
                    context_backlog,
                    ..Default::default()
                };
                let collab = stand_up_client(broker);
                let channel = collab.join_channel("c", b"id").await.unwrap();
                let stream = channel.subscribe_context().await.unwrap();

                let s1 = stream.read().await.unwrap();
                assert_eq!(s1.collaborators.len(), 2);
                assert_eq!(s1.collaborators[0].user_id, "alice");
                assert_eq!(s1.collaborators[0].display_name, "Alice");
                assert_eq!(s1.collaborators[0].color, 0x00ff_0000);
                assert_eq!(s1.collaborators[1].user_id, "bob");
                assert_eq!(s1.permissions, vec!["write", "share"]);

                let s2 = stream.read().await.unwrap();
                assert_eq!(s2.collaborators.len(), 1);
                assert_eq!(s2.permissions, vec!["write"]);
            })
            .await;
    }
    // phase: op_stream_carries_from_user_id
    {
        let local = LocalSet::new();
        local
            .run_until(async {
                let captures = Rc::new(RefCell::new(Captures::default()));
                let op_backlog = vec![
                    (b"first-op".to_vec(), "alice".to_string()),
                    (b"second-op".to_vec(), "peer-42".to_string()),
                ];
                let broker = FakeBroker {
                    captures: captures.clone(),
                    op_backlog,
                    ..Default::default()
                };
                let collab = stand_up_client(broker);
                let channel = collab.join_channel("c", b"id").await.unwrap();
                let stream = channel.subscribe().await.unwrap();

                let ev1 = stream.read().await.unwrap();
                assert_eq!(ev1.op, b"first-op");
                assert_eq!(ev1.from_user_id, "alice");

                let ev2 = stream.read().await.unwrap();
                assert_eq!(ev2.op, b"second-op");
                assert_eq!(ev2.from_user_id, "peer-42");
            })
            .await;
    }
    // phase: fetch_scrollback_round_trip_full_batch
    {
        let local = LocalSet::new();
        local
            .run_until(async {
                let captures = Rc::new(RefCell::new(Captures::default()));
                let scrollback_response = Some(SeedScrollback {
                    ops: vec![b"op-a".to_vec(), b"op-b".to_vec(), b"op-c".to_vec()],
                    earliest_seq: 7,
                    latest_seq: 9,
                    done: false,
                });
                let broker = FakeBroker {
                    captures: captures.clone(),
                    scrollback_response,
                    ..Default::default()
                };
                let collab = stand_up_client(broker);
                let channel = collab.join_channel("c", b"id").await.unwrap();

                let artifact_id = [7_u8; 16];
                let batch = channel
                    .fetch_scrollback_for(&artifact_id, 50, 25)
                    .await
                    .unwrap();
                assert_eq!(
                    batch.ops,
                    vec![b"op-a".to_vec(), b"op-b".to_vec(), b"op-c".to_vec()]
                );
                assert_eq!(batch.earliest_seq, 7);
                assert_eq!(batch.latest_seq, 9);
                assert!(!batch.done);
                assert_eq!(
                    captures.borrow().scrollback_requests,
                    vec![(artifact_id.to_vec(), 50, 25)]
                );
            })
            .await;
    }
    // phase: fetch_scrollback_empty_done_batch_decodes
    {
        let local = LocalSet::new();
        local
            .run_until(async {
                let captures = Rc::new(RefCell::new(Captures::default()));
                // No scrollback_response means the fake replies with an empty+done batch —
                // the "broker holds nothing" path clients must distinguish from "here's a page".
                let broker = FakeBroker {
                    captures: captures.clone(),
                    ..Default::default()
                };
                let collab = stand_up_client(broker);
                let channel = collab.join_channel("c", b"id").await.unwrap();

                let batch = channel
                    .fetch_scrollback_for(&[8_u8; 16], 0, 10)
                    .await
                    .unwrap();
                assert!(batch.ops.is_empty());
                assert!(batch.done);
                assert_eq!(batch.earliest_seq, 0);
                assert_eq!(batch.latest_seq, 0);
            })
            .await;
    }
}
