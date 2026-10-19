//! Timed keystroke write cost through the real relay and co-edit client.
//!
//! Preserves the original network scenario: 21 keystrokes, the first as
//! warm-up, a 30 ms pause after each, and a median of exactly one write.
//! This scheduling-sensitive assertion belongs to Perf / scheduled's hosting
//! lane, not the normal test gate. The production-writer tests separately
//! enforce exact counts for both valid ingress schedules.
//!
//! Run: `cargo run --locked -p tilo-relay --example keystroke_writes`

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "this assertion-based benchmark fails immediately on broken fixtures or budgets"
)]
#![allow(
    clippy::significant_drop_tightening,
    reason = "relay, endpoint and fixture guards must live through the whole benchmark"
)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::arithmetic_side_effects,
    reason = "positions and counts in this bounded 21-keystroke fixture fit the conversions and arithmetic"
)]

#[path = "../tests/common/mod.rs"]
mod common;

use std::time::{Duration, SystemTime};

use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, transaction::Transactable};
use collab_types::{CommitNamed as _, PeerPresence, PeerPresencePayload};
use neighbour_wire::NeighbourMsg;
use tilo_delivery_protocol::transport_v2::{EnvelopeV2, SessionReader, encode_state};
use tilo_peer_state::{SeedDocument, SeedState, StatePeer, seed_room};
use tilo_relay::{AttributedPresence, Envelope, Inbound, RelayClient, RelayCoeditSession};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestDevice, TestRelay, TestUser, client_endpoint, spawn_relay};

const WORKSPACE: u64 = 710;
const ROOM: &str = "5e11f00d-7110-4625-8000-000000000710";

struct Member {
    _endpoint: iroh::Endpoint,
    _connection: iroh::endpoint::Connection,
    _send: iroh::endpoint::SendStream,
    reader: SessionReader<iroh::endpoint::RecvStream>,
}

impl Member {
    async fn open(relay: &TestRelay, device: &TestDevice) -> Self {
        let endpoint = client_endpoint().await;
        let connection = endpoint
            .connect(relay.relay.node_addr(), tilo_relay::ALPN)
            .await
            .unwrap();
        let (mut send, mut recv) = connection.open_bi().await.unwrap();
        let hello = tilo_relay::auth::Hello::new(device.cert.clone(), [7; 32]);
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
            &encode_state(&Envelope::Subscribe {
                workspace_id: WORKSPACE,
            })
            .unwrap(),
        )
        .await
        .unwrap();
        let mut member = Self {
            _endpoint: endpoint,
            _connection: connection,
            _send: send,
            reader: SessionReader::new(recv),
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

fn main() -> std::process::ExitCode {
    if let Some(code) = tilo_automerge_isolate::dispatch_early() {
        return code.into();
    }
    run();
    std::process::ExitCode::SUCCESS
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn run() {
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

    let mut member = Member::open(&relay, &reader).await;
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
        let (channel, _) = neighbour_wire::retention(&message, false).unwrap();
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
