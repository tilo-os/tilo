//! LIVE_V1 lane C on the installed native data routes: verified items are
//! handed to the live forwarder from inside their commit, before the durable
//! store barrier; items that fail any admission check never are; a forwarded
//! item whose barrier fails is absent from durable replay until the author's
//! retry commits the same digest (the receivers' correction signal).
use super::{
    data_tests::{Harness, committed, drive, frame, native_edit, page, poll_target},
    *,
};
use crate::live_service::{PACKAGE_PRIMARY_RECORD, signed_change};
use hyper::StatusCode;
use std::sync::Mutex;
use tilo_relay::delivery_v3::{
    framing::{self, MessageType},
    live_wire::{LiveRecord, OpsFrame, ops_track},
    state::{AdmissionOutcome, StateAdmission, StateItemChunk},
};

#[derive(Clone, Debug)]
struct Forwarded {
    device: [u8; 32],
    canonical: Vec<u8>,
    /// Digest of the durable checkpoint when the sink ran.
    durable_at_forward: [u8; 32],
    at: std::time::Instant,
}

fn durable_digest(root: &std::path::Path) -> [u8; 32] {
    *blake3::hash(&std::fs::read(root.join("session.bin")).unwrap()).as_bytes()
}

/// Replace the live forwarder sink with a recorder on the real data routes.
fn record(h: &mut Harness) -> Arc<Mutex<Vec<Forwarded>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    let root = h.f.owner.root().to_owned();
    h.ingress
        .data
        .install_admitted_sink(Arc::new(move |device, canonical: &[u8]| {
            sink.lock().unwrap().push(Forwarded {
                device,
                canonical: canonical.to_vec(),
                durable_at_forward: durable_digest(&root),
                at: std::time::Instant::now(),
            })
        }));
    seen
}

/// One complete inline `POST /peer/v3/state`; returns status and admission.
async fn inline(
    h: &mut Harness,
    id: [u8; 16],
    canonical: &[u8],
) -> (StatusCode, Option<StateAdmission>) {
    let chunk = StateItemChunk::new(
        id,
        *blake3::hash(canonical).as_bytes(),
        canonical.len() as u64,
        0,
        canonical,
    )
    .unwrap();
    let (status, bytes) = h
        .signed(
            "POST",
            "/peer/v3/state",
            frame(MessageType::StateItemChunk, &chunk.encode().unwrap()),
        )
        .await;
    let admission = (status == StatusCode::OK).then(|| {
        let (kind, payload, _) = framing::decode_frame_prefix(&bytes).unwrap();
        assert_eq!(kind, MessageType::StateAdmission);
        StateAdmission::decode(payload).unwrap()
    });
    (status, admission)
}

fn accepted(admission: &Option<StateAdmission>) -> bool {
    admission
        .as_ref()
        .is_some_and(|a| matches!(a.outcome(), AdmissionOutcome::Accepted { .. }))
}

/// Durable replay groups served from the start through the latest commit.
async fn replayed_groups(h: &mut Harness) -> usize {
    let last =
        *h.f.owner
            .delivery_ledger()
            .unwrap()
            .replay
            .last_key_value()
            .unwrap()
            .0;
    let mut previous: Option<tilo_relay::delivery_v3::state::CommitBoundary> = None;
    let mut groups = 0;
    while previous
        .as_ref()
        .is_none_or(|b| b.committed().sequence < last)
    {
        let (status, bytes) = h
            .signed("GET", &poll_target(previous.as_ref()), vec![])
            .await;
        assert_eq!(status, StatusCode::OK);
        let (_, boundary) = page(&bytes);
        groups += 1;
        previous = Some(boundary);
    }
    groups
}

#[tokio::test]
async fn verified_items_are_forwarded_before_the_durable_barrier_inline_chat_and_staged() {
    let mut h = Harness::start().await;
    let seen = record(&mut h);
    let root = h.f.owner.root().to_owned();
    let device = h.f.owner.identity.native_cert.device_public_key;

    // Record-less chat document, inline.
    let before = durable_digest(&root);
    let chat = native_edit(&h.f.owner, "live", vec![1; 32]);
    let sent = std::time::Instant::now();
    let (status, admission) = inline(&mut h, [81; 16], &chat).await;
    let answered = std::time::Instant::now();
    assert_eq!(status, StatusCode::OK);
    assert!(accepted(&admission), "{admission:?}");
    let after = durable_digest(&root);
    assert_ne!(before, after, "the item is durable once answered");
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].device, device);
        assert_eq!(seen[0].canonical, chat, "exact canonical bytes");
        assert_eq!(
            seen[0].durable_at_forward, before,
            "forwarded while the durable checkpoint still predates the item"
        );
        assert!(sent <= seen[0].at && seen[0].at <= answered);
    }
    // Chat maps to the stable package-primary live record, inside the contract.
    let (record, bytes) = signed_change(&chat).expect("record-less chat is forwardable");
    assert_eq!(
        record,
        LiveRecord {
            app: "so.tilo.chat".into(),
            record: PACKAGE_PRIMARY_RECORD.into(),
        }
    );
    assert_eq!(
        PACKAGE_PRIMARY_RECORD,
        ui::record::RecordId::package_primary().as_str()
    );
    assert!(ops_track(&record).starts_with("ops/"));
    let OpsFrame::SignedChange { canonical, .. } = OpsFrame::decode(&bytes).unwrap() else {
        panic!("signed change frame");
    };
    assert_eq!(canonical, chat);

    // A small item uploaded in two staged chunks is forwarded on completion,
    // again before its barrier.
    let staged = native_edit(&h.f.owner, "staged", vec![2; 20 * 1024]);
    let half = staged.len() / 2;
    let (kind, _) = h.chunk([82; 16], &staged, 0..half).await;
    assert_eq!(kind, MessageType::FlowCredit);
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "partial staging forwards nothing"
    );
    let before = durable_digest(&root);
    let (kind, payload) = h.chunk([82; 16], &staged, half..staged.len()).await;
    assert_eq!(kind, MessageType::StateAdmission);
    assert!(matches!(
        StateAdmission::decode(&payload).unwrap().outcome(),
        AdmissionOutcome::Accepted { .. }
    ));
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[1].canonical, staged);
        assert_eq!(seen[1].durable_at_forward, before);
    }
    assert_ne!(durable_digest(&root), before);

    // An item too large for one ops frame is admitted but reaches members only
    // through durable replay.
    let large = native_edit(&h.f.owner, "large", vec![3; 300 * 1024]);
    let (admission, _) = h.upload([83; 16], &large).await;
    assert!(matches!(
        admission.outcome(),
        AdmissionOutcome::Accepted { .. }
    ));
    assert_eq!(seen.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn items_that_fail_admission_or_are_duplicates_are_never_forwarded() {
    let mut h = Harness::start().await;
    let seen = record(&mut h);

    // Forged: signature no longer covers the (changed) cluster.
    let valid = native_edit(&h.f.owner, "forged", vec![4; 16]);
    let mut message: qos_peer_state::NeighbourMsg = postcard::from_bytes(&valid).unwrap();
    if let qos_peer_state::NeighbourMsg::AppDocument { cluster_id, .. } = &mut message {
        *cluster_id = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".into();
    }
    let forged = postcard::to_stdvec(&message).unwrap();
    let (_, admission) = inline(&mut h, [91; 16], &forged).await;
    assert!(!accepted(&admission), "{admission:?}");
    // Malformed canonical bytes.
    let (_, admission) = inline(&mut h, [92; 16], b"not a canonical item").await;
    assert!(!accepted(&admission));
    // Missing prerequisite: an edit built on an unpublished edit waits.
    let mut author = h.f.owner.peer().clone();
    let first = {
        let mut document =
            ui::automerge::AutoCommit::load(&author.document("so.tilo.chat", "").unwrap()).unwrap();
        let heads = document.get_heads();
        ui::automerge::transaction::Transactable::put(
            &mut document,
            ui::automerge::ROOT,
            "first",
            1_u64,
        )
        .unwrap();
        document.commit();
        author
            .publish_incremental(
                "so.tilo.chat",
                "",
                document.save_after(&heads),
                &h.f.owner.identity.native,
                web_time::SystemTime::now(),
            )
            .unwrap()
    };
    let second = {
        let mut document =
            ui::automerge::AutoCommit::load(&author.document("so.tilo.chat", "").unwrap()).unwrap();
        let heads = document.get_heads();
        ui::automerge::transaction::Transactable::put(
            &mut document,
            ui::automerge::ROOT,
            "second",
            2_u64,
        )
        .unwrap();
        document.commit();
        author
            .publish_incremental(
                "so.tilo.chat",
                "",
                document.save_after(&heads),
                &h.f.owner.identity.native,
                web_time::SystemTime::now(),
            )
            .unwrap()
    };
    let second = postcard::to_stdvec(&second).unwrap();
    let (_, admission) = inline(&mut h, [93; 16], &second).await;
    assert!(
        admission
            .as_ref()
            .is_some_and(|a| matches!(a.outcome(), AdmissionOutcome::RetryLater { .. })),
        "{admission:?}"
    );
    assert!(
        seen.lock().unwrap().is_empty(),
        "no rejected or deferred item is forwarded"
    );

    // The accepted prerequisite is forwarded once; its duplicate is not.
    let first = postcard::to_stdvec(&first).unwrap();
    let (_, admission) = inline(&mut h, [94; 16], &first).await;
    assert!(accepted(&admission), "{admission:?}");
    assert_eq!(seen.lock().unwrap().len(), 1);
    let (_, again) = inline(&mut h, [94; 16], &first).await;
    assert_eq!(
        again, admission,
        "lost-reply probe returns the original receipt"
    );
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "duplicates are never re-forwarded"
    );
    let _ = committed(admission.as_ref().unwrap());
}

#[tokio::test]
async fn a_forwarded_item_whose_barrier_fails_is_not_durable_until_the_retry_commits() {
    let mut h = Harness::start().await;
    let seen = record(&mut h);
    let root = h.f.owner.root().to_owned();
    let canonical = native_edit(&h.f.owner, "fails", vec![5; 32]);
    let message: qos_peer_state::NeighbourMsg = postcard::from_bytes(&canonical).unwrap();
    let before = durable_digest(&root);
    let groups = replayed_groups(&mut h).await;

    crate::store::test_fault::arm(&root, crate::store::test_fault::Point::BeforeWrite);
    let (status, admission) = inline(&mut h, [95; 16], &canonical).await;
    assert!(!accepted(&admission), "{status} {admission:?}");
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "forwarded before the failed barrier"
    );
    assert_eq!(seen.lock().unwrap()[0].canonical, canonical);
    assert_eq!(durable_digest(&root), before, "nothing durable");

    // The host reopens (a failed barrier leaves durability uncertain). Durable
    // state does not cover the forwarded change and replay has no new group:
    // receivers holding it speculatively roll it back at their deadline.
    let mut h = h.restart().await;
    assert!(!h.f.owner.peer().covers(&message));
    assert_eq!(replayed_groups(&mut h).await, groups);

    // The author's outbox retries the same exact item; it is now committed and
    // replayed as its author's original signed bytes. The durable cut that
    // carries it settles the receivers' speculative copy; host-certified
    // coverage follows with the next retention checkpoint.
    let seen = record(&mut h);
    let (_, admission) = inline(&mut h, [95; 16], &canonical).await;
    assert!(accepted(&admission), "{admission:?}");
    assert_eq!(seen.lock().unwrap().len(), 1);
    let ledger = h.f.owner.delivery_ledger().unwrap();
    let (_, entry) = ledger.replay.last_key_value().unwrap();
    assert_eq!(
        entry.publications.last().unwrap().payload,
        *blake3::hash(&canonical).as_bytes(),
        "durable replay carries the exact forwarded bytes"
    );
    assert_eq!(replayed_groups(&mut h).await, groups + 1);
    assert!(!h.f.owner.peer().covers(&message));
    h.f.owner.permit_unscoped_legacy_commits().unwrap();
    h.f.current
        .with_owner_view(&mut h.f.owner, |_, owner| {
            owner.retain_delivery(
                crate::owner::retention::RetentionAction::Checkpoint,
                web_time::SystemTime::now(),
            )
        })
        .unwrap()
        .unwrap();
    assert!(h.f.owner.peer().covers(&message));
}

fn percentiles(label: &str, mut values: Vec<f64>) {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let at = |p: f64| values[((values.len() as f64 - 1.0) * p).round() as usize];
    println!(
        "{label}: n={} p50={:.3} p90={:.3} p99={:.3} max={:.3} ms",
        values.len(),
        at(0.5),
        at(0.9),
        at(0.99),
        values[values.len() - 1]
    );
}

/// One live subscriber session on the host relay: the forwarded frames of
/// `record` on the author's own namespace, timestamped on arrival by a task
/// independent of the owner loop that `drive` runs on the test thread.
async fn subscribe_ops(
    h: &mut Harness,
    participation: u8,
    record: &LiveRecord,
) -> (
    super::live_tests::LiveClient,
    tokio::sync::mpsc::UnboundedReceiver<(Vec<u8>, tokio::time::Instant)>,
    tokio::task::JoinHandle<()>,
) {
    use super::live_tests::{grant, live_connect, request};
    use tilo_relay::delivery_v3::live_wire::room_digest;
    use tokio::time::{Duration, Instant, timeout};
    let wanted = request(h, participation, true);
    let (status, token) = grant(h, &wanted).await;
    assert_eq!(status, StatusCode::OK);
    let client = live_connect(h, token).await.unwrap();
    let path = format!(
        "tilo-live/{}/{}",
        hex::encode(room_digest(h.f.owner.peer().room())),
        hex::encode(wanted.device)
    );
    let own = timeout(
        Duration::from_secs(5),
        client.incoming.consume().announced_broadcast(path.as_str()),
    )
    .await
    .unwrap()
    .expect("own namespace relayed");
    let mut subscription = moq_net::track::Subscription::default();
    subscription.ordered = true;
    let mut ops = own
        .track(&ops_track(record))
        .unwrap()
        .subscribe(subscription)
        .await
        .unwrap();
    let (frames, received) = tokio::sync::mpsc::unbounded_channel();
    let listener = tokio::spawn(async move {
        let _own = own;
        while let Ok(Some(mut group)) = ops.recv_group().await {
            if let Ok(Some(frame)) = group.read_frame().await {
                let _ = frames.send((frame.payload.to_vec(), Instant::now()));
            }
        }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    (client, received, listener)
}

/// End to end on loopback: TLS HTTPS upload of a record-less chat change on
/// the installed native host, a native MoQ Draft 21 live subscriber on the
/// same host's `/moq/v21/live`, and a held durable poll opened beforehand.
/// Each sample times, from the upload request start: the forwarded
/// `SignedChange` arriving at the subscriber, the upload's durable receipt,
/// and the durable replay page. `LIVE_FORWARD_SAMPLES` sets the sample count
/// (default 5); timings print with `--nocapture`. Samples run in batches of
/// 16 per fresh live session: the relay (lane A) withdraws a namespace's
/// egress ops track ~2-3 s into steady use and a re-subscription on the same
/// namespace is not served until that track is gone, so batches pause 3.5 s.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chat_changes_reach_live_subscribers_ahead_of_durable_replay() {
    use tokio::time::{Duration, Instant, timeout};
    const BATCH: usize = 16;
    let samples: usize = std::env::var("LIVE_FORWARD_SAMPLES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);
    let mut h = Harness::start().await;
    let record = LiveRecord {
        app: "so.tilo.chat".into(),
        record: PACKAGE_PRIMARY_RECORD.into(),
    };
    // Latest durable boundary: held polls wait for the next group.
    let last =
        *h.f.owner
            .delivery_ledger()
            .unwrap()
            .replay
            .last_key_value()
            .unwrap()
            .0;
    let mut boundary: Option<tilo_relay::delivery_v3::state::CommitBoundary> = None;
    while boundary
        .as_ref()
        .is_none_or(|b| b.committed().sequence < last)
    {
        let (_, bytes) = h
            .signed("GET", &poll_target(boundary.as_ref()), vec![])
            .await;
        boundary = Some(page(&bytes).1);
    }
    let address = h.client.address;
    let tls = h.client.tls.clone();
    let (mut live, mut ack, mut durable, mut lead) = (vec![], vec![], vec![], vec![]);
    let (mut live_before_ack, mut missed, mut sequence) = (0, 0, 0_u64);
    for batch in 0..samples.div_ceil(BATCH) {
        let (client, mut received, listener) =
            subscribe_ops(&mut h, 21 + batch as u8, &record).await;
        let count = BATCH.min(samples - batch * BATCH);
        for sample in 0..=count {
            sequence += 1;
            let canonical = native_edit(&h.f.owner, "live", sequence.to_le_bytes().to_vec());
            let held = poll_target(boundary.as_ref());
            let held_proof = h.proof("GET", &held, &[]);
            let mut id = [7; 16];
            id[..8].copy_from_slice(&sequence.to_le_bytes());
            let chunk = StateItemChunk::new(
                id,
                *blake3::hash(&canonical).as_bytes(),
                canonical.len() as u64,
                0,
                &canonical,
            )
            .unwrap();
            let body = frame(MessageType::StateItemChunk, &chunk.encode().unwrap());
            let proof = h.proof("POST", "/peer/v3/state", &body);
            let client = || super::tests::Client {
                address,
                tls: tls.clone(),
            };
            let poller = client();
            let poll = tokio::spawn(async move {
                let reply = poller
                    .request_method("GET", &held, vec![], Some(held_proof))
                    .await;
                (reply, Instant::now())
            });
            let uploader = client();
            let upload = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                let started = Instant::now();
                let reply = uploader
                    .request_method("POST", "/peer/v3/state", body, Some(proof))
                    .await;
                (started, reply, Instant::now())
            });
            let receiver = &mut received;
            let expected = canonical.clone();
            let run = async move {
                // A new subscriber may first see recent groups from the relay's
                // 2 s in-memory window; wait for this sample's exact change.
                let forwarded = timeout(Duration::from_secs(2), async {
                    while let Some((bytes, at)) = receiver.recv().await {
                        if matches!(OpsFrame::decode(&bytes), Ok(OpsFrame::SignedChange { canonical, .. }) if canonical == expected) {
                            return Some((bytes, at));
                        }
                    }
                    None
                })
                .await
                .ok()
                .flatten();
                (poll.await.unwrap(), upload.await.unwrap(), forwarded)
            };
            let (
                ((poll_status, page_bytes), durable_at),
                (started, (status, _), acked_at),
                forwarded,
            ) = drive(&mut h.ingress, &mut h.f.owner, run).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(poll_status, StatusCode::OK);
            boundary = Some(page(&page_bytes).1);
            let Some((bytes, live_at)) = forwarded else {
                missed += 1;
                if std::env::var_os("LIVE_FORWARD_TRACE").is_some() {
                    eprintln!("LIVE_FORWARD missed batch {batch} sample {sample}");
                }
                continue;
            };
            let OpsFrame::SignedChange {
                canonical: forwarded,
                record: named,
                ..
            } = OpsFrame::decode(&bytes).unwrap()
            else {
                panic!("signed change");
            };
            assert_eq!(named, record);
            assert_eq!(forwarded, canonical, "the exact uploaded chat change");
            if sample == 0 {
                continue; // warm-up (egress track creation)
            }
            let ms = |at: Instant| at.duration_since(started).as_secs_f64() * 1000.0;
            live.push(ms(live_at));
            ack.push(ms(acked_at));
            durable.push(ms(durable_at));
            lead.push(ms(durable_at) - ms(live_at));
            live_before_ack += usize::from(live_at <= acked_at);
        }
        listener.abort();
        client.session.abort(moq_net::Error::Cancel);
        if (batch + 1) * BATCH < samples {
            // Let the relay withdraw this namespace's egress ops track before
            // the next session subscribes (see the relay note in the report).
            tokio::time::sleep(Duration::from_millis(3500)).await;
        }
    }
    let measured = live.len();
    percentiles("LIVE_FORWARD upload->live SignedChange", live);
    percentiles("LIVE_FORWARD upload->durable receipt (author ack)", ack);
    percentiles("LIVE_FORWARD upload->durable replay page", durable);
    percentiles("LIVE_FORWARD live lead over durable replay", lead);
    println!(
        "LIVE_FORWARD live frame before author ack: {live_before_ack}/{measured}; missed {missed}"
    );
    if let Some(relay) = h.ingress.transport.live.get() {
        let c = relay.counters();
        println!(
            "LIVE_FORWARD relay injected={} injected_dropped={} records_refused={} accepted={} revoked={}",
            c.injected, c.injected_dropped, c.records_refused, c.accepted, c.revoked
        );
    }
    // Frames can be lost to a relay-side revocation of the author's live
    // session under sustained owner load (lane A relay, reported separately);
    // every frame that did arrive was checked for exact bytes above.
    assert!(measured > 0 || samples == 0, "changes are forwarded live");
}
