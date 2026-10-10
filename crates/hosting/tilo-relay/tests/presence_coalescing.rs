//! What presence costs the host, and that coalescing it keeps what a
//! receiver draws.
//!
//! A collaborator drags a pointer at 100 Hz through the real relay, then
//! moves a caret and clears the pointer, publishing as the shell does
//! (`RelayCoeditSession::publish_presence` on a client that dials
//! `tilo-relay/6`; the relay reads each message's `presence_slot`). Two
//! members read what the relay writes them: one on `tilo-relay/5`, which
//! predates batching, and one on
//! `tilo-relay/6`. Both must see the pointer paced to at most 20 Hz, its
//! last position, the caret, and the event after everything sent before it;
//! the `/6` member must pay a fraction of the bytes.
//!
//! It prints one `PRESENCE_COST key=value …` line. It gates only what does not
//! depend on timing: the final state, the order, a generous frame ceiling and
//! the bytes per presence.
//!
//! A second scenario types: each keystroke is a retained delta followed by
//! the toolkit's `Typing` and `Caret`, as the shell sends them. The caret
//! and typing are held for the delta they follow and ride in its write, so a `tilo-relay/7` member receives a keystroke in one write
//! where it took two or three. It prints one `KEYSTROKE_WRITES key=value …`
//! line and gates the median keystroke.
//!
//! A third drags a pointer at 100 Hz from the production client: the
//! session writer paces it to the rate the relay forwards it, so the relay
//! receives about one datagram per 50 ms rather than one per move, and the
//! resting position still reaches a member with nothing sent after it. It
//! prints one `POINTER_PACING key=value …` line.

#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::arithmetic_side_effects,
    reason = "byte and frame counts of a bounded 60-move run fit every conversion and sum"
)]

use std::time::{Duration, SystemTime};

use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, transaction::Transactable};
use collab_types::{CommitNamed as _, PeerPresence, PeerPresencePayload};
use tilo_delivery_protocol::transport_v2::{
    ALPN_CODED_DELTA_V1, ALPN_COMPACT_STATE_V1, ALPN_PRESENCE_BATCH_V1, EnvelopeV2, SessionReader,
    StateFraming, encode_state,
};
use tilo_peer_state::{NeighbourMsg, SeedDocument, SeedState, StatePeer, seed_room};
use tilo_relay::{AttributedPresence, Envelope, Inbound, RelayClient, RelayCoeditSession};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestDevice, TestRelay, TestUser, client_endpoint, spawn_relay};

const WORKSPACE: u64 = 710;
const ROOM: &str = "5e11f00d-7110-4625-8000-000000000710";
const MOVES: u32 = 60;
/// What each presence frame cost on `tilo-relay/5` before the change, ~236 B
/// for a pointer move; on `tilo-relay/6` it must stay well below a third.
const BUDGET_SLOT_CODED_BYTES_PER_PRESENCE: f64 = 70.0;

/// A member reading the relay's frames on one ALPN through the production
/// session reader, which expands `PresenceBatch` frames.
struct Member {
    _endpoint: iroh::Endpoint,
    _connection: iroh::endpoint::Connection,
    _send: iroh::endpoint::SendStream,
    reader: SessionReader<iroh::endpoint::RecvStream>,
}

impl Member {
    async fn open(
        relay: &TestRelay,
        device: &TestDevice,
        alpn: &[u8],
        framing: StateFraming,
    ) -> Self {
        let endpoint = client_endpoint().await;
        let connection = endpoint
            .connect(relay.relay.node_addr(), alpn)
            .await
            .unwrap();
        let (mut send, mut recv) = connection.open_bi().await.unwrap();
        let hello = tilo_relay::auth::Hello {
            cert: device.cert.clone(),
            client_nonce: [7; 32],
        };
        tilo_relay::frame::write_frame(&mut send, &postcard::to_allocvec(&hello).unwrap())
            .await
            .unwrap();
        let ack: tilo_relay::auth::HelloAck = postcard::from_bytes(
            &tilo_relay::frame::read_frame(&mut recv, tilo_relay::transport::MAX_FRAME_BYTES)
                .await
                .unwrap(),
        )
        .unwrap();
        let ack2 = tilo_relay::auth::build_hello_ack2(&device.sk, &ack.server_nonce);
        tilo_relay::frame::write_frame(&mut send, &postcard::to_allocvec(&ack2).unwrap())
            .await
            .unwrap();
        send.write_all(
            &encode_state(
                &Envelope::Subscribe {
                    workspace_id: WORKSPACE,
                },
                true,
            )
            .unwrap(),
        )
        .await
        .unwrap();
        let mut member = Self {
            _endpoint: endpoint,
            _connection: connection,
            _send: send,
            reader: SessionReader::framed(recv, framing),
        };
        tilo_test_env::within("the subscription to be ready", async {
            while !matches!(
                member.reader.read_next().await.unwrap(),
                Some(EnvelopeV2::State(Envelope::Ready { .. }))
            ) {}
        })
        .await;
        member
    }

    /// Every presence message up to and including the pointer being cleared,
    /// with its author.
    async fn presence_until_cleared(&mut self) -> Vec<([u8; 32], NeighbourMsg)> {
        tilo_test_env::within("the presence burst to arrive", async {
            let mut seen = Vec::new();
            loop {
                let Some(EnvelopeV2::State(Envelope::Presence { payload, .. })) =
                    self.reader.read_next().await.unwrap()
                else {
                    continue;
                };
                let attributed = AttributedPresence::decode(&payload).unwrap();
                let message: NeighbourMsg = postcard::from_bytes(&attributed.body).unwrap();
                let cleared = matches!(message, NeighbourMsg::PresenceCleared { .. });
                seen.push((attributed.author, message));
                if cleared {
                    return seen;
                }
            }
        })
        .await
    }
}

fn session_writes(relay: &TestRelay, device: [u8; 32]) -> u64 {
    relay
        .relay
        .handle()
        .metrics
        .session_egress()
        .into_iter()
        .filter(|session| session.device_public_key == device)
        .map(|session| session.writes)
        .sum()
}

fn session_bytes(relay: &TestRelay, device: [u8; 32]) -> u64 {
    relay
        .relay
        .handle()
        .metrics
        .session_egress()
        .into_iter()
        .filter(|session| session.device_public_key == device)
        .map(|session| session.bytes_sent)
        .sum()
}

fn pointer(step: u32, user: &str) -> NeighbourMsg {
    NeighbourMsg::PresenceMoved {
        peer: 0x9e37_79b9_7f4a_7c15,
        user: user.into(),
        key: format!(r#"{{"pointer":{{"space":{{"tile":{{"room":"{ROOM}","tab":1}}}}}}}}"#),
        point: (f64::from(step).mul_add(3.0, 200.0), 300.0),
        ts: 1_800_000_000_000 + u64::from(step) * 10,
        colour: Some("teal".into()),
    }
}

fn caret(pos: u64) -> NeighbourMsg {
    NeighbourMsg::AppPresenceV2 {
        peer: 0x9e37_79b9_7f4a_7c15,
        app_id: "so.tilo.word-processor".into(),
        cluster_id: ROOM.into(),
        presence: PeerPresence {
            key: br#"{"widget":{"path":[{"field":"body"}]}}"#.to_vec(),
            payload: PeerPresencePayload::Caret { pos },
        },
        colour: Some("teal".into()),
    }
}

fn typing(start: u64, end: u64) -> NeighbourMsg {
    NeighbourMsg::AppPresenceV2 {
        peer: 0x9e37_79b9_7f4a_7c15,
        app_id: "so.tilo.word-processor".into(),
        cluster_id: ROOM.into(),
        presence: PeerPresence {
            key: br#"{"widget":{"path":[{"field":"body"}]}}"#.to_vec(),
            payload: PeerPresencePayload::Typing { start, end },
        },
        colour: Some("teal".into()),
    }
}

fn publish(session: &RelayCoeditSession, message: &NeighbourMsg) {
    session
        .publish_presence(postcard::to_allocvec(message).unwrap())
        .unwrap();
}

/// What a member saw of the burst, checked against what a receiver must draw.
fn check(seen: &[([u8; 32], NeighbourMsg)], author: [u8; 32], user: &str, protocol: &str) -> usize {
    assert!(
        seen.iter().all(|(from, _)| *from == author),
        "{protocol}: the relay stamps the authenticated author"
    );
    let moves: Vec<_> = seen
        .iter()
        .filter(|(_, message)| matches!(message, NeighbourMsg::PresenceMoved { .. }))
        .collect();
    assert!(
        !moves.is_empty() && moves.len() < 30,
        "{protocol}: {} pointer moves of {MOVES} published at 100 Hz for 600 ms; \
         the relay paces a pointer to 20 Hz",
        moves.len()
    );
    assert_eq!(
        moves.last().map(|(_, message)| message),
        Some(&pointer(MOVES - 1, user)),
        "{protocol}: the pointer's last position arrives"
    );
    assert_eq!(
        seen.iter()
            .rev()
            .find(|(_, message)| matches!(message, NeighbourMsg::AppPresenceV2 { .. }))
            .map(|(_, message)| message),
        Some(&caret(42)),
        "{protocol}: the caret's last position arrives before the event sent after it"
    );
    // `seen` ends at the event: the last pointer position and caret above
    // were delivered before it, as they were sent. Slots are independent
    // states and may arrive in any order among themselves.
    seen.len()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn presence_is_coalesced_for_every_protocol_and_slot_coded_on_tilo_relay_6() {
    let user = TestUser::fresh("Presence cost");
    let author = user.enrol("laptop", DeviceCapabilities::primary());
    let older = user.enrol("tilo-relay/5 peer", DeviceCapabilities::member());
    let current = user.enrol("tilo-relay/6 peer", DeviceCapabilities::member());
    let members = [
        author.device_pubkey(),
        older.device_pubkey(),
        current.device_pubkey(),
    ];
    let relay = Box::pin(spawn_relay(
        &user,
        user.enrol("relay", DeviceCapabilities::host()),
        &[WORKSPACE],
        &members,
    ))
    .await;
    let mut old_member =
        Member::open(&relay, &older, ALPN_COMPACT_STATE_V1, StateFraming::Compact).await;
    let mut new_member = Member::open(
        &relay,
        &current,
        ALPN_PRESENCE_BATCH_V1,
        StateFraming::Presence,
    )
    .await;
    // The publisher dials the newest protocol, as the shell does.
    let publisher = RelayCoeditSession::join(
        RelayClient::connect_v2_to_enrolled_home_peer(
            client_endpoint().await,
            relay.relay.node_addr(),
            user.user_pk,
            relay.relay.handle().mesh_profile().device,
            author.cert.clone(),
            author.sk.clone(),
            RevocationList::new(),
        )
        .await
        .unwrap(),
        WORKSPACE,
        author.device_pubkey(),
    )
    .unwrap();
    tilo_test_env::within("the publisher's subscription", async {
        while !matches!(publisher.recv().await, Some(Inbound::Ready { .. })) {}
    })
    .await;

    let user_hex = hex::encode(user.user_pk);
    let old_before = session_bytes(&relay, older.device_pubkey());
    let new_before = session_bytes(&relay, current.device_pubkey());
    for step in 0..MOVES {
        publish(&publisher, &pointer(step, &user_hex));
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    for pos in 30..=42 {
        publish(&publisher, &caret(pos));
    }
    publish(
        &publisher,
        &NeighbourMsg::PresenceCleared {
            peer: 0x9e37_79b9_7f4a_7c15,
            key: "tile".into(),
        },
    );
    let old_seen = old_member.presence_until_cleared().await;
    let new_seen = new_member.presence_until_cleared().await;
    let old_count = check(&old_seen, author.device_pubkey(), &user_hex, "tilo-relay/5");
    let new_count = check(&new_seen, author.device_pubkey(), &user_hex, "tilo-relay/6");

    // Only presence reached either member since the counters were read.
    let old_bytes = session_bytes(&relay, older.device_pubkey()) - old_before;
    let new_bytes = session_bytes(&relay, current.device_pubkey()) - new_before;
    let old_per = old_bytes as f64 / old_count as f64;
    let new_per = new_bytes as f64 / new_count as f64;
    println!(
        "PRESENCE_COST moves_published={MOVES} relay5_presence={old_count} relay5_bytes={old_bytes} \
         relay5_bytes_per_presence={old_per:.1} relay6_presence={new_count} \
         relay6_bytes={new_bytes} relay6_bytes_per_presence={new_per:.1}"
    );
    assert!(
        new_per <= BUDGET_SLOT_CODED_BYTES_PER_PRESENCE,
        "tilo-relay/6 paid {new_per:.1} B per presence (budget \
         {BUDGET_SLOT_CODED_BYTES_PER_PRESENCE} B; /5 paid {old_per:.1} B)"
    );
    assert!(
        old_per > 3.0 * new_per,
        "slot coding must cut the bytes per presence to under a third: \
         /5 {old_per:.1} B, /6 {new_per:.1} B"
    );
}

const APP: &str = "so.tilo.word-processor";
const RECORD: &str = "tab:0000000000000001";
const KEYSTROKES: u64 = 21;

impl Member {
    /// Read up to and including the caret at `pos`: whether a delta came
    /// before it.
    async fn keystroke(&mut self, pos: u64) -> bool {
        tilo_test_env::within("the keystroke's delta and caret", async {
            let mut delta = false;
            loop {
                match self.reader.read_next().await.unwrap() {
                    Some(EnvelopeV2::State(Envelope::Delta { .. })) => delta = true,
                    Some(EnvelopeV2::State(Envelope::Presence { payload, .. })) => {
                        let attributed = AttributedPresence::decode(&payload).unwrap();
                        let message: NeighbourMsg = postcard::from_bytes(&attributed.body).unwrap();
                        if message == caret(pos) {
                            return delta;
                        }
                    }
                    _ => {}
                }
            }
        })
        .await
    }
}

/// The author's room state with one text document, and the author's
/// replica of that document.
fn seeded_room(author: &TestDevice, reader: &TestDevice) -> (StatePeer, AutoCommit) {
    let now = SystemTime::now();
    let certificates = vec![author.cert.clone(), reader.cert.clone()];
    let mut owner = StatePeer::new(
        ROOM,
        author.cert.clone(),
        certificates.clone(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    let mut seed = AutoCommit::new().with_actor(automerge::ActorId::from(vec![1; 16]));
    let text = seed.put_object(ROOT, "body", ObjType::Text).unwrap();
    seed.splice_text(&text, 0, 0, "Collaboration lives below applications.")
        .unwrap();
    seed.commit();
    owner
        .install_seed(
            SeedState {
                room_changes: seed_room(ROOM, &author.cert, &certificates).unwrap(),
                documents: vec![SeedDocument {
                    app_id: APP.into(),
                    record_id: RECORD.into(),
                    kind: "text".into(),
                    bytes: seed.save(),
                }],
            },
            &author.sk,
            now,
        )
        .unwrap();
    let mut doc = AutoCommit::load(&owner.document(APP, RECORD).unwrap()).unwrap();
    doc.set_actor(automerge::ActorId::from(author.device_pubkey().to_vec()));
    (owner, doc)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_keystrokes_caret_and_typing_ride_in_its_deltas_write() {
    let user = TestUser::fresh("Keystroke writes");
    let author = user.enrol("laptop", DeviceCapabilities::primary());
    let reader = user.enrol("tilo-relay/7 peer", DeviceCapabilities::member());
    let members = [author.device_pubkey(), reader.device_pubkey()];
    let relay = Box::pin(spawn_relay(
        &user,
        user.enrol("relay", DeviceCapabilities::host()),
        &[WORKSPACE],
        &members,
    ))
    .await;

    let (mut owner, mut doc) = seeded_room(&author, &reader);
    let body = doc.get(ROOT, "body").unwrap().unwrap().1;

    let mut member = Member::open(
        &relay,
        &reader,
        ALPN_CODED_DELTA_V1,
        StateFraming::CodedDelta,
    )
    .await;
    let publisher = RelayCoeditSession::join(
        RelayClient::connect_v2_to_enrolled_home_peer(
            client_endpoint().await,
            relay.relay.node_addr(),
            user.user_pk,
            relay.relay.handle().mesh_profile().device,
            author.cert.clone(),
            author.sk.clone(),
            RevocationList::new(),
        )
        .await
        .unwrap(),
        WORKSPACE,
        author.device_pubkey(),
    )
    .unwrap();
    tilo_test_env::within("the publisher's subscription", async {
        while !matches!(publisher.recv().await, Some(Inbound::Ready { .. })) {}
    })
    .await;

    let mut writes = Vec::new();
    let mut delta_first = 0usize;
    for keystroke in 0..KEYSTROKES {
        // One keystroke as the toolkit sends it: the edit, then where the
        // author is typing and their caret.
        let heads = doc.get_heads();
        let end = doc.length(&body);
        doc.splice_text(&body, end, 0, "a").unwrap();
        doc.commit_edit("fixture", "body");
        let message = owner
            .publish_incremental(
                APP,
                RECORD,
                doc.save_after(&heads),
                &author.sk,
                SystemTime::now(),
            )
            .unwrap();
        let (channel, _) = tilo_peer_state::transport::retention(&message, false).unwrap();
        let pos = end as u64 + 1;
        let before = session_writes(&relay, reader.device_pubkey());
        publisher
            .publish_retained_delta(channel, postcard::to_allocvec(&message).unwrap())
            .unwrap();
        publish(&publisher, &typing(end as u64, pos));
        publish(&publisher, &caret(pos));
        let delta = member.keystroke(pos).await;
        // The first keystroke warms the session's coding up.
        if keystroke > 0 {
            writes.push(session_writes(&relay, reader.device_pubkey()) - before);
            delta_first += usize::from(delta);
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    writes.sort_unstable();
    let median = writes[writes.len() / 2];
    println!(
        "KEYSTROKE_WRITES keystrokes={} median_writes={median} max_writes={} delta_before_caret={delta_first}",
        writes.len(),
        writes.last().copied().unwrap_or_default(),
    );
    assert_eq!(
        median, 1,
        "a keystroke's delta, position, typing and caret go to a member in one write: {writes:?}"
    );
}

impl Member {
    /// Every pointer move until `last` arrives, with nothing sent after it.
    async fn pointer_until(&mut self, last: &NeighbourMsg) -> Vec<NeighbourMsg> {
        tilo_test_env::within("the pointer's resting position", async {
            let mut moves = Vec::new();
            loop {
                let Some(EnvelopeV2::State(Envelope::Presence { payload, .. })) =
                    self.reader.read_next().await.unwrap()
                else {
                    continue;
                };
                let attributed = AttributedPresence::decode(&payload).unwrap();
                let message: NeighbourMsg = postcard::from_bytes(&attributed.body).unwrap();
                if matches!(message, NeighbourMsg::PresenceMoved { .. }) {
                    let rested = message == *last;
                    moves.push(message);
                    if rested {
                        return moves;
                    }
                }
            }
        })
        .await
    }
}

/// QUIC datagrams the relay's endpoint has received so far.
fn relay_datagrams_received(relay: &TestRelay) -> u64 {
    relay.relay.endpoint.metrics().socket.recv_datagrams.get()
}

/// A client paces its own pointer to the rate the relay forwards it: a drag
/// published at 100 Hz reaches the relay in about one packet per 50 ms, and
/// its resting position still arrives with nothing sent after it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_sends_its_pointer_at_the_rate_the_relay_forwards_it() {
    let user = TestUser::fresh("Pointer pacing");
    let author = user.enrol("laptop", DeviceCapabilities::primary());
    let reader = user.enrol("tilo-relay/7 peer", DeviceCapabilities::member());
    let members = [author.device_pubkey(), reader.device_pubkey()];
    let relay = Box::pin(spawn_relay(
        &user,
        user.enrol("relay", DeviceCapabilities::host()),
        &[WORKSPACE],
        &members,
    ))
    .await;
    let publisher = RelayCoeditSession::join(
        RelayClient::connect_v2_to_enrolled_home_peer(
            client_endpoint().await,
            relay.relay.node_addr(),
            user.user_pk,
            relay.relay.handle().mesh_profile().device,
            author.cert.clone(),
            author.sk.clone(),
            RevocationList::new(),
        )
        .await
        .unwrap(),
        WORKSPACE,
        author.device_pubkey(),
    )
    .unwrap();
    tilo_test_env::within("the publisher's subscription", async {
        while !matches!(publisher.recv().await, Some(Inbound::Ready { .. })) {}
    })
    .await;
    let user_hex = hex::encode(user.user_pk);

    // A fresh connection still settles its path (MTU probes, address
    // observations, their acknowledgements); let it, then measure what the
    // relay receives from the publisher while it is quiet, and while it
    // drags. Alone in the room, every datagram the relay receives is the
    // publisher's: unpaced, the drag alone would add one per move.
    for step in 0..MOVES {
        publish(&publisher, &pointer(step, &user_hex));
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    let window = Duration::from_millis(10) * MOVES + Duration::from_millis(200);
    let quiet_from = relay_datagrams_received(&relay);
    tokio::time::sleep(window).await;
    let quiet = relay_datagrams_received(&relay) - quiet_from;
    let drag_from = relay_datagrams_received(&relay);
    for step in 0..MOVES {
        publish(&publisher, &pointer(step, &user_hex));
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    let drag = relay_datagrams_received(&relay) - drag_from;
    println!(
        "POINTER_PACING moves={MOVES} window_ms={} relay_datagrams_quiet={quiet} \
         relay_datagrams_drag={drag}",
        window.as_millis()
    );
    assert!(
        drag.saturating_sub(quiet) <= u64::from(MOVES) / 3,
        "{MOVES} moves published over 600 ms reached the relay in {drag} datagrams \
         ({quiet} in as long a quiet window); paced to one per 50 ms they take about 13"
    );

    // With a member in the room, the last move arrives on its own: it is
    // written when its gap ends, not when something else is sent.
    let mut member = Member::open(
        &relay,
        &reader,
        ALPN_CODED_DELTA_V1,
        StateFraming::CodedDelta,
    )
    .await;
    let last = pointer(2 * MOVES - 1, &user_hex);
    for step in MOVES..2 * MOVES {
        publish(&publisher, &pointer(step, &user_hex));
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let moves = member.pointer_until(&last).await;
    assert!(
        moves.len() < 30,
        "{} pointer moves of {MOVES} published at 100 Hz for 600 ms; a pointer goes at 20 Hz",
        moves.len()
    );
}
