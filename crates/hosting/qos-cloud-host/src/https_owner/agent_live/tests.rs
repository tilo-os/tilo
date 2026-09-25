use super::*;
use qos_moq::live::{LiveAuthority, LiveSession, fallback};
use qos_moq::relay::{Admission, AuthorizationStatus};
use std::sync::{Arc, Mutex};

const ROOM: &str = "4f1e5c3a-0000-4000-8000-000000000001";
const HOST: [u8; 32] = [7; 32];
const RECORD: &str = "tab:0000000000000001";

#[derive(Clone, Debug, PartialEq)]
enum Sent {
    Presence(String, PresenceFrame),
    Ops(String, String, OpsFrame),
}
#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<Sent>>>);
impl LiveSink for Recorder {
    fn presence(&self, room: &str, publisher: &str, frame: &[u8]) -> bool {
        assert_eq!(room, hex::encode(room_digest(ROOM)));
        let frame = PresenceFrame::decode(frame).expect("canonical presence");
        self.0
            .lock()
            .unwrap()
            .push(Sent::Presence(publisher.into(), frame));
        true
    }
    fn ops(&self, room: &str, publisher: &str, record: &str, frame: &[u8]) -> bool {
        assert_eq!(room, hex::encode(room_digest(ROOM)));
        let frame = OpsFrame::decode(frame).expect("canonical ops frame");
        let named = match &frame {
            OpsFrame::Preview { record, .. } | OpsFrame::SignedChange { record, .. } => record,
        };
        assert_eq!(
            record,
            hex::encode(named.digest()),
            "track names the frame's record"
        );
        self.0
            .lock()
            .unwrap()
            .push(Sent::Ops(publisher.into(), record.into(), frame));
        true
    }
}
impl Recorder {
    fn take(&self) -> Vec<Sent> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

/// Browser `live_view::decode_selection`, reproduced: `(block, start, end)`.
fn decode_selection(anchor: &[u8], head: &[u8]) -> Option<(PresenceKey, u64, u64)> {
    fn uvarint(bytes: &[u8]) -> Option<(u64, &[u8])> {
        let mut value = 0u64;
        for (index, byte) in bytes.iter().enumerate().take(10) {
            value |= u64::from(byte & 0x7f) << (7 * index);
            if byte & 0x80 == 0 {
                return (value <= 1 << 32).then(|| (value, &bytes[index + 1..]));
            }
        }
        None
    }
    if anchor.len() > 64 || head.len() > 64 {
        return None;
    }
    let (&1, rest) = anchor.split_first()? else {
        return None;
    };
    let (start, scope) = uvarint(rest)?;
    let key: PresenceKey = postcard::from_bytes(scope).ok()?;
    if postcard::to_allocvec(&key).ok()? != scope {
        return None;
    }
    let (&2, rest) = head.split_first()? else {
        return None;
    };
    let (end, tail) = uvarint(rest)?;
    tail.is_empty().then_some((key, start, end))
}

fn writer_key(block: &str) -> PresenceKey {
    PresenceKey::Widget {
        path: vec![
            PathSegment::Field("wp_block".into()),
            PathSegment::Field(block.into()),
        ],
    }
}

#[test]
fn caret_anchor_bytes_match_the_browser_encoding() {
    let caret = Caret {
        block: "b1".into(),
        offset: 5,
    };
    let (anchor, head) = caret_selection(&caret).unwrap();
    // Known vector: tag, uvarint(5), postcard(Widget{path:[Field("wp_block"), Field("b1")]}).
    let mut expected = vec![1, 5, 0, 2, 0, 8];
    expected.extend_from_slice(b"wp_block");
    expected.extend_from_slice(&[0, 2]);
    expected.extend_from_slice(b"b1");
    assert_eq!(anchor.cursor, expected);
    assert_eq!(head.cursor, vec![2, 5]);
    // Multi-byte varint offsets and a real block id decode like the browser.
    let caret = Caret {
        block: "2@5f3a9c01d2e4b6a8".into(),
        offset: 300,
    };
    let (anchor, head) = caret_selection(&caret).unwrap();
    assert_eq!(head.cursor, vec![2, 0xac, 0x02]);
    assert_eq!(
        decode_selection(&anchor.cursor, &head.cursor),
        Some((writer_key("2@5f3a9c01d2e4b6a8"), 300, 300))
    );
    // Over the 64-byte scoped bound: published in the digest form.
    let (long, _) = caret_selection(&Caret {
        block: "x".repeat(60),
        offset: 1,
    })
    .unwrap();
    assert_eq!(long.cursor[0], 3);
}

fn activity(sent: &[Sent]) -> Vec<AgentActivity> {
    sent.iter()
        .filter_map(|sent| match sent {
            Sent::Ops(_, _, frame @ OpsFrame::Preview { record, .. })
                if *record == agent_activity_record() =>
            {
                Some(AgentActivity::from_frame(frame).unwrap().1)
            }
            _ => None,
        })
        .collect()
}
fn previews(sent: &[Sent]) -> Vec<Vec<PreviewOp>> {
    sent.iter()
        .filter_map(|sent| match sent {
            Sent::Ops(_, _, OpsFrame::Preview { record, ops, .. }) if record.record == RECORD => {
                Some(ops.clone())
            }
            _ => None,
        })
        .collect()
}
fn presences(sent: &[Sent]) -> Vec<PresenceFrame> {
    sent.iter()
        .filter_map(|sent| match sent {
            Sent::Presence(_, frame) => Some(frame.clone()),
            _ => None,
        })
        .collect()
}
fn inserted(ops: &[PreviewOp]) -> Vec<String> {
    ops.iter()
        .filter_map(|op| match op {
            PreviewOp::Insert { text, .. } => Some(text.clone()),
            PreviewOp::Delete { .. } => None,
        })
        .collect()
}

fn preview_event(text: &str, caret: Option<Caret>) -> AgentLiveEvent {
    AgentLiveEvent::Preview {
        agent_id: "agent-1".into(),
        name: "Scribe".into(),
        app: WRITER_APP.into(),
        record: RECORD.into(),
        pending_text: text.into(),
        caret,
    }
}

#[test]
fn publisher_speaks_in_the_agent_namespace_with_presence_previews_and_activity() {
    let recorder = Recorder::default();
    let mut publisher = Publisher::new(recorder.clone(), ROOM, HOST);
    let expected = hex::encode(qos_relay::delivery_v3::live_wire::agent_publisher(
        ROOM, HOST, "agent-1",
    ));
    assert_eq!(publisher.publisher_of("agent-1"), expected);
    let start = Instant::now();
    publisher.handle(
        AgentLiveEvent::Activity {
            agent_id: "agent-1".into(),
            name: "Scribe".into(),
            state: "thinking".into(),
            label: "Thinking".into(),
            tool: None,
        },
        start,
    );
    let sent = recorder.take();
    assert!(
        sent.iter().all(
            |sent| matches!(sent, Sent::Presence(p, _) | Sent::Ops(p, _, _) if *p == expected)
        )
    );
    let values = activity(&sent);
    assert_eq!(values.len(), 1);
    assert_eq!(
        (
            values[0].agent.as_str(),
            values[0].name.as_str(),
            values[0].state.as_str()
        ),
        ("agent-1", "Scribe", "thinking")
    );
    assert_eq!(
        presences(&sent).len(),
        1,
        "the agent appears as a member at once"
    );

    // First preview: focus + caret presence, then the pending text.
    let caret = Caret {
        block: "b1".into(),
        offset: 5,
    };
    publisher.handle(preview_event("The ocean", Some(caret.clone())), start);
    let sent = recorder.take();
    let presence = presences(&sent);
    assert_eq!(presence.len(), 1);
    assert_eq!(
        presence[0].focus.as_ref().unwrap(),
        &LiveRecord {
            app: WRITER_APP.into(),
            record: RECORD.into()
        }
    );
    let (anchor, head) = presence[0].selection.clone().unwrap();
    assert_eq!(
        decode_selection(&anchor.cursor, &head.cursor),
        Some((writer_key("b1"), 5, 5))
    );
    assert!(
        previews(&sent).is_empty(),
        "pending text is revealed at the typing cadence, not dumped"
    );
    // 250 ms at 30 chars/s reveals the first 7 characters, anchored at the caret.
    publisher.tick(start + Duration::from_millis(250));
    let ops = previews(&recorder.take());
    assert_eq!(ops.len(), 1);
    assert_eq!(inserted(&ops[0]), ["The oce"]);
    let PreviewOp::Insert { at, .. } = &ops[0][0] else {
        panic!()
    };
    assert_eq!(at, &anchor, "previews are anchored at the caret");

    // Appended text (the model is still streaming it) keeps the reveal going:
    // only newly revealed characters are sent.
    publisher.handle(preview_event("The ocean covers", None), start);
    assert!(recorder.take().is_empty());
    publisher.tick(start + Duration::from_millis(500));
    assert_eq!(inserted(&previews(&recorder.take())[0]), ["an cove"]);
    // Unchanged text: nothing new until the next reveal step.
    publisher.handle(preview_event("The ocean covers", None), start);
    assert!(recorder.take().is_empty());
    // A revision that no longer extends: withdraw what was shown, reveal anew.
    publisher.handle(preview_event("The sea covers", None), start);
    let ops = previews(&recorder.take());
    assert!(
        matches!(ops[0][0], PreviewOp::Delete { len: 14, .. }),
        "{ops:?}"
    );
    assert!(inserted(&ops[0]).is_empty());
    // Fully revealed after enough ticks, then re-sent every 400 ms.
    let mut shown = String::new();
    for ms in (600..=1_600).step_by(100) {
        publisher.tick(start + Duration::from_millis(ms));
        for ops in previews(&recorder.take()) {
            shown.extend(inserted(&ops));
        }
    }
    assert!(shown.starts_with("The sea covers"), "{shown:?}");
    publisher.tick(start + Duration::from_millis(2_050));
    let ops = previews(&recorder.take());
    assert!(
        ops.iter()
            .any(|ops| matches!(ops[0], PreviewOp::Delete { .. })
                && inserted(ops) == ["The sea covers"]),
        "{ops:?}"
    );
    publisher.tick(start + Duration::from_millis(3100));
    assert!(!activity(&recorder.take()).is_empty(), "activity heartbeat");
    publisher.tick(start + Duration::from_millis(4200));
    let sent = recorder.take();
    assert_eq!(presences(&sent).len(), 1, "presence heartbeat");
    // Every frame of one kind is strictly ordered per namespace.
    let mut seqs = Vec::new();
    publisher.tick(start + Duration::from_millis(6300));
    for sent in recorder.take() {
        if let Sent::Presence(_, frame) = sent {
            seqs.push(frame.seq);
        }
    }
    assert!(presence[0].seq < seqs[0]);

    // Ended: final activity and a presence frame without focus or selection.
    publisher.handle(
        AgentLiveEvent::Ended {
            agent_id: "agent-1".into(),
            state: "done".into(),
        },
        start + Duration::from_secs(7),
    );
    let sent = recorder.take();
    assert_eq!(activity(&sent)[0].state, "done");
    assert_eq!(activity(&sent)[0].status.as_deref(), Some("waiting"));
    let last = presences(&sent).pop().unwrap();
    assert!(last.focus.is_none() && last.selection.is_none());
    // Finished but alive: stays announced as `waiting` with a slow heartbeat
    // (no presence heartbeats), like the owner's agent reads.
    publisher.tick(start + Duration::from_secs(10));
    assert!(recorder.take().is_empty());
    publisher.tick(start + Duration::from_secs(13));
    let sent = recorder.take();
    assert_eq!(activity(&sent)[0].status.as_deref(), Some("waiting"));
    assert!(presences(&sent).is_empty());
    // Process exit: `completed`, then forgotten.
    publisher.handle(
        AgentLiveEvent::Exited {
            agent_id: "agent-1".into(),
            failed: false,
        },
        start + Duration::from_secs(14),
    );
    assert_eq!(
        activity(&recorder.take())[0].status.as_deref(),
        Some("completed")
    );
    publisher.tick(start + Duration::from_secs(20));
    assert!(publisher.agents.is_empty());
}

#[test]
fn commits_forward_the_exact_signed_change_and_clear_the_pending_preview() {
    let (dir, mut store, _device) = crate::agent_owner_tests::enrolled();
    store
        .activate_delivery_storage(qos_relay::clock::now_ns())
        .unwrap();
    drop(store);
    let canonical =
        crate::delivery::admission::tests::browser_delta(dir.path(), "agent-admission", "hi");
    let recorder = Recorder::default();
    let mut publisher = Publisher::new(recorder.clone(), ROOM, HOST);
    let now = Instant::now();
    publisher.handle(
        preview_event(
            "hi there",
            Some(Caret {
                block: "b1".into(),
                offset: 0,
            }),
        ),
        now,
    );
    recorder.take();
    let caret = Caret {
        block: "b1".into(),
        offset: 2,
    };
    publisher.handle(
        AgentLiveEvent::Committed {
            agent_id: "agent-1".into(),
            name: "Scribe".into(),
            app: WRITER_APP.into(),
            record: RECORD.into(),
            canonical: canonical.clone(),
            caret: Some(caret),
        },
        now,
    );
    let sent = recorder.take();
    let change = sent
        .iter()
        .find_map(|sent| match sent {
            Sent::Ops(
                _,
                _,
                OpsFrame::SignedChange {
                    canonical,
                    digest,
                    record,
                },
            ) => Some((canonical.clone(), *digest, record.clone())),
            _ => None,
        })
        .expect("signed change forwarded");
    assert_eq!(change.0, canonical);
    assert_eq!(change.1, *blake3::hash(&canonical).as_bytes());
    assert_eq!(change.2.record, RECORD);
    let (anchor, head) = presences(&sent)[0].selection.clone().unwrap();
    assert_eq!(
        decode_selection(&anchor.cursor, &head.cursor),
        Some((writer_key("b1"), 2, 2))
    );
    // Pending cleared: no re-send, and the next preview starts fresh.
    publisher.tick(now + Duration::from_millis(500));
    assert!(previews(&recorder.take()).is_empty());
    publisher.handle(
        preview_event(" there", None),
        now + Duration::from_millis(600),
    );
    assert!(previews(&recorder.take()).is_empty());
    // Revealed from the start at the typing cadence (30 chars/s: 6 in 400 ms).
    publisher.tick(now + Duration::from_millis(1000));
    let ops = previews(&recorder.take());
    assert_eq!(ops[0].len(), 1);
    assert_eq!(inserted(&ops[0]), [" there"]);
}

// ---- Real relay with an HTTPS fallback subscriber ---------------------------

struct Viewer {
    room: String,
}
impl LiveSession for Viewer {
    fn room(&self) -> &str {
        &self.room
    }
    fn publisher(&self) -> &str {
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    }
    fn publish(&self) -> bool {
        false
    }
    fn subscribe(&self) -> bool {
        true
    }
    fn status(&self) -> AuthorizationStatus {
        AuthorizationStatus::Current
    }
}
/// Test authority with the production frame validation (canonical codecs).
struct TestAuthority;
impl LiveAuthority for TestAuthority {
    fn try_admit(&self, token: &[u8]) -> Result<Arc<dyn LiveSession>, Admission> {
        match token {
            b"viewer" => Ok(Arc::new(Viewer {
                room: hex::encode(room_digest(ROOM)),
            })),
            _ => Err(Admission::Denied),
        }
    }
    fn presence_seq(&self, frame: &[u8]) -> Option<u64> {
        PresenceFrame::decode(frame).ok().map(|frame| frame.seq)
    }
    fn ops_valid(&self, record: &str, frame: &[u8]) -> bool {
        OpsFrame::decode(frame).is_ok_and(|frame| match &frame {
            OpsFrame::Preview { record: named, .. }
            | OpsFrame::SignedChange { record: named, .. } => hex::encode(named.digest()) == record,
        })
    }
}

struct Stream {
    inner: fallback::FallbackStream,
    pending: Vec<u8>,
}
impl Stream {
    async fn frame(&mut self) -> (u8, Vec<u8>) {
        loop {
            if self.pending.len() >= 5 {
                let len = u32::from_be_bytes(self.pending[1..5].try_into().unwrap()) as usize;
                if self.pending.len() >= 5 + len {
                    let kind = self.pending[0];
                    let payload = self.pending[5..5 + len].to_vec();
                    self.pending.drain(..5 + len);
                    return (kind, payload);
                }
            }
            let chunk = self.inner.next().await.expect("stream open");
            self.pending.extend_from_slice(&chunk);
        }
    }
    async fn until(&mut self, kind: u8) -> Vec<u8> {
        loop {
            let (next, payload) = self.frame().await;
            if next == kind {
                return payload;
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn publisher_task_reaches_a_fallback_subscriber_through_the_real_relay() {
    let relay = LiveRelay::new(Arc::new(TestAuthority));
    // Own channel: the process-global bus may be replaced by a concurrently
    // running serve test (the bus itself is covered in `agent_live::tests`).
    let (events, receiver) = tokio::sync::mpsc::channel(crate::agent_live::BUS_CAPACITY);
    let running = AgentLivePublisher::spawn(relay.clone(), ROOM.into(), HOST, receiver);
    let agent = qos_relay::delivery_v3::live_wire::agent_publisher(ROOM, HOST, "agent-9");
    let mut stream = Stream {
        inner: relay
            .fallback_open(b"viewer", [3; 16])
            .await
            .expect("viewer admitted"),
        pending: Vec::new(),
    };
    assert_eq!(stream.frame().await.0, fallback::FRAME_HELLO);
    let deadline = Duration::from_secs(10);
    let activity = |state: &str| AgentLiveEvent::Activity {
        agent_id: "agent-9".into(),
        name: "Scribe".into(),
        state: state.into(),
        label: "Writing in Writer".into(),
        tool: Some("append-paragraph".into()),
    };
    events.try_send(activity("writing")).unwrap();
    // The agent namespace is announced as a member, then its presence arrives.
    tokio::time::timeout(deadline, async {
        loop {
            let member = stream.until(fallback::FRAME_MEMBER).await;
            if member[..32] == agent && member[32] == 1 {
                break;
            }
        }
    })
    .await
    .expect("agent member announced");
    let record = agent_activity_record().digest();
    relay
        .fallback_send(
            &[3; 16],
            &fallback::encode_frame(fallback::FRAME_SUBSCRIBE_OPS, &[&agent, &record]),
        )
        .unwrap();
    // Heartbeats keep both flowing: a presence datagram and an activity frame.
    let (presence, value) = tokio::time::timeout(deadline, async {
        let (mut presence, mut value) = (None, None);
        while presence.is_none() || value.is_none() {
            let (kind, payload) = stream.frame().await;
            if kind == fallback::FRAME_PRESENCE && payload[..32] == agent {
                presence = Some(PresenceFrame::decode(&payload[32..]).unwrap());
            }
            if kind == fallback::FRAME_OPS && payload[..32] == agent && payload[32..64] == record {
                let frame = OpsFrame::decode(&payload[64..]).unwrap();
                value = Some(AgentActivity::from_frame(&frame).unwrap().1);
            }
        }
        (presence.unwrap(), value.unwrap())
    })
    .await
    .expect("presence and activity delivered");
    assert!(presence.focus.is_none());
    assert_eq!(value.agent, "agent-9");
    assert_eq!(value.name, "Scribe");
    assert_eq!(value.tool.as_deref(), Some("append-paragraph"));
    assert!(relay.counters().injected >= 2, "{:?}", relay.counters());
    running.abort();
}

#[test]
fn participant_view_and_commit_carets_are_latest_wins_and_carry_selections() {
    let recorder = Recorder::default();
    let mut publisher = Publisher::new(recorder.clone(), ROOM, HOST);
    let now = Instant::now();
    publisher.handle(
        AgentLiveEvent::Presence {
            agent_id: "agent-1".into(),
            name: "Scribe".into(),
            app: WRITER_APP.into(),
            record: RECORD.into(),
            caret: Some(Caret {
                block: "b1".into(),
                offset: 3,
            }),
            head: Some(9),
        },
        now,
    );
    let frame = presences(&recorder.take()).pop().expect("agent presence");
    let (anchor, head) = frame.selection.unwrap();
    assert_eq!(
        decode_selection(&anchor.cursor, &head.cursor),
        Some((writer_key("b1"), 3, 9))
    );
    // A commit no longer moves the caret the participant view owns.
    let (dir, mut store, _device) = crate::agent_owner_tests::enrolled();
    store
        .activate_delivery_storage(qos_relay::clock::now_ns())
        .unwrap();
    drop(store);
    let canonical =
        crate::delivery::admission::tests::browser_delta(dir.path(), "agent-admission", "hi");
    publisher.handle(
        AgentLiveEvent::Committed {
            agent_id: "agent-1".into(),
            name: "Scribe".into(),
            app: WRITER_APP.into(),
            record: RECORD.into(),
            canonical,
            caret: Some(Caret {
                block: "b7".into(),
                offset: 1,
            }),
        },
        now,
    );
    let sent = recorder.take();
    // Latest wins: the later commit moves the caret (to b7:1).
    let frame = presences(&sent).pop().expect("commit caret presence");
    let (anchor, head) = frame.selection.unwrap();
    assert_eq!(
        decode_selection(&anchor.cursor, &head.cursor),
        Some((writer_key("b7"), 1, 1))
    );
    assert!(
        sent.iter()
            .any(|s| matches!(s, Sent::Ops(_, _, OpsFrame::SignedChange { .. })))
    );
    // Cleared by the participant view.
    publisher.handle(
        AgentLiveEvent::Presence {
            agent_id: "agent-1".into(),
            name: "Scribe".into(),
            app: WRITER_APP.into(),
            record: RECORD.into(),
            caret: None,
            head: None,
        },
        now,
    );
    let frame = presences(&recorder.take()).pop().unwrap();
    assert!(frame.selection.is_none());
}

/// A commit without a caret (a Chat post) announces the agent's focus on that
/// record before its signed change: browsers subscribe to a member's ops only
/// for records the member (or they) focus, so without it the forwarded change
/// reaches no one who is not already focused on Chat.
#[test]
fn caretless_commits_announce_focus_on_their_record_before_the_change() {
    let (dir, mut store, _device) = crate::agent_owner_tests::enrolled();
    store
        .activate_delivery_storage(qos_relay::clock::now_ns())
        .unwrap();
    drop(store);
    let canonical =
        crate::delivery::admission::tests::browser_delta(dir.path(), "agent-admission", "hi");
    let recorder = Recorder::default();
    let mut publisher = Publisher::new(recorder.clone(), ROOM, HOST);
    let now = Instant::now();
    let chat = |canonical: Vec<u8>| AgentLiveEvent::Committed {
        agent_id: "agent-1".into(),
        name: "Scribe".into(),
        app: "so.tilo.chat".into(),
        record: String::new(),
        canonical,
        caret: None,
    };
    publisher.handle(chat(canonical.clone()), now);
    let sent = recorder.take();
    let presence = sent
        .iter()
        .position(|s| matches!(s, Sent::Presence(..)))
        .expect("focus announced");
    let change = sent
        .iter()
        .position(|s| matches!(s, Sent::Ops(_, _, OpsFrame::SignedChange { .. })))
        .expect("signed change forwarded");
    assert!(presence < change, "presence precedes the forwarded change");
    let frame = presences(&sent).pop().unwrap();
    let focus = frame.focus.expect("chat focus");
    assert_eq!(focus.app, "so.tilo.chat");
    assert_eq!(focus.record, super::super::live::PACKAGE_PRIMARY_RECORD);
    assert!(frame.selection.is_none(), "no Writer caret in Chat");
    // Further commits on the same record forward without re-announcing.
    publisher.handle(chat(canonical), now);
    let sent = recorder.take();
    assert!(presences(&sent).is_empty());
    assert!(
        sent.iter()
            .any(|s| matches!(s, Sent::Ops(_, _, OpsFrame::SignedChange { .. })))
    );
}

#[test]
fn long_block_ids_publish_a_digest_anchor_the_browser_resolves() {
    let block = format!("{}.3", "cd".repeat(64));
    let caret = Caret {
        block: block.clone(),
        offset: 12,
    };
    let (anchor, head) = range_selection(&caret, 20).expect("digest form fits");
    assert_eq!(anchor.cursor[0], 3);
    assert!(anchor.cursor.len() <= 64);
    let key = writer_key(&block);
    assert_eq!(&anchor.cursor[2..], &key.scope_digest().unwrap());
    assert_eq!(head.cursor, vec![2, 20]);
    // Shared vector with ui-core and the browser live_view tests.
    assert_eq!(
        hex::encode(writer_key("b1").scope_digest().unwrap()),
        "9c59c4ded04a69ccf5b2ac58797c3b77"
    );
    // Short ids keep the exact scoped form.
    let (short, _) = caret_selection(&Caret {
        block: "b1".into(),
        offset: 5,
    })
    .unwrap();
    assert_eq!(short.cursor[0], 1);
}
