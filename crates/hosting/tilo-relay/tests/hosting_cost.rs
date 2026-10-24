//! What one keystroke costs the host: bytes on the wire, bytes on disk.
//!
//! One-character edits take the production path end to end: the toolkit's
//! Automerge change, `StatePeer::publish_incremental`, the host retention
//! classifier, `RelayCoeditSession::publish_retained_delta` on a real
//! `RelayClient`, and the relay's authenticated ingress, retention and
//! fan-out. A subscriber reads the frames the relay actually writes it, whose
//! deltas arrive coded against the previous one and are rebuilt by the
//! production session reader; each delta's uncoded frame (the one a coded
//! delta rebuilds) is measured beside it. Afterwards the test walks the
//! relay's state root.
//!
//! It prints one `HOSTING_COST key=value …` line for the cost model.
//! It gates only deterministic byte counts, as docs/measurements/performance.md
//! requires of a per-PR check; the delivery latency it prints is a
//! report, never an assertion.

#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]
#![allow(
    clippy::too_many_lines,
    reason = "one end-to-end scenario: publish, two subscribers and the store walk stay together"
)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::arithmetic_side_effects,
    reason = "byte counts, file counts and microseconds of a bounded 256-edit run fit every \
              conversion and sum"
)]
#![allow(
    clippy::similar_names,
    reason = "paired measurements are named after the protocol they were taken on"
)]

use collab_types::CommitNamed as _;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::{Instant, SystemTime};

use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, transaction::Transactable};
use neighbour_wire::Retention;
use tilo_delivery_protocol::transport_v2::{EnvelopeV2, SessionReader, encode_state};
use tilo_peer_state::{SeedDocument, SeedState, StatePeer, seed_room};
use tilo_relay::{Envelope, Inbound, RelayClient, RelayCoeditSession};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestDevice, TestRelay, TestUser, client_endpoint, spawn_relay};

const ROOM: &str = "5e11f00d-7110-4625-8000-000000000625";
const WORKSPACE: u64 = 625;
/// Four routes, as in a room with four open documents.
const ROUTES: [(&str, &str); 4] = [
    ("so.tilo.word-processor", "tab:0000000000000001"),
    ("so.tilo.notes", "tab:0000000000000002"),
    ("so.tilo.sheets", "tab:0000000000000003"),
    ("so.tilo.code-editor", "tab:0000000000000004"),
];
/// Eight words in each document: every keystroke is a durable relay commit,
/// whose barriers take a tenth of a second or more on a busy CI disk. What
/// is gated is the size of single frames, which more keystrokes repeat:
/// 1,000 of them took 5 to 16 minutes and measured 2 to 3 B more at the
/// medians and maxima, as their later edits' larger counters took more
/// bytes. Each document still grows to the compactor's 64-entry check.
const KEYSTROKES: usize = 256;
/// Characters in each document before the first keystroke (~10 kB).
const DOCUMENT_CHARS: usize = 10_000;

/// Ceiling for the median one-character keystroke frame uncoded:
/// 481 B measured on Linux CI before every change named its action, plus
/// the keystroke's compact continuation of its word's gesture (~60 B; a
/// cleartext op carries no second copy of it). Measured here: 542 B.
const BUDGET_COMPACT_FRAME_P50_BYTES: u64 = 550;
/// Ceiling for the largest such frame, a word's first keystroke carrying
/// the gesture's full entry in its compact form: measured 599 B, held to
/// the 697 B of the frame before changes named their actions.
const BUDGET_COMPACT_FRAME_MAX_BYTES: u64 = 697;
/// Ceiling for the median keystroke frame as the relay writes it, coded
/// against the author's previous keystroke on the same document. Measured
/// on Linux CI: 157 B against 482 B uncoded, plus 15 %.
const BUDGET_CODED_FRAME_P50_BYTES: u64 = 181;

/// A stream that counts the bytes read through it.
struct Counted<R> {
    inner: R,
    read: Arc<AtomicU64>,
}

impl<R: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for Counted<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let polled = Pin::new(&mut self.inner).poll_read(cx, buf);
        let read = buf.filled().len() - before;
        self.read.fetch_add(read as u64, Ordering::Relaxed);
        polled
    }
}

/// A subscriber reading through the production session reader, which
/// rebuilds coded deltas, and counting the bytes the relay wrote for each.
struct CodedSubscriber {
    _endpoint: iroh::Endpoint,
    _connection: iroh::endpoint::Connection,
    _send: iroh::endpoint::SendStream,
    reader: SessionReader<Counted<iroh::endpoint::RecvStream>>,
    read: Arc<AtomicU64>,
}

impl CodedSubscriber {
    async fn open(relay: &TestRelay, device: &TestDevice) -> Self {
        let (endpoint, connection, send, recv) = subscribe(relay, device).await;
        let read = Arc::new(AtomicU64::new(0));
        let mut subscriber = Self {
            _endpoint: endpoint,
            _connection: connection,
            _send: send,
            reader: SessionReader::new(Counted {
                inner: recv,
                read: read.clone(),
            }),
            read,
        };
        tilo_test_env::within("the subscription to be ready", async {
            while !matches!(
                subscriber.reader.read_next().await.unwrap(),
                Some(EnvelopeV2::State(Envelope::Ready { .. }))
            ) {}
        })
        .await;
        subscriber
    }

    /// The next delta as the session reader returns it, and the bytes its
    /// frame took on the wire.
    async fn delta(&mut self) -> (usize, EnvelopeV2) {
        tilo_test_env::within("the keystroke's coded delta", async {
            loop {
                let before = self.read.load(Ordering::Relaxed);
                let frame = self.reader.read_next().await.unwrap().unwrap();
                if matches!(frame, EnvelopeV2::State(Envelope::Delta { .. })) {
                    let wire = self.read.load(Ordering::Relaxed) - before;
                    return (wire as usize, frame);
                }
            }
        })
        .await
    }
}

/// Dial the relay as `device`, authenticate and subscribe.
async fn subscribe(
    relay: &TestRelay,
    device: &TestDevice,
) -> (
    iroh::Endpoint,
    iroh::endpoint::Connection,
    iroh::endpoint::SendStream,
    iroh::endpoint::RecvStream,
) {
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
    (endpoint, connection, send, recv)
}

#[derive(Debug, PartialEq, Eq)]
struct DeltaFields {
    workspace_id: u64,
    author: [u8; 32],
    authored_at_ns: u64,
    delta: Vec<u8>,
    signature: [u8; 64],
}

fn delta_fields(frame: EnvelopeV2) -> DeltaFields {
    match frame {
        EnvelopeV2::State(Envelope::Delta {
            workspace_id,
            author_device_public_key,
            authored_at_ns,
            delta,
            signature,
        }) => DeltaFields {
            workspace_id,
            author: author_device_public_key,
            authored_at_ns,
            delta,
            signature,
        },
        other => panic!("not a delta frame: {other:?}"),
    }
}

/// Typing one character at the end of the body, as an app's text control
/// does: through the executor, as its writer's step in the gesture of the
/// word it types. A word is eight keystrokes; every keystroke after a word's
/// first continues its gesture.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Type(String);

impl tilo_actions::Action for Type {
    fn apply(&self, doc: &mut tilo_actions::Doc) -> Result<(), tilo_actions::ActionError> {
        let Some((_, body)) = doc.get(ROOT, "body")? else {
            return Err(tilo_actions::ActionError::Refused("no body".into()));
        };
        let end = doc.length(&body);
        doc.splice_text(&body, end, 0, &self.0)?;
        Ok(())
    }
}

const WORD_KEYSTROKES: usize = 8;

fn type_one(doc: &mut AutoCommit, typed: usize) {
    let word = typed / WORD_KEYSTROKES;
    let gesture = tilo_actions::ActionId(uuid::Uuid::from_u128(
        u128::try_from(word).unwrap().saturating_add(1),
    ));
    let context = tilo_actions::ActionContext {
        what: Some(tilo_actions::Did::Edit(tilo_actions::FieldEdit {
            ty: "fixture".into(),
            field: "body".into(),
            path: vec![tilo_actions::PathStep::Field("body".into())],
            op: tilo_actions::EditOp::Splice,
            old: None,
            new: None,
            args: std::collections::BTreeMap::new(),
        })),
        gesture: Some(gesture),
        ..tilo_actions::ActionContext::for_resource("fixture", "fixture", "Fixture")
    };
    let action = Type("a".into());
    tilo_actions::dispatch_by_with_context(
        doc,
        &action,
        tilo_actions::EncodedAction::encode("fixture.type", &action).unwrap(),
        &mut tilo_actions::LocalTransport::new(),
        Some(&[3; 32]),
        context,
    )
    .unwrap();
}

fn text_document(actor: u8) -> Vec<u8> {
    let mut doc = AutoCommit::new().with_actor(automerge::ActorId::from(vec![actor; 16]));
    let body = doc.put_object(ROOT, "body", ObjType::Text).unwrap();
    doc.splice_text(&body, 0, 0, &"x".repeat(DOCUMENT_CHARS))
        .unwrap();
    doc.commit_edit("fixture", "body");
    doc.save()
}

/// Logical bytes and files under `root`, split into retained history and
/// everything else.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Footprint {
    retained_files: u64,
    retained_bytes: u64,
    other_files: u64,
    other_bytes: u64,
}

fn footprint(root: &Path) -> Footprint {
    fn walk(dir: &Path, retained: bool, out: &mut Footprint) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let meta = entry.metadata().unwrap();
            let retained = retained
                || path.file_name().and_then(|name| name.to_str())
                    == Some(tilo_relay::store::RETAINED_DIR);
            if meta.is_dir() {
                walk(&path, retained, out);
            } else if retained {
                out.retained_files += 1;
                out.retained_bytes += meta.len();
            } else {
                out.other_files += 1;
                out.other_bytes += meta.len();
            }
        }
    }
    let mut out = Footprint::default();
    walk(root, false, &mut out);
    out
}

/// Bytes the relay has written to `device`'s connected session.
fn session_bytes(metrics: &tilo_relay::metrics::Metrics, device: [u8; 32]) -> u64 {
    let sessions: Vec<_> = metrics
        .session_egress()
        .into_iter()
        .filter(|session| session.device_public_key == device)
        .collect();
    assert_eq!(sessions.len(), 1, "one live session per subscriber");
    sessions[0].bytes_sent
}

fn percentile(sorted: &[u64], p: usize) -> u64 {
    let rank = (sorted.len() * p).div_ceil(100).max(1);
    sorted[rank - 1]
}

fn sorted(mut values: Vec<u64>) -> Vec<u64> {
    values.sort_unstable();
    values
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_keystroke_costs_the_host_bounded_bytes_on_the_wire_and_disk() {
    let user = TestUser::fresh("Hosting cost");
    let author = user.enrol("laptop", DeviceCapabilities::primary());
    let coding = user.enrol("subscriber", DeviceCapabilities::member());
    let members = [author.device_pubkey(), coding.device_pubkey()];
    let relay = Box::pin(spawn_relay(
        &user,
        user.enrol("relay", DeviceCapabilities::host()),
        &[WORKSPACE],
        &members,
    ))
    .await;

    // The author's room state: four ~10 kB text documents.
    let now = SystemTime::now();
    let certificates = vec![author.cert.clone(), coding.cert.clone()];
    let mut owner = StatePeer::new(
        ROOM,
        author.cert.clone(),
        certificates.clone(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    owner
        .install_seed(
            SeedState {
                room_changes: seed_room(ROOM, &author.cert, &certificates).unwrap(),
                documents: ROUTES
                    .iter()
                    .zip(1u8..)
                    .map(|((app, record), actor)| SeedDocument {
                        app_id: (*app).into(),
                        record_id: (*record).into(),
                        kind: "text".into(),
                        bytes: text_document(actor),
                    })
                    .collect(),
            },
            &author.sk,
            now,
        )
        .unwrap();
    let mut documents: Vec<AutoCommit> = ROUTES
        .iter()
        .map(|(app, record)| {
            let mut doc = AutoCommit::load(&owner.document(app, record).unwrap()).unwrap();
            doc.set_actor(automerge::ActorId::from(author.device_pubkey().to_vec()));
            doc
        })
        .collect();

    let mut coded_peer = CodedSubscriber::open(&relay, &coding).await;
    // The publisher dials as the shell does.
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

    let store = relay
        .state_root
        .join("workspaces")
        .join(WORKSPACE.to_string());
    let metrics = relay.relay.handle().metrics;
    let egress_before = metrics.snapshot();
    let session_bytes_before = session_bytes(&metrics, coding.device_pubkey());
    let mut change_bytes = Vec::with_capacity(KEYSTROKES);
    let mut message_bytes = Vec::with_capacity(KEYSTROKES);
    let mut payload_bytes = Vec::with_capacity(KEYSTROKES);
    let mut stored_frame_bytes = Vec::with_capacity(KEYSTROKES);
    let mut new_frame_bytes = Vec::with_capacity(KEYSTROKES);
    let mut coded_frame_bytes = Vec::with_capacity(KEYSTROKES);
    let mut delivery_us = Vec::with_capacity(KEYSTROKES);
    let mut warm = None;
    let fsyncs_before = tilo_relay::store::fsyncs();
    let typing = Instant::now();
    for keystroke in 0..KEYSTROKES {
        let route = keystroke % ROUTES.len();
        let (app, record) = ROUTES[route];
        let doc = &mut documents[route];
        let heads = doc.get_heads();
        type_one(doc, keystroke / ROUTES.len());
        let change = doc.save_after(&heads);
        change_bytes.push(change.len() as u64);
        let message = owner
            .publish_incremental(app, record, change, &author.sk, SystemTime::now())
            .unwrap();
        let (channel, kind) = neighbour_wire::retention(&message, false).unwrap();
        assert_eq!(kind, Retention::Delta, "a keystroke is retained history");
        let body = postcard::to_allocvec(&message).unwrap();
        message_bytes.push(body.len() as u64);

        let sent = Instant::now();
        publisher.publish_retained_delta(channel, body).unwrap();
        let (coded_len, coded_frame) = coded_peer.delta().await;
        delivery_us.push(sent.elapsed().as_micros() as u64);

        let received = delta_fields(coded_frame);
        assert_eq!(
            received.signature, [0; 64],
            "the co-edit path signs no envelope, and the reader returns a coded delta as the \
             state value"
        );
        // The frame a coded delta rebuilds: the delta without its zero
        // signature, 66 bytes smaller than the whole state frame.
        let state = Envelope::Delta {
            workspace_id: received.workspace_id,
            author_device_public_key: received.author,
            authored_at_ns: received.authored_at_ns,
            delta: received.delta.clone(),
            signature: [0; 64],
        };
        let uncoded = encode_state(&state).unwrap();
        let whole =
            tilo_delivery_protocol::transport_v2::encode(&EnvelopeV2::State(state)).unwrap();
        assert_eq!(
            whole.len() - uncoded.len(),
            66,
            "the uncoded frame drops the zero signature's 66 bytes"
        );
        coded_frame_bytes.push(coded_len as u64);
        payload_bytes.push(received.delta.len() as u64);
        let stored = tilo_relay::transport::encode(&Envelope::Delta {
            workspace_id: received.workspace_id,
            author_device_public_key: received.author,
            authored_at_ns: received.authored_at_ns,
            delta: received.delta,
            signature: [0; 64],
        })
        .unwrap();
        stored_frame_bytes.push(stored.len() as u64);
        new_frame_bytes.push(uncoded.len() as u64);
        if keystroke == ROUTES.len() * 2 {
            warm = Some(footprint(&store));
        }
    }
    let typed_ms = typing.elapsed().as_millis();
    let fsyncs = tilo_relay::store::fsyncs() - fsyncs_before;
    let after = footprint(&store);
    let warm = warm.unwrap();

    // The relay's egress counters account for exactly the frames the
    // subscriber read: live fan-out in total and for this room, and the
    // subscriber's own session.
    let egress_after = metrics.snapshot();
    let wire_bytes: u64 = coded_frame_bytes.iter().sum();
    assert_eq!(
        egress_after.bytes_sent_live - egress_before.bytes_sent_live,
        wire_bytes,
        "live egress must count every fanned-out keystroke frame, once",
    );
    let room_live = |snapshot: &tilo_relay::metrics::MetricsSnapshot| {
        snapshot
            .rooms
            .get(&WORKSPACE)
            .map_or(0, |room| room.bytes_sent_live)
    };
    assert_eq!(
        room_live(&egress_after) - room_live(&egress_before),
        wire_bytes,
        "the room's live egress is the same frames",
    );
    let session = session_bytes(&metrics, coding.device_pubkey()) - session_bytes_before;
    assert!(
        session >= wire_bytes,
        "the session wrote {session} B, less than the {wire_bytes} B of keystroke frames its \
         subscriber read",
    );
    assert!(
        egress_after.bytes_sent - egress_before.bytes_sent >= wire_bytes,
        "total egress is at least the live frames",
    );

    // No file outside retained history grows with the number of edits: the
    // per-delta `log.jsonl` is gone, and nothing replaced it.
    assert_eq!(
        (after.other_files, after.other_bytes),
        (warm.other_files, warm.other_bytes),
        "metadata outside retained history grew with edits: {warm:?} -> {after:?}",
    );
    assert!(
        !store.join("log.jsonl").exists(),
        "the relay must not keep a per-delta append log"
    );

    let change_bytes = sorted(change_bytes);
    let message_bytes = sorted(message_bytes);
    let payload_bytes = sorted(payload_bytes);
    let stored_frame_bytes = sorted(stored_frame_bytes);
    let new_frame_bytes = sorted(new_frame_bytes);
    let coded_frame_bytes = sorted(coded_frame_bytes);
    let delivery_us = sorted(delivery_us);
    let keystrokes = KEYSTROKES as u64;
    println!(
        "HOSTING_COST keystrokes={keystrokes} routes={routes} \
         automerge_change_bytes_p50={change} neighbour_msg_bytes_p50={message} \
         coedit_payload_bytes_p50={payload} stored_frame_bytes_p50={stored} \
         frame_bytes_uncoded_p50={new} frame_bytes_uncoded_max={new_max} \
         frame_bytes_p50={coded} frame_bytes_max={coded_max} retained_files={retained_files} \
         retained_bytes_per_keystroke={retained_per} other_bytes_growth={growth} \
         delivery_us_p50={p50} delivery_us_p99={p99} fsyncs={fsyncs} typed_ms={typed_ms}",
        routes = ROUTES.len(),
        change = percentile(&change_bytes, 50),
        message = percentile(&message_bytes, 50),
        payload = percentile(&payload_bytes, 50),
        stored = percentile(&stored_frame_bytes, 50),
        new = percentile(&new_frame_bytes, 50),
        new_max = percentile(&new_frame_bytes, 100),
        coded = percentile(&coded_frame_bytes, 50),
        coded_max = percentile(&coded_frame_bytes, 100),
        retained_files = after.retained_files,
        retained_per = after.retained_bytes / keystrokes,
        growth = after.other_bytes - warm.other_bytes,
        p50 = percentile(&delivery_us, 50),
        p99 = percentile(&delivery_us, 99),
    );

    // Byte budgets: deterministic for a fixed scenario, so a regression in
    // framing (a whole-document resend, a re-wrapped payload, a field that
    // came back) fails here rather than in a hosting bill.
    let new_p50 = percentile(&new_frame_bytes, 50);
    assert!(
        new_p50 <= BUDGET_COMPACT_FRAME_P50_BYTES,
        "the median keystroke frame is {new_p50} B, over the \
         {BUDGET_COMPACT_FRAME_P50_BYTES} B budget",
    );
    let new_max = percentile(&new_frame_bytes, 100);
    assert!(
        new_max <= BUDGET_COMPACT_FRAME_MAX_BYTES,
        "the largest keystroke frame is {new_max} B, over the \
         {BUDGET_COMPACT_FRAME_MAX_BYTES} B budget",
    );
    let coded_p50 = percentile(&coded_frame_bytes, 50);
    assert!(
        coded_p50 <= BUDGET_CODED_FRAME_P50_BYTES,
        "the median coded keystroke frame is {coded_p50} B, over the \
         {BUDGET_CODED_FRAME_P50_BYTES} B budget",
    );
    assert!(
        percentile(&coded_frame_bytes, 100) <= new_max + 16,
        "a coded frame never costs more than a few bytes over the uncoded one",
    );
    let change_max = percentile(&change_bytes, 100);
    assert!(
        new_max <= 4 * change_max,
        "framing overhead exceeds 4x the {change_max} B Automerge change: {new_max} B",
    );

    publisher.close().await;
    relay.relay.shutdown().await;
}
