use super::*;
use crate::delivery_http::{HttpSessions, TestClock, tests::Fixture};
use crate::https::{
    BufferedRequest, Ingress,
    tests::{HeldPollTestClient, data_test_request},
};
use tilo_relay::delivery_v3::{
    commit::CommitRef,
    http_proof::{Request, RequestProof},
    state::{AdmissionOutcome, CommitScope, StateAdmission, StateItemChunk},
};
use ui::automerge::{ROOT, transaction::Transactable};

#[test]
fn attached_owner_callback_uses_the_actual_ingress_resource_turn() {
    let (mut f, budget, ingress) = attached_fixture();
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    let session = f.handshake(&mut registry, [109; 32]);
    let body = state_body(1, &native_edit(&f.owner, "attached-callback"));
    let response = send_state(&mut f, &mut registry, session, &body, &budget, &ingress).unwrap();
    assert!(matches!(
        response.outcome(),
        AdmissionOutcome::Accepted { .. }
    ));
}

fn attached_fixture() -> (Fixture, HostBytes, Ingress) {
    let mut f = Fixture::new();
    let (ingress, _receiver) = crate::https::ingress_channel();
    let budget = ingress.host_bytes();
    f.current
        .with_owner(&mut f.owner, |owner| owner.attach_data_resources(&budget))
        .unwrap()
        .unwrap();
    (f, budget, ingress)
}
fn charged_request(
    ingress: &Ingress,
    request: &Request<'_>,
    proof: &RequestProof,
) -> (BufferedRequest, HeldPollTestClient) {
    let method = match request.method() {
        tilo_relay::delivery_v3::http_proof::Method::Get => "GET",
        tilo_relay::delivery_v3::http_proof::Method::Post => "POST",
        _ => panic!("not a data callback method"),
    };
    data_test_request(
        ingress,
        method,
        request.target(),
        request.body(),
        &proof.encode().unwrap(),
        tokio::time::Instant::now() + std::time::Duration::from_secs(5),
    )
}
fn native_edit(owner: &SessionOwner, key: &str) -> Vec<u8> {
    let mut author = owner.peer().clone();
    let mut document =
        ui::automerge::AutoCommit::load(&author.document("dev.qos.chat", "").unwrap()).unwrap();
    let heads = document.get_heads();
    document.put(ROOT, key, key).unwrap();
    document.commit();
    let msg = author
        .publish_incremental(
            "dev.qos.chat",
            "",
            document.save_after(&heads),
            &owner.identity.native,
            web_time::SystemTime::now(),
        )
        .unwrap();
    postcard::to_stdvec(&msg).unwrap()
}
fn state_body(id: u8, canonical: &[u8]) -> Vec<u8> {
    let item = StateItemChunk::new(
        [id; 16],
        *blake3::hash(canonical).as_bytes(),
        canonical.len() as u64,
        0,
        canonical,
    )
    .unwrap();
    let mut body = Vec::new();
    framing::append_frame(
        &mut body,
        MessageType::StateItemChunk,
        &item.encode().unwrap(),
        1024 * 1024,
    )
    .unwrap();
    body
}
fn send_state(
    f: &mut Fixture,
    registry: &mut HttpSessions,
    session: ([u8; 16], [u8; 32]),
    body: &[u8],
    budget: &HostBytes,
    ingress: &Ingress,
) -> anyhow::Result<StateAdmission> {
    let request = Request::new("POST", "/peer/v3/state", body)?;
    let proof = RequestProof::sign(
        session.0,
        session.1,
        &request,
        f.now,
        f.now + 30_000_000_000,
        &f.owner.identity.native,
    )?;
    let items = prepare_state(&request).map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let (original, _client) = charged_request(ingress, &request, &proof);
    let (admitted, mut checker, turn) = registry.admit_revalidating_at(
        &f.owner,
        &f.current,
        budget,
        OwnerDataOperation::State {
            input_len: items[0].canonical.len(),
        },
        &proof,
        &request,
        &original,
        &TestClock::new(f.now),
    )?;
    let response = admit_state_core(
        &mut f.owner,
        &f.current,
        &admitted,
        |owner, view, session| checker.check(owner, view, session),
        budget,
        turn,
    )?;
    let (kind, payload, rest) = framing::decode_frame_prefix(&response)?;
    assert_eq!(kind, MessageType::StateAdmission);
    assert!(rest.is_empty());
    Ok(StateAdmission::decode(payload)?)
}
#[test]
fn callback_commits_native_receipt_and_fresh_proof_retry_survives_owner_reopen() {
    let (mut f, budget, ingress) = attached_fixture();
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    let session = f.handshake(&mut registry, [101; 32]);
    let canonical = native_edit(&f.owner, "durable-http");
    let body = state_body(1, &canonical);
    let first = send_state(&mut f, &mut registry, session, &body, &budget, &ingress).unwrap();
    let (operations, committed) = match first.outcome() {
        AdmissionOutcome::Accepted {
            scope: CommitScope::NativeCommitted,
            operations,
            committed,
        } => (operations.clone(), committed.clone()),
        outcome => panic!("not a native durable admission: {outcome:?}"),
    };
    assert_eq!(operations.len(), 1);
    assert_eq!(
        f.owner.delivery_ledger().unwrap().replay[&committed.sequence].digest,
        committed.digest
    );
    let path = f.owner.root().to_owned();
    let document = f.owner.peer().document("dev.qos.chat", "").unwrap();
    drop(f.owner);
    f.owner = SessionOwner::open(&path).unwrap();
    f.owner.attach_data_resources(&budget).unwrap();
    f.current = CommittedAuthority::new(&f.owner);
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    let session = f.handshake(&mut registry, [102; 32]);
    let generation = f.owner.status().generation;
    let retry = send_state(&mut f, &mut registry, session, &body, &budget, &ingress).unwrap();
    assert_eq!(retry, first);
    assert_eq!(f.owner.status().generation, generation);
    assert_eq!(
        f.owner.peer().document("dev.qos.chat", "").unwrap(),
        document
    );
    let changed = state_body(1, &native_edit(&f.owner, "changed-same-id"));
    assert!(send_state(&mut f, &mut registry, session, &changed, &budget, &ingress).is_err());
    assert_eq!(f.owner.status().generation, generation);
}

#[test]
fn callback_ack_uses_real_cursor_and_persists_once_without_relocking_current() {
    let (mut f, budget, ingress) = attached_fixture();
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    let session = f.handshake(&mut registry, [103; 32]);
    register_consumer(&mut f, &mut registry, session, &budget);
    // Obtain the actual registry-owned session only through a successful exact proof.
    let poll = Request::new("GET", "/peer/v3/events?mode=poll", b"").unwrap();
    let proof = RequestProof::sign(
        session.0,
        session.1,
        &poll,
        f.now,
        f.now + 30_000_000_000,
        &f.owner.identity.native,
    )
    .unwrap();
    let mut turn = f
        .owner
        .prepare_data_turn(&budget, OwnerDataOperation::Replay)
        .unwrap();
    let admitted = registry
        .admit(&f.owner, &f.current, &proof, &poll, f.now)
        .unwrap();

    let token = {
        let guard = f.current.read().unwrap();
        let source = f
            .owner
            .data_source(&mut turn, guard.view(), admitted.session(), f.now)
            .unwrap();
        let (sequence, entry) = f
            .owner
            .delivery_ledger()
            .unwrap()
            .replay
            .last_key_value()
            .unwrap();
        f.owner
            .data_issue_cursor(
                &mut turn,
                &source,
                CommitRef {
                    sequence: *sequence,
                    digest: entry.digest,
                },
            )
            .unwrap()
    };
    drop(admitted);
    drop(turn);
    let expected = token
        .authenticate(&f.owner.identity.host.verifying_key())
        .unwrap()
        .committed()
        .clone();
    let payload = CursorAck::new(token).encode().unwrap();
    let mut body = Vec::new();
    framing::append_frame(&mut body, MessageType::CursorAck, &payload, 8192).unwrap();
    let before = f.owner.status().generation;
    for attempt in 0..2 {
        let request = Request::new("POST", "/peer/v3/acks", &body).unwrap();
        let proof = RequestProof::sign(
            session.0,
            session.1,
            &request,
            f.now,
            f.now + 30_000_000_000,
            &f.owner.identity.native,
        )
        .unwrap();
        let (original, _client) = charged_request(&ingress, &request, &proof);
        let (admitted, mut checker, turn) = registry
            .admit_revalidating_at(
                &f.owner,
                &f.current,
                &budget,
                OwnerDataOperation::Ack,
                &proof,
                &request,
                &original,
                &TestClock::new(f.now),
            )
            .unwrap();
        ack_core(
            &mut f.owner,
            &f.current,
            &admitted,
            |owner, view, session| checker.check(owner, view, session),
            turn,
        )
        .unwrap();
        assert_eq!(f.owner.status().generation, before + 1, "attempt {attempt}");
    }
    let device = f.owner.identity.native_cert.device_public_key;
    assert_eq!(
        f.owner.delivery_ledger().unwrap().consumers[&device].acknowledged,
        Some(expected.clone())
    );
    let path = f.owner.root().to_owned();
    drop(f.owner);
    let reopened = SessionOwner::open(&path).unwrap();
    assert_eq!(
        reopened.delivery_ledger().unwrap().consumers[&device].acknowledged,
        Some(expected)
    );
}

fn register_consumer(
    f: &mut Fixture,
    registry: &mut HttpSessions,
    session: ([u8; 16], [u8; 32]),
    budget: &HostBytes,
) {
    let request = Request::new("GET", "/peer/v3/events?mode=poll", b"").unwrap();
    let proof = RequestProof::sign(
        session.0,
        session.1,
        &request,
        f.now,
        f.now + 30_000_000_000,
        &f.owner.identity.native,
    )
    .unwrap();
    let mut turn = f
        .owner
        .prepare_data_turn(budget, OwnerDataOperation::RegisterConsumer)
        .unwrap();
    let admitted = registry
        .admit(&f.owner, &f.current, &proof, &request, f.now)
        .unwrap();
    f.current
        .with_owner_view(&mut f.owner, |view, owner| {
            let source = owner.data_source(&mut turn, view, admitted.session(), f.now)?;
            owner.data_register_consumer(&mut turn, &source)
        })
        .unwrap()
        .unwrap();
}
fn poll(
    f: &Fixture,
    registry: &mut HttpSessions,
    session: ([u8; 16], [u8; 32]),
    target: &str,
    budget: &HostBytes,
    ingress: &Ingress,
) -> anyhow::Result<Option<Vec<u8>>> {
    let request = Request::new("GET", target, b"")?;
    let proof = RequestProof::sign(
        session.0,
        session.1,
        &request,
        f.now,
        f.now + 30_000_000_000,
        &f.owner.identity.native,
    )?;
    let (original, _client) = charged_request(ingress, &request, &proof);
    let (admitted, mut checker, turn) = registry.admit_revalidating_at(
        &f.owner,
        &f.current,
        budget,
        OwnerDataOperation::Replay,
        &proof,
        &request,
        &original,
        &TestClock::new(f.now),
    )?;
    replay_core(
        &f.owner,
        &f.current,
        &admitted,
        |owner, view, session| checker.check(owner, view, session),
        turn,
    )
}
#[test]
fn replay_callback_returns_original_native_group_before_its_real_signed_boundary() {
    use tilo_relay::delivery_v3::state::{
        CommitBoundary, EventChunk, EventSource, visible_order_digest,
    };
    let (mut f, budget, ingress) = attached_fixture();
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    let session = f.handshake(&mut registry, [104; 32]);
    register_consumer(&mut f, &mut registry, session, &budget);
    let expected = {
        let request = Request::new("GET", "/peer/v3/events?mode=poll", b"").unwrap();
        let proof = RequestProof::sign(
            session.0,
            session.1,
            &request,
            f.now,
            f.now + 30_000_000_000,
            &f.owner.identity.native,
        )
        .unwrap();
        let mut turn = f
            .owner
            .prepare_data_turn(&budget, OwnerDataOperation::Replay)
            .unwrap();
        let admitted = registry
            .admit(&f.owner, &f.current, &proof, &request, f.now)
            .unwrap();
        let guard = f.current.read().unwrap();
        let source = f
            .owner
            .data_source(&mut turn, guard.view(), admitted.session(), f.now)
            .unwrap();
        let (sequence, entry) = f
            .owner
            .delivery_ledger()
            .unwrap()
            .replay
            .first_key_value()
            .unwrap();
        f.owner
            .data_replay(
                &mut turn,
                &source,
                &CommitRef {
                    sequence: *sequence,
                    digest: entry.digest,
                },
            )
            .unwrap()
            .iter()
            .map(|p| p.unwrap().bytes.to_vec())
            .collect::<Vec<_>>()
    };
    let page = poll(
        &f,
        &mut registry,
        session,
        "/peer/v3/events?mode=poll",
        &budget,
        &ingress,
    )
    .unwrap()
    .unwrap();
    assert!(page.len() <= 1024 * 1024);
    let mut rest = page.as_slice();
    let mut originals = Vec::<Vec<u8>>::new();
    let mut digests = Vec::new();
    let mut frames = 0;
    let boundary = loop {
        let (kind, payload, next) = framing::decode_frame_prefix(rest).unwrap();
        frames += 1;
        match kind {
            MessageType::EventChunk => {
                let chunk = EventChunk::decode(payload).unwrap();
                assert_eq!(chunk.source(), EventSource::Native);
                let ordinal = chunk.ordinal() as usize;
                if ordinal == originals.len() {
                    originals.push(vec![]);
                    digests.push((chunk.ordinal(), chunk.event_digest()));
                }
                assert_eq!(chunk.offset(), originals[ordinal].len() as u64);
                originals[ordinal].extend_from_slice(chunk.bytes());
            }
            MessageType::CommitBoundary => {
                assert!(next.is_empty(), "boundary must terminate complete group");
                break CommitBoundary::decode(payload).unwrap();
            }
            kind => panic!("wrong download lane {kind:?}"),
        }
        rest = next;
    };
    assert!(frames <= 64);
    assert_eq!(originals, expected);
    assert_eq!(boundary.visible_count(), originals.len() as u32);
    assert_eq!(
        boundary.visible_order_digest(),
        visible_order_digest(&digests).unwrap()
    );
    for ((_, digest), bytes) in digests.iter().zip(&originals) {
        assert_eq!(digest, blake3::hash(bytes).as_bytes());
    }
    let signed = boundary
        .cursor()
        .authenticate(&f.owner.identity.host.verifying_key())
        .unwrap();
    assert_eq!(signed.committed(), boundary.committed());
    let consumer = &f.owner.delivery_ledger().unwrap().consumers
        [&f.owner.identity.native_cert.device_public_key];
    assert_eq!(boundary.consumer(), consumer.id);
    assert_eq!(boundary.authorization_view(), consumer.authorization_view);
    let target = format!(
        "/peer/v3/events?cursor={}&mode=poll",
        state::encode_cursor_url(boundary.cursor().as_bytes()).unwrap()
    );
    // Initialization/transport installation may produce several real groups.
    // Walk the actual indexed suffix once; generation-only work creates no page.
    let remaining = f
        .owner
        .delivery_ledger()
        .unwrap()
        .replay
        .range((
            std::ops::Bound::Excluded(boundary.committed().sequence),
            std::ops::Bound::Unbounded,
        ))
        .map(|(sequence, entry)| CommitRef {
            sequence: *sequence,
            digest: entry.digest,
        })
        .collect::<Vec<_>>();
    let mut target = target;
    for expected_cut in remaining {
        let page = poll(&f, &mut registry, session, &target, &budget, &ingress)
            .unwrap()
            .unwrap();
        let mut rest = page.as_slice();
        let next_boundary = loop {
            let (kind, payload, next) = framing::decode_frame_prefix(rest).unwrap();
            if kind == MessageType::CommitBoundary {
                assert!(next.is_empty());
                break CommitBoundary::decode(payload).unwrap();
            }
            assert_eq!(kind, MessageType::EventChunk);
            rest = next;
        };
        assert_eq!(next_boundary.committed(), &expected_cut);
        target = format!(
            "/peer/v3/events?cursor={}&mode=poll",
            state::encode_cursor_url(next_boundary.cursor().as_bytes()).unwrap()
        );
    }
    assert!(
        poll(&f, &mut registry, session, &target, &budget, &ingress)
            .unwrap()
            .is_none()
    );
}

#[test]
fn every_batch_item_observes_the_current_view_after_earlier_receipt_commit() {
    let (mut f, budget, ingress) = attached_fixture();
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    let session = f.handshake(&mut registry, [105; 32]);
    let first = native_edit(&f.owner, "before-batch");
    send_state(
        &mut f,
        &mut registry,
        session,
        &state_body(1, &first),
        &budget,
        &ingress,
    )
    .unwrap();
    let second = native_edit(&f.owner, "second-batch-item");
    let mut body = state_body(2, &first); // New ID for an already admitted operation commits metadata.
    body.extend(state_body(3, &second));
    let request = Request::new("POST", "/peer/v3/state", &body).unwrap();
    let proof = RequestProof::sign(
        session.0,
        session.1,
        &request,
        f.now,
        f.now + 30_000_000_000,
        &f.owner.identity.native,
    )
    .unwrap();
    let (original, _client) = charged_request(&ingress, &request, &proof);
    let (admitted, mut checker, turn) = registry
        .admit_revalidating_at(
            &f.owner,
            &f.current,
            &budget,
            OwnerDataOperation::State {
                input_len: first.len(),
            },
            &proof,
            &request,
            &original,
            &TestClock::new(f.now),
        )
        .unwrap();
    let before = f.owner.status().generation;
    let response = admit_state_core(
        &mut f.owner,
        &f.current,
        &admitted,
        |owner, view, session| checker.check(owner, view, session),
        &budget,
        turn,
    )
    .unwrap();
    let (_, first, rest) = framing::decode_frame_prefix(&response).unwrap();
    let (_, second, rest) = framing::decode_frame_prefix(rest).unwrap();
    assert!(rest.is_empty());
    assert!(matches!(
        StateAdmission::decode(first).unwrap().outcome(),
        AdmissionOutcome::Duplicate {
            scope: CommitScope::NativeCommitted,
            ..
        }
    ));
    assert!(matches!(
        StateAdmission::decode(second).unwrap().outcome(),
        AdmissionOutcome::Accepted {
            scope: CommitScope::NativeCommitted,
            ..
        }
    ));
    assert_eq!(f.owner.status().generation, before + 2);
}

#[test]
fn malformed_batch_tail_prevents_the_first_valid_item_from_mutating_owner() {
    let (mut f, budget, ingress) = attached_fixture();
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    let session = f.handshake(&mut registry, [106; 32]);
    let mut body = state_body(1, &native_edit(&f.owner, "must-not-commit"));
    body.extend_from_slice(b"TLD3");
    let before = f.owner.status().generation;
    assert!(send_state(&mut f, &mut registry, session, &body, &budget, &ingress).is_err());
    assert_eq!(f.owner.status().generation, before);
    assert!(f.owner.delivery_ledger().unwrap().admissions.is_empty());
}

#[test]
fn failed_real_barriers_emit_no_receipt_and_reopen_reconciles_stable_input() {
    use crate::store::test_fault::{Point, with_fault};
    for point in [
        Point::BeforeWrite,
        Point::AfterAtomicWrite,
        Point::DirectoryBarrier,
    ] {
        let (mut f, budget, ingress) = attached_fixture();
        let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
        let session = f.handshake(&mut registry, [107; 32]);
        let body = state_body(1, &native_edit(&f.owner, "faulted-native-callback"));
        let path = f.owner.root().to_owned();
        assert!(
            with_fault(&path, point, || send_state(
                &mut f,
                &mut registry,
                session,
                &body,
                &budget,
                &ingress
            ))
            .is_err()
        );
        assert!(!f.owner.permits_publication());
        assert!(f.current.read().is_err());
        drop(f.owner);
        f.owner = SessionOwner::open(&path).unwrap();
        f.owner.attach_data_resources(&budget).unwrap();
        f.current = CommittedAuthority::new(&f.owner);
        let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
        let session = f.handshake(&mut registry, [108; 32]);
        let recovered =
            send_state(&mut f, &mut registry, session, &body, &budget, &ingress).unwrap();
        assert!(matches!(
            recovered.outcome(),
            AdmissionOutcome::Accepted {
                scope: CommitScope::NativeCommitted,
                ..
            } | AdmissionOutcome::Duplicate {
                scope: CommitScope::NativeCommitted,
                ..
            }
        ));
        assert_eq!(f.owner.delivery_ledger().unwrap().admissions.len(), 1);
    }
}

#[test]
fn oversized_real_replay_group_answers_snapshot_reset_without_page_or_ack_advancement() {
    let (mut f, budget, ingress) = attached_fixture();
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    let session = f.handshake(&mut registry, [110; 32]);
    register_consumer(&mut f, &mut registry, session, &budget);
    let mut preceding = None;
    for index in 0_u8..6 {
        // Rebase the real native author after each admitted operation/checkpoint.
        let mut author = f.owner.peer().clone();
        let mut document =
            ui::automerge::AutoCommit::load(&author.document("dev.qos.chat", "").unwrap()).unwrap();
        let heads = document.get_heads();
        let mut bytes = vec![0; 192 * 1024];
        let mut generator = blake3::Hasher::new();
        generator.update(b"real oversized replay fixture");
        generator.update(&[index]);
        generator.finalize_xof().fill(&mut bytes);
        document
            .put(
                ROOT,
                format!("oversized-{index}"),
                ui::automerge::ScalarValue::Bytes(bytes),
            )
            .unwrap();
        document.commit();
        let message = author
            .publish_incremental(
                "dev.qos.chat",
                "",
                document.save_after(&heads),
                &f.owner.identity.native,
                web_time::SystemTime::now(),
            )
            .unwrap();
        let canonical = postcard::to_stdvec(&message).unwrap();
        let (sequence, entry) = f
            .owner
            .delivery_ledger()
            .unwrap()
            .replay
            .last_key_value()
            .unwrap();
        preceding = Some(CommitRef {
            sequence: *sequence,
            digest: entry.digest,
        });
        let receipt = send_state(
            &mut f,
            &mut registry,
            session,
            &state_body(index + 1, &canonical),
            &budget,
            &ingress,
        )
        .unwrap();
        assert!(matches!(
            receipt.outcome(),
            AdmissionOutcome::Accepted { .. }
        ));
    }
    // Uploads publish their authors' incremental items, never a host copy of
    // the growing document. A certified checkpoint of the ~1.1 MiB document
    // (without rotation) is one complete group above the page budget.
    let (sequence, entry) = f
        .owner
        .delivery_ledger()
        .unwrap()
        .replay
        .last_key_value()
        .unwrap();
    preceding = Some(CommitRef {
        sequence: *sequence,
        digest: entry.digest,
    });
    f.owner.permit_unscoped_legacy_commits().unwrap();
    f.current
        .with_owner_view(&mut f.owner, |_, owner| owner.refresh_checkpoint())
        .unwrap()
        .unwrap();
    let preceding = preceding.unwrap();
    let token = {
        let request = Request::new("GET", "/peer/v3/events?mode=poll", b"").unwrap();
        let proof = RequestProof::sign(
            session.0,
            session.1,
            &request,
            f.now,
            f.now + 30_000_000_000,
            &f.owner.identity.native,
        )
        .unwrap();
        let mut turn = f
            .owner
            .prepare_data_turn(&budget, OwnerDataOperation::Replay)
            .unwrap();
        let admitted = registry
            .admit(&f.owner, &f.current, &proof, &request, f.now)
            .unwrap();
        let guard = f.current.read().unwrap();
        let source = f
            .owner
            .data_source(&mut turn, guard.view(), admitted.session(), f.now)
            .unwrap();
        let (sequence, entry) = f
            .owner
            .delivery_ledger()
            .unwrap()
            .replay
            .last_key_value()
            .unwrap();
        let final_cut = CommitRef {
            sequence: *sequence,
            digest: entry.digest,
        };
        let publications = f.owner.data_replay(&mut turn, &source, &final_cut).unwrap();
        assert!(
            publications.len() < 64,
            "fixture must hit bytes, not frame count"
        );
        let actual_bytes: usize = publications.iter().map(|p| p.unwrap().bytes.len()).sum();
        assert!(
            actual_bytes > 1024 * 1024,
            "real native group was only {actual_bytes} bytes"
        );
        f.owner
            .data_issue_cursor(&mut turn, &source, preceding.clone())
            .unwrap()
    };
    let target = format!(
        "/peer/v3/events?cursor={}&mode=poll",
        state::encode_cursor_url(token.as_bytes()).unwrap(),
    );
    let device = f.owner.identity.native_cert.device_public_key;
    let before_ack = f.owner.delivery_ledger().unwrap().consumers[&device]
        .acknowledged
        .clone();
    let generation = f.owner.status().generation;
    for _ in 0..2 {
        // Fresh proof, same verified cut: no prefix/boundary or hidden
        // advancement. The route answers the contract's SnapshotRequired
        // (HistoryUnavailable) reset, never a denial: the consumer installs a
        // chunked snapshot and continues with the suffix.
        let error = poll(&f, &mut registry, session, &target, &budget, &ingress).unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<crate::owner::delivery::error::OwnerDataFailure>()
                .map(|failure| failure.kind()),
            Some(crate::owner::delivery::error::OwnerDataFailureKind::Reset(
                tilo_relay::delivery_v3::state::ResetReason::HistoryUnavailable
            ))
        );
        assert_eq!(f.owner.status().generation, generation);
        assert_eq!(
            f.owner.delivery_ledger().unwrap().consumers[&device].acknowledged,
            before_ack
        );
    }
}

#[test]
fn rotated_store_cursor_ack_is_a_no_op_and_its_poll_answers_store_rotated() {
    let (mut f, budget, ingress) = attached_fixture();
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    let session = f.handshake(&mut registry, [114; 32]);
    register_consumer(&mut f, &mut registry, session, &budget);
    let edit = native_edit(&f.owner, "before-rotation");
    send_state(
        &mut f,
        &mut registry,
        session,
        &state_body(1, &edit),
        &budget,
        &ingress,
    )
    .unwrap();
    let token = current_tail_cursor(&f, &mut registry, session, &budget);
    // Retention rotates the store to a certified checkpoint incarnation.
    f.owner.permit_unscoped_legacy_commits().unwrap();
    f.current
        .with_owner_view(&mut f.owner, |_, owner| {
            owner.retain_delivery(
                crate::owner::retention::RetentionAction::Rotate,
                web_time::SystemTime::now(),
            )
        })
        .unwrap()
        .unwrap();
    let generation = f.owner.status().generation;
    let payload = CursorAck::new(token.clone()).encode().unwrap();
    let mut body = Vec::new();
    framing::append_frame(&mut body, MessageType::CursorAck, &payload, 8192).unwrap();
    let request = Request::new("POST", "/peer/v3/acks", &body).unwrap();
    let proof = RequestProof::sign(
        session.0,
        session.1,
        &request,
        f.now,
        f.now + 30_000_000_000,
        &f.owner.identity.native,
    )
    .unwrap();
    let (original, _client) = charged_request(&ingress, &request, &proof);
    let (admitted, mut checker, turn) = registry
        .admit_revalidating_at(
            &f.owner,
            &f.current,
            &budget,
            OwnerDataOperation::Ack,
            &proof,
            &request,
            &original,
            &TestClock::new(f.now),
        )
        .unwrap();
    // Not a denial (which would discard the client's session): a no-op.
    ack_core(
        &mut f.owner,
        &f.current,
        &admitted,
        |owner, view, session| checker.check(owner, view, session),
        turn,
    )
    .unwrap();
    assert_eq!(f.owner.status().generation, generation);
    let device = f.owner.identity.native_cert.device_public_key;
    assert_eq!(
        f.owner.delivery_ledger().unwrap().consumers[&device].acknowledged,
        None
    );
    let target = format!(
        "/peer/v3/events?cursor={}&mode=poll",
        state::encode_cursor_url(token.as_bytes()).unwrap(),
    );
    let error = poll(&f, &mut registry, session, &target, &budget, &ingress).unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<crate::owner::delivery::error::OwnerDataFailure>()
            .map(|failure| failure.kind()),
        Some(crate::owner::delivery::error::OwnerDataFailureKind::Reset(
            tilo_relay::delivery_v3::state::ResetReason::StoreRotated
        ))
    );
}

#[derive(Clone, Copy)]
enum BetweenItems {
    SessionExpiry,
    RequestDeadline,
    ClientCancellation,
}
fn stops_between_items(cause: BetweenItems) {
    let (mut f, budget, ingress) = attached_fixture();
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    let session = f.handshake(&mut registry, [111; 32]);
    let first = native_edit(&f.owner, "already-native-before-stop");
    send_state(
        &mut f,
        &mut registry,
        session,
        &state_body(1, &first),
        &budget,
        &ingress,
    )
    .unwrap();
    let second = native_edit(&f.owner, "must-not-enter-after-stop");
    let mut body = state_body(2, &first);
    body.extend(state_body(3, &second));
    let request = Request::new("POST", "/peer/v3/state", &body).unwrap();
    let proof = RequestProof::sign(
        session.0,
        session.1,
        &request,
        f.now,
        f.now + 30_000_000_000,
        &f.owner.identity.native,
    )
    .unwrap();
    let (original, client) = charged_request(&ingress, &request, &proof);
    let clock = TestClock::new(f.now);
    let (admitted, mut checker, turn) = registry
        .admit_revalidating_at(
            &f.owner,
            &f.current,
            &budget,
            OwnerDataOperation::State {
                input_len: first.len(),
            },
            &proof,
            &request,
            &original,
            &clock,
        )
        .unwrap();
    let expires = admitted.session().expires_at_ns();
    let before = f.owner.status().generation;
    let admissions = f.owner.delivery_ledger().unwrap().admissions.len();
    let document = f.owner.peer().document("dev.qos.chat", "").unwrap();
    let mut checks = 0;
    let outcome = admit_state_core(
        &mut f.owner,
        &f.current,
        &admitted,
        |owner, view, session| {
            checks += 1;
            let now = checker.check(owner, view, session)?;
            if checks == 1 {
                match cause {
                    BetweenItems::SessionExpiry => clock.set_wall(expires),
                    BetweenItems::RequestDeadline => {
                        clock.advance_monotonic(std::time::Duration::from_secs(6))
                    }
                    BetweenItems::ClientCancellation => client.cancel(),
                }
            }
            Ok(now)
        },
        &budget,
        turn,
    );
    assert!(
        outcome.is_err(),
        "second item entered after original context was fenced"
    );
    assert_eq!(
        checks, 2,
        "each item must consult the genuine current checker"
    );
    assert_eq!(f.owner.status().generation, before + 1);
    assert_eq!(
        f.owner.delivery_ledger().unwrap().admissions.len(),
        admissions + 1
    );
    assert_eq!(
        f.owner.peer().document("dev.qos.chat", "").unwrap(),
        document
    );
    assert!(f.owner.permits_publication());
    assert!(
        f.current.read().is_ok(),
        "ordinary denial must not poison committed authority"
    );
    drop(admitted);
    drop(checker);
    if !matches!(cause, BetweenItems::SessionExpiry) {
        assert!(
            registry
                .admit(&f.owner, &f.current, &proof, &request, f.now)
                .is_err(),
            "original nonce must remain consumed"
        );
    }
}
#[test]
fn native_batch_stops_when_session_expires_between_items() {
    stops_between_items(BetweenItems::SessionExpiry);
}
#[test]
fn native_batch_stops_when_original_deadline_expires_between_items() {
    stops_between_items(BetweenItems::RequestDeadline);
}
#[test]
fn native_batch_stops_when_original_client_cancels_between_items() {
    stops_between_items(BetweenItems::ClientCancellation);
}
#[test]
fn replay_page_revalidates_expiry_after_original_proof_admission() {
    let (mut f, budget, ingress) = attached_fixture();
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    let session = f.handshake(&mut registry, [112; 32]);
    register_consumer(&mut f, &mut registry, session, &budget);
    let request = Request::new("GET", "/peer/v3/events?mode=poll", b"").unwrap();
    let proof = RequestProof::sign(
        session.0,
        session.1,
        &request,
        f.now,
        f.now + 30_000_000_000,
        &f.owner.identity.native,
    )
    .unwrap();
    let (original, _client) = charged_request(&ingress, &request, &proof);
    let clock = TestClock::new(f.now);
    let (admitted, mut checker, turn) = registry
        .admit_revalidating_at(
            &f.owner,
            &f.current,
            &budget,
            OwnerDataOperation::Replay,
            &proof,
            &request,
            &original,
            &clock,
        )
        .unwrap();
    clock.set_wall(admitted.session().expires_at_ns());
    let before = f.owner.status().generation;
    let outcome = replay_core(
        &f.owner,
        &f.current,
        &admitted,
        |owner, view, session| checker.check(owner, view, session),
        turn,
    );
    assert!(
        outcome.is_err(),
        "expired session received a page after original admission"
    );
    assert_eq!(f.owner.status().generation, before);
}

#[test]
fn native_batch_revalidates_real_governed_host_revocation_before_next_item() {
    let (mut f, budget, ingress) = attached_fixture();
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    let session = f.handshake(&mut registry, [113; 32]);
    let host = f.owner.host_certificate().device_public_key;
    let mut revoked = tilo_user_identity::RevocationEntry {
        user_public_key: f.owner.host_certificate().user_public_key,
        revoked_device_public_key: host,
        revoked_at_ns: f.now,
        reason: tilo_user_identity::RevocationReason::Retired,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut revoked, &f.owner.identity.root).unwrap();
    let mut author = f.owner.peer().clone();
    author
        .publish_governed_revocations(
            &[revoked],
            &f.owner.identity.native,
            web_time::SystemTime::UNIX_EPOCH + web_time::Duration::from_nanos(f.now),
        )
        .unwrap();
    let governance = postcard::to_stdvec(&author.room_message()).unwrap();
    let second = native_edit(&f.owner, "must-not-enter-revoked-host");
    let mut body = state_body(1, &governance);
    body.extend(state_body(2, &second));
    let request = Request::new("POST", "/peer/v3/state", &body).unwrap();
    let proof = RequestProof::sign(
        session.0,
        session.1,
        &request,
        f.now,
        f.now + 30_000_000_000,
        &f.owner.identity.native,
    )
    .unwrap();
    let (original, _client) = charged_request(&ingress, &request, &proof);
    let clock = TestClock::new(f.now);
    let (admitted, mut checker, turn) = registry
        .admit_revalidating_at(
            &f.owner,
            &f.current,
            &budget,
            OwnerDataOperation::State {
                input_len: governance.len(),
            },
            &proof,
            &request,
            &original,
            &clock,
        )
        .unwrap();
    let before = f.owner.status().generation;
    let document = f.owner.peer().document("dev.qos.chat", "").unwrap();
    let mut checks = 0;
    let outcome = admit_state_core(
        &mut f.owner,
        &f.current,
        &admitted,
        |owner, view, session| {
            checks += 1;
            checker.check(owner, view, session)
        },
        &budget,
        turn,
    );
    assert!(
        outcome.is_err(),
        "host revocation must fence subsequent data"
    );
    assert_eq!(checks, 2);
    assert_eq!(f.owner.status().generation, before + 1);
    assert!(
        f.owner
            .peer()
            .revocations()
            .iter()
            .any(|entry| entry.revoked_device_public_key == host)
    );
    assert_eq!(
        f.owner.peer().document("dev.qos.chat", "").unwrap(),
        document
    );
}

fn current_tail_cursor(
    f: &Fixture,
    registry: &mut HttpSessions,
    session: ([u8; 16], [u8; 32]),
    budget: &HostBytes,
) -> CursorToken {
    let request = Request::new("GET", "/peer/v3/events?mode=poll", b"").unwrap();
    let proof = RequestProof::sign(
        session.0,
        session.1,
        &request,
        f.now,
        f.now + 30_000_000_000,
        &f.owner.identity.native,
    )
    .unwrap();
    let mut turn = f
        .owner
        .prepare_data_turn(budget, OwnerDataOperation::Replay)
        .unwrap();
    let admitted = registry
        .admit(&f.owner, &f.current, &proof, &request, f.now)
        .unwrap();
    let guard = f.current.read().unwrap();
    let source = f
        .owner
        .data_source(&mut turn, guard.view(), admitted.session(), f.now)
        .unwrap();
    let (sequence, entry) = f
        .owner
        .delivery_ledger()
        .unwrap()
        .replay
        .last_key_value()
        .unwrap();
    f.owner
        .data_issue_cursor(
            &mut turn,
            &source,
            CommitRef {
                sequence: *sequence,
                digest: entry.digest,
            },
        )
        .unwrap()
}
#[test]
fn held_original_poll_resumes_new_native_group_without_nonce_readmission() {
    use crate::delivery_http::poll::Polls;
    let (mut f, budget, ingress) = attached_fixture();
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    let session = f.handshake(&mut registry, [114; 32]);
    register_consumer(&mut f, &mut registry, session, &budget);
    let cursor = current_tail_cursor(&f, &mut registry, session, &budget);
    let target = format!(
        "/peer/v3/events?cursor={}&mode=poll",
        state::encode_cursor_url(cursor.as_bytes()).unwrap()
    );
    let request = Request::new("GET", &target, b"").unwrap();
    let proof = RequestProof::sign(
        session.0,
        session.1,
        &request,
        f.now,
        f.now + 30_000_000_000,
        &f.owner.identity.native,
    )
    .unwrap();
    let (original, _client) = charged_request(&ingress, &request, &proof);
    let clock = TestClock::new(f.now);
    let mut polls = Polls::new();
    let ticket = {
        let (admitted, mut checker, turn) = registry
            .admit_revalidating_at(
                &f.owner,
                &f.current,
                &budget,
                OwnerDataOperation::Replay,
                &proof,
                &request,
                &original,
                &clock,
            )
            .unwrap();
        let ticket = polls
            .capture(&admitted, &original, checker.entry_serial())
            .unwrap();
        assert!(
            replay_core(
                &f.owner,
                &f.current,
                &admitted,
                |owner, view, session| checker.check(owner, view, session),
                turn
            )
            .unwrap()
            .is_none()
        );
        ticket
    };
    let id = polls.park(ticket, original).unwrap();
    let native = native_edit(&f.owner, "new-native-group-wakes-poll");
    let admission = send_state(
        &mut f,
        &mut registry,
        session,
        &state_body(1, &native),
        &budget,
        &ingress,
    )
    .unwrap();
    let expected_cut = match admission.outcome() {
        AdmissionOutcome::Accepted { committed, .. } => committed.clone(),
        outcome => panic!("not durable: {outcome:?}"),
    };
    let pending = polls.take(id).unwrap();
    let page = {
        let (session, mut checker, turn) = registry
            .resume_poll_at(&f.owner, &f.current, &budget, &pending, &clock)
            .unwrap();
        replay_pending_core(
            &f.owner,
            &f.current,
            &pending,
            session,
            |owner, view, session| checker.check(owner, view, session),
            turn,
        )
        .unwrap()
        .unwrap()
    };
    let mut rest = page.as_slice();
    let boundary = loop {
        let (kind, payload, next) = framing::decode_frame_prefix(rest).unwrap();
        if kind == MessageType::CommitBoundary {
            assert!(next.is_empty());
            break state::CommitBoundary::decode(payload).unwrap();
        }
        assert_eq!(kind, MessageType::EventChunk);
        rest = next;
    };
    assert_eq!(boundary.committed(), &expected_cut);
    assert!(
        registry
            .admit(&f.owner, &f.current, &proof, &request, f.now)
            .is_err(),
        "resume must not reset the consumed original nonce"
    );
    pending
        .into_request()
        .respond(hyper::StatusCode::OK, page)
        .unwrap();
}

/// Host efficiency: with the session journal, an advancing ACK is a signed
/// side record (no aggregate commit, no new owner generation), durable before
/// the ACK returns, recovered after abrupt death and folded by the next
/// aggregate commit.
#[test]
fn journaled_ack_is_a_durable_side_record_recovered_after_crash() {
    crate::store::test_journal::with_journal(|| {
        let (mut f, budget, ingress) = attached_fixture();
        let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
        let session = f.handshake(&mut registry, [113; 32]);
        register_consumer(&mut f, &mut registry, session, &budget);
        let poll = Request::new("GET", "/peer/v3/events?mode=poll", b"").unwrap();
        let proof = RequestProof::sign(
            session.0,
            session.1,
            &poll,
            f.now,
            f.now + 30_000_000_000,
            &f.owner.identity.native,
        )
        .unwrap();
        let mut turn = f
            .owner
            .prepare_data_turn(&budget, OwnerDataOperation::Replay)
            .unwrap();
        let admitted = registry
            .admit(&f.owner, &f.current, &proof, &poll, f.now)
            .unwrap();
        let token = {
            let guard = f.current.read().unwrap();
            let source = f
                .owner
                .data_source(&mut turn, guard.view(), admitted.session(), f.now)
                .unwrap();
            let (sequence, entry) = f
                .owner
                .delivery_ledger()
                .unwrap()
                .replay
                .last_key_value()
                .unwrap();
            f.owner
                .data_issue_cursor(
                    &mut turn,
                    &source,
                    CommitRef {
                        sequence: *sequence,
                        digest: entry.digest,
                    },
                )
                .unwrap()
        };
        drop(admitted);
        drop(turn);
        let expected = token
            .authenticate(&f.owner.identity.host.verifying_key())
            .unwrap()
            .committed()
            .clone();
        let payload = CursorAck::new(token).encode().unwrap();
        let mut body = Vec::new();
        framing::append_frame(&mut body, MessageType::CursorAck, &payload, 8192).unwrap();
        let root = f.owner.root().to_owned();
        let checkpoint = std::fs::read(root.join("session.bin")).unwrap();
        let before = f.owner.status().generation;
        for attempt in 0..2 {
            let request = Request::new("POST", "/peer/v3/acks", &body).unwrap();
            let proof = RequestProof::sign(
                session.0,
                session.1,
                &request,
                f.now,
                f.now + 30_000_000_000,
                &f.owner.identity.native,
            )
            .unwrap();
            let (original, _client) = charged_request(&ingress, &request, &proof);
            let (admitted, mut checker, turn) = registry
                .admit_revalidating_at(
                    &f.owner,
                    &f.current,
                    &budget,
                    OwnerDataOperation::Ack,
                    &proof,
                    &request,
                    &original,
                    &TestClock::new(f.now),
                )
                .unwrap();
            ack_core(
                &mut f.owner,
                &f.current,
                &admitted,
                |owner, view, session| checker.check(owner, view, session),
                turn,
            )
            .unwrap();
            assert_eq!(f.owner.status().generation, before, "attempt {attempt}");
            // One record, written on the first (advancing) ACK only.
            let log = std::fs::metadata(root.join("session.acks")).unwrap().len();
            assert!(
                log > 0 && log < 512,
                "attempt {attempt}: side log {log} bytes"
            );
        }
        assert_eq!(std::fs::read(root.join("session.bin")).unwrap(), checkpoint);
        let device = f.owner.identity.native_cert.device_public_key;
        assert_eq!(
            f.owner.delivery_ledger().unwrap().consumers[&device].acknowledged,
            Some(expected.clone())
        );
        // Abrupt death: neither journal compaction nor any fold.
        f.owner.crash_store_on_drop();
        drop(f.owner);
        let mut reopened = SessionOwner::open(&root).unwrap();
        assert_eq!(
            reopened.delivery_ledger().unwrap().consumers[&device].acknowledged,
            Some(expected.clone())
        );
        // The reopen barrier commit folded the record and truncated the log.
        assert_eq!(
            std::fs::metadata(root.join("session.acks")).unwrap().len(),
            0
        );
        reopened.crash_store_on_drop();
        drop(reopened);
        let again = SessionOwner::open(&root).unwrap();
        assert_eq!(
            again.delivery_ledger().unwrap().consumers[&device].acknowledged,
            Some(expected)
        );
    });
}

// ---- Lane D2: one commit/replay group per multi-item request; multi-group pages.

/// Consecutive edits by one author, each building on the previous one.
fn native_edits(owner: &SessionOwner, keys: &[&str]) -> Vec<Vec<u8>> {
    let mut author = owner.peer().clone();
    let mut document =
        ui::automerge::AutoCommit::load(&author.document("dev.qos.chat", "").unwrap()).unwrap();
    keys.iter()
        .map(|key| {
            let heads = document.get_heads();
            document.put(ROOT, *key, *key).unwrap();
            document.commit();
            let msg = author
                .publish_incremental(
                    "dev.qos.chat",
                    "",
                    document.save_after(&heads),
                    &owner.identity.native,
                    web_time::SystemTime::now(),
                )
                .unwrap();
            postcard::to_stdvec(&msg).unwrap()
        })
        .collect()
}
/// Run one multi-item state request with a first turn reserved for the batch.
fn send_batch(
    f: &mut Fixture,
    registry: &mut HttpSessions,
    session: ([u8; 16], [u8; 32]),
    items: &[(u8, &[u8])],
    budget: &HostBytes,
    ingress: &Ingress,
    sink: Option<&crate::owner::delivery::early_forward::EarlySink>,
) -> anyhow::Result<Vec<StateAdmission>> {
    let mut body = Vec::new();
    for (id, canonical) in items {
        body.extend(state_body(*id, canonical));
    }
    let request = Request::new("POST", "/peer/v3/state", &body)?;
    let proof = RequestProof::sign(
        session.0,
        session.1,
        &request,
        f.now,
        f.now + 30_000_000_000,
        &f.owner.identity.native,
    )?;
    let total = items.iter().map(|(_, canonical)| canonical.len()).sum();
    let (original, _client) = charged_request(ingress, &request, &proof);
    let (admitted, mut checker, turn) = registry.admit_revalidating_at(
        &f.owner,
        &f.current,
        budget,
        OwnerDataOperation::State { input_len: total },
        &proof,
        &request,
        &original,
        &TestClock::new(f.now),
    )?;
    let response = crate::owner::delivery::early_forward::scoped(sink, || {
        admit_state_core(
            &mut f.owner,
            &f.current,
            &admitted,
            |owner, view, session| checker.check(owner, view, session),
            budget,
            turn,
        )
    })?;
    let mut admissions = Vec::new();
    let mut rest = response.as_slice();
    while !rest.is_empty() {
        let (kind, payload, next) = framing::decode_frame_prefix(rest)?;
        assert_eq!(kind, MessageType::StateAdmission);
        admissions.push(StateAdmission::decode(payload)?);
        rest = next;
    }
    Ok(admissions)
}
fn accepted_commit(admission: &StateAdmission) -> (CommitRef, Vec<[u8; 32]>) {
    match admission.outcome() {
        AdmissionOutcome::Accepted {
            scope: CommitScope::NativeCommitted,
            committed,
            operations,
        } => (committed.clone(), operations.clone()),
        other => panic!("expected native acceptance, got {other:?}"),
    }
}

#[test]
fn consecutive_new_items_commit_as_one_group_and_forward_each_before_the_barrier() {
    use std::sync::{Arc, Mutex};
    let (mut f, budget, ingress) = attached_fixture();
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    let session = f.handshake(&mut registry, [120; 32]);
    let edits = native_edits(&f.owner, &["g1", "g2", "g3", "g4"]);
    let items = edits
        .iter()
        .enumerate()
        .map(|(i, e)| (i as u8 + 1, e.as_slice()))
        .collect::<Vec<_>>();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = seen.clone();
    let sink: crate::owner::delivery::early_forward::EarlySink = Arc::new(move |_, bytes| {
        record.lock().unwrap().push(bytes.to_vec());
    });
    let before = f.owner.status().generation;
    let replay_before = f.owner.delivery_ledger().unwrap().replay.len();
    let admissions = send_batch(
        &mut f,
        &mut registry,
        session,
        &items,
        &budget,
        &ingress,
        Some(&sink),
    )
    .unwrap();
    assert_eq!(admissions.len(), 4);
    assert_eq!(f.owner.status().generation, before + 1, "one commit");
    assert_eq!(
        f.owner.delivery_ledger().unwrap().replay.len(),
        replay_before + 1,
        "one replay group"
    );
    let (group, _) = accepted_commit(&admissions[0]);
    let mut operations = Vec::new();
    for (admission, (id, canonical)) in admissions.iter().zip(&items) {
        assert_eq!(admission.delivery(), [*id; 16]);
        assert_eq!(
            admission.input_digest(),
            *blake3::hash(canonical).as_bytes()
        );
        let (committed, ops) = accepted_commit(admission);
        assert_eq!(committed, group, "every receipt references the same group");
        assert_eq!(ops.len(), 1);
        operations.push(ops[0]);
    }
    let entry = &f.owner.delivery_ledger().unwrap().replay[&group.sequence];
    assert_eq!(entry.digest, group.digest);
    assert!(
        operations
            .iter()
            .all(|op| entry.admitted_operations.contains(op))
    );
    assert_eq!(
        *seen.lock().unwrap(),
        edits,
        "each item forwarded once, in order"
    );
    // A lost-reply retry of the whole batch answers from the stored receipts.
    let retried = send_batch(
        &mut f,
        &mut registry,
        session,
        &items,
        &budget,
        &ingress,
        None,
    )
    .unwrap();
    assert_eq!(
        f.owner.status().generation,
        before + 1,
        "retries commit nothing"
    );
    for (retry, original) in retried.iter().zip(&admissions) {
        match retry.outcome() {
            AdmissionOutcome::Accepted { committed, .. }
            | AdmissionOutcome::Duplicate { committed, .. } => {
                assert_eq!(committed, &accepted_commit(original).0)
            }
            other => panic!("retry lost its receipt: {other:?}"),
        }
    }
}

#[test]
fn a_refused_item_ends_the_group_without_entering_its_commit() {
    let (mut f, budget, ingress) = attached_fixture();
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    let session = f.handshake(&mut registry, [121; 32]);
    let edits = native_edits(&f.owner, &["r1", "r2"]);
    let garbage = b"not a canonical native message".to_vec();
    let before = f.owner.status().generation;
    let admissions = f.owner.delivery_ledger().unwrap().admissions.len();
    let outcome = send_batch(
        &mut f,
        &mut registry,
        session,
        &[(1, &edits[0]), (2, &edits[1]), (3, &garbage)],
        &budget,
        &ingress,
        None,
    );
    // As before, a refused item fails the request after the earlier items
    // committed; now those earlier items share one group.
    assert!(outcome.is_err());
    assert_eq!(f.owner.status().generation, before + 1);
    let ledger = f.owner.delivery_ledger().unwrap();
    assert_eq!(ledger.admissions.len(), admissions + 2);
    let device = f.owner.identity.native_cert.device_public_key;
    let receipt = |id: u8| {
        ledger.admissions[&tilo_relay::delivery_v3::commit::ReceiptKey {
            device,
            delivery: [id; 16],
        }]
            .committed
            .clone()
    };
    assert_eq!(receipt(1), receipt(2));
    assert!(
        !ledger
            .admissions
            .contains_key(&tilo_relay::delivery_v3::commit::ReceiptKey {
                device,
                delivery: [3; 16],
            })
    );
}

#[test]
fn a_duplicate_or_single_item_keeps_the_single_item_path() {
    let (mut f, budget, ingress) = attached_fixture();
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    let session = f.handshake(&mut registry, [122; 32]);
    let first = native_edit(&f.owner, "d1");
    send_batch(
        &mut f,
        &mut registry,
        session,
        &[(1, &first)],
        &budget,
        &ingress,
        None,
    )
    .unwrap();
    // The host signs its publications with the same native identity, so later
    // edits must build on the committed state.
    let mut edits = vec![first];
    edits.extend(native_edits(&f.owner, &["d2", "d3"]));
    let before = f.owner.status().generation;
    // [known op under a new id, new, new]: the duplicate commits alone, the
    // two new items then share one group.
    let admissions = send_batch(
        &mut f,
        &mut registry,
        session,
        &[(4, &edits[0]), (2, &edits[1]), (3, &edits[2])],
        &budget,
        &ingress,
        None,
    )
    .unwrap();
    assert!(matches!(
        admissions[0].outcome(),
        AdmissionOutcome::Duplicate { .. }
    ));
    assert_eq!(
        accepted_commit(&admissions[1]).0,
        accepted_commit(&admissions[2]).0
    );
    assert_eq!(f.owner.status().generation, before + 2);
}

/// Handshake a data session with explicit requested/allowed capabilities
/// against a host whose descriptor advertises the product contract.
fn product_fixture() -> (Fixture, HostBytes, Ingress) {
    use tilo_relay::delivery_v3::capabilities::PRODUCT_DATA_V1;
    let mut f = Fixture::new();
    let now = f.now;
    f.current
        .with_owner(&mut f.owner, |owner| {
            owner.install_delivery_transport(
                "https://localhost:9443",
                PRODUCT_DATA_V1,
                web_time::SystemTime::UNIX_EPOCH + web_time::Duration::from_nanos(now),
            )
        })
        .unwrap()
        .unwrap();
    let (ingress, _receiver) = crate::https::ingress_channel();
    let budget = ingress.host_bytes();
    f.current
        .with_owner(&mut f.owner, |owner| owner.attach_data_resources(&budget))
        .unwrap()
        .unwrap();
    (f, budget, ingress)
}
fn product_session(
    f: &mut Fixture,
    registry: &mut HttpSessions,
    budget: &HostBytes,
    requested: u64,
    client: tilo_relay::delivery_v3::auth::CapabilityPolicy,
    nonce: u8,
) -> (([u8; 16], [u8; 32]), u64) {
    use crate::owner::delivery::NativeDeliveryAuthority;
    use tilo_relay::delivery_v3::{
        auth, capabilities::PRODUCT_DATA_V1, descriptor::GenerationExpectation,
    };
    let nonce = [nonce; 32];
    let device = f.owner.identity.native_cert.device_public_key;
    let body = postcard::to_stdvec(&(3_u16, device, nonce, requested, PRODUCT_DATA_V1)).unwrap();
    let server = auth::ServerProof::decode(
        &registry
            .challenge(&f.owner, &f.current, &body, f.now)
            .unwrap(),
    )
    .unwrap();
    let at = web_time::SystemTime::UNIX_EPOCH + web_time::Duration::from_nanos(f.now);
    let proof = {
        let guard = f.current.read().unwrap();
        let descriptor = f
            .owner
            .verified_delivery_transport(guard.view(), at, PRODUCT_DATA_V1)
            .unwrap();
        let authority = f.owner.transport_authority(
            guard.peer(),
            GenerationExpectation::Current {
                generation: descriptor.generation(),
                descriptor_digest: descriptor.descriptor_digest(),
            },
            at,
            PRODUCT_DATA_V1,
        );
        let scope = descriptor
            .client_scope(
                &authority,
                &f.owner.identity.native_cert,
                auth::Carrier::Https,
                requested,
                PRODUCT_DATA_V1,
            )
            .unwrap();
        let native = NativeDeliveryAuthority {
            view: guard.view(),
            workspace: f.owner.workspace_id(),
            host: f.owner.host_certificate(),
        };
        let revocations = tilo_user_identity::RevocationList::new();
        auth::verify_server(
            &auth::ClientExpectations {
                scope,
                capabilities: client,
                credentials: auth::Credentials {
                    device: &f.owner.identity.native_cert,
                    host: f.owner.host_certificate(),
                    device_revocations: &revocations,
                    host_revocations: &revocations,
                },
                authority: &native,
                now_ns: f.now,
            },
            nonce,
            &server,
        )
        .unwrap()
        .client_proof(&f.owner.identity.native)
        .unwrap()
    };
    let ready = registry
        .finish_data_at(
            &mut f.owner,
            &f.current,
            budget,
            &proof.encode().unwrap(),
            f.now,
        )
        .unwrap();
    let wire: (u16, [u8; 16], [u8; 32], u64, u64) = postcard::from_bytes(&ready.bytes).unwrap();
    ((wire.1, wire.2), wire.4)
}
/// Decode a page into its groups' boundaries, checking group shape.
fn page_boundaries(page: &[u8]) -> Vec<tilo_relay::delivery_v3::state::CommitBoundary> {
    use tilo_relay::delivery_v3::state::{CommitBoundary, EventChunk};
    let mut boundaries = Vec::new();
    let mut group = None;
    let mut rest = page;
    while !rest.is_empty() {
        let (kind, payload, next) = framing::decode_frame_prefix(rest).unwrap();
        match kind {
            MessageType::EventChunk => {
                let chunk = EventChunk::decode(payload).unwrap();
                assert!(group.is_none_or(|g: CommitRef| g == *chunk.committed()));
                group = Some(chunk.committed().clone());
            }
            MessageType::CommitBoundary => {
                let boundary = CommitBoundary::decode(payload).unwrap();
                assert!(group.take().is_none_or(|g| g == *boundary.committed()));
                boundaries.push(boundary);
            }
            other => panic!("unexpected replay frame {other:?}"),
        }
        rest = next;
    }
    assert!(group.is_none(), "trailing partial group");
    boundaries
}
fn cursor_target(boundary: &tilo_relay::delivery_v3::state::CommitBoundary) -> String {
    format!(
        "/peer/v3/events?cursor={}&mode=poll",
        state::encode_cursor_url(boundary.cursor().as_bytes()).unwrap()
    )
}

#[test]
fn multi_group_pages_only_for_sessions_that_negotiated_the_extension() {
    use tilo_relay::delivery_v3::{
        auth::CapabilityPolicy,
        capabilities::{
            KNOWN_DATA, MULTI_GROUP_REPLAY_V1, PRODUCT_DATA_V1, client_allowed, handshake_policy,
        },
    };
    let (mut f, budget, ingress) = product_fixture();
    let mut registry =
        HttpSessions::new_for_policy(&f.owner, &f.current, f.now, PRODUCT_DATA_V1).unwrap();
    // Old client: requests exactly the product contract.
    let (old, old_caps) = product_session(
        &mut f,
        &mut registry,
        &budget,
        PRODUCT_DATA_V1,
        CapabilityPolicy {
            supported: PRODUCT_DATA_V1,
            allowed: PRODUCT_DATA_V1,
        },
        130,
    );
    assert_eq!(old_caps, PRODUCT_DATA_V1);
    // Commit several groups, one per request.
    for (i, key) in ["m1", "m2", "m3", "m4", "m5"].iter().enumerate() {
        let edit = native_edit(&f.owner, key);
        send_batch(
            &mut f,
            &mut registry,
            old,
            &[(i as u8 + 1, &edit)],
            &budget,
            &ingress,
            None,
        )
        .unwrap();
    }
    let groups = f.owner.delivery_ledger().unwrap().replay.len();
    assert!(groups >= 6);
    // Old client: every page carries exactly one group, walked one RTT each.
    let mut target = "/peer/v3/events?mode=poll".to_owned();
    let mut walked = Vec::new();
    while let Some(page) = poll(&f, &mut registry, old, &target, &budget, &ingress).unwrap() {
        let boundaries = page_boundaries(&page);
        assert_eq!(boundaries.len(), 1);
        target = cursor_target(&boundaries[0]);
        walked.push(boundaries[0].committed().clone());
    }
    assert_eq!(walked.len(), groups);
    // New client (a new device session replaces the old one on this device):
    // negotiates the extension and receives the whole suffix in one page.
    let (new, new_caps) = product_session(
        &mut f,
        &mut registry,
        &budget,
        handshake_policy(PRODUCT_DATA_V1),
        CapabilityPolicy {
            supported: KNOWN_DATA,
            allowed: client_allowed(PRODUCT_DATA_V1),
        },
        131,
    );
    assert_eq!(new_caps, PRODUCT_DATA_V1 | MULTI_GROUP_REPLAY_V1);
    let page = poll(
        &f,
        &mut registry,
        new,
        "/peer/v3/events?mode=poll",
        &budget,
        &ingress,
    )
    .unwrap()
    .unwrap();
    assert!(page.len() <= 1024 * 1024);
    let boundaries = page_boundaries(&page);
    let committed = boundaries
        .iter()
        .map(|b| b.committed().clone())
        .collect::<Vec<_>>();
    assert_eq!(committed, walked, "same groups, same order, one page");
    for boundary in &boundaries {
        let signed = boundary
            .cursor()
            .authenticate(&f.owner.identity.host.verifying_key())
            .unwrap();
        assert_eq!(signed.committed(), boundary.committed());
    }
    // From a middle cursor the rest arrives together; from the last, nothing.
    let page = poll(
        &f,
        &mut registry,
        new,
        &cursor_target(&boundaries[1]),
        &budget,
        &ingress,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        page_boundaries(&page)
            .iter()
            .map(|b| b.committed().clone())
            .collect::<Vec<_>>(),
        walked[2..]
    );
    assert!(
        poll(
            &f,
            &mut registry,
            new,
            &cursor_target(boundaries.last().unwrap()),
            &budget,
            &ingress
        )
        .unwrap()
        .is_none()
    );
}

/// Real installed State and poll cores over a long session: every turn's
/// measured allocation peak stays within its ingress reservation, and the
/// pool never refuses a write because of accumulated owner custody.
#[test]
fn long_session_state_and_replay_turns_stay_within_measured_reservations() {
    use crate::owner::allocations_only;
    let writes: usize = std::env::var("QOS_LONG_RUN_WRITES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100);
    let csv = std::env::var_os("QOS_LONG_RUN_CSV").is_some();
    let (mut f, budget, ingress) = attached_fixture();
    let initial = budget.available_bytes();
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    let session = f.handshake(&mut registry, [121; 32]);
    register_consumer(&mut f, &mut registry, session, &budget);
    for index in 0..writes {
        let cursor = current_tail_cursor(&f, &mut registry, session, &budget);
        let current = f.owner.aggregate_encoded_len();
        // State: reservation observed while the admitted turn is held.
        let canonical = native_edit(&f.owner, &format!("long-session-{index}"));
        let mut id = [0u8; 16];
        id[..8].copy_from_slice(&(index as u64 + 1).to_le_bytes());
        let item = StateItemChunk::new(
            id,
            *blake3::hash(&canonical).as_bytes(),
            canonical.len() as u64,
            0,
            &canonical,
        )
        .unwrap();
        let mut body = Vec::new();
        framing::append_frame(
            &mut body,
            MessageType::StateItemChunk,
            &item.encode().unwrap(),
            1024 * 1024,
        )
        .unwrap();
        let request = Request::new("POST", "/peer/v3/state", &body).unwrap();
        let proof = RequestProof::sign(
            session.0,
            session.1,
            &request,
            f.now,
            f.now + 30_000_000_000,
            &f.owner.identity.native,
        )
        .unwrap();
        let (original, _client) = charged_request(&ingress, &request, &proof);
        let before = budget.available_bytes();
        let (admitted, mut checker, turn) = registry
            .admit_revalidating_at(
                &f.owner,
                &f.current,
                &budget,
                OwnerDataOperation::State {
                    input_len: canonical.len(),
                },
                &proof,
                &request,
                &original,
                &TestClock::new(f.now),
            )
            .unwrap_or_else(|e| panic!("write {index} refused: {e:?}"));
        let reserved = before - budget.available_bytes();
        let (response, stats) = allocations_only(|| {
            admit_state_core(
                &mut f.owner,
                &f.current,
                &admitted,
                |owner, view, session| checker.check(owner, view, session),
                &budget,
                turn,
            )
        });
        response.unwrap();
        drop(admitted);
        let peak = usize::try_from(stats.peak).unwrap_or(0);
        assert!(
            peak <= reserved,
            "write {index}: peak {peak} > reserved {reserved}"
        );
        // Poll: replay of exactly the new group under a Replay turn.
        let target = format!(
            "/peer/v3/events?cursor={}&mode=poll",
            state::encode_cursor_url(cursor.as_bytes()).unwrap()
        );
        let request = Request::new("GET", &target, b"").unwrap();
        let proof = RequestProof::sign(
            session.0,
            session.1,
            &request,
            f.now,
            f.now + 30_000_000_000,
            &f.owner.identity.native,
        )
        .unwrap();
        let (original, _client) = charged_request(&ingress, &request, &proof);
        let before = budget.available_bytes();
        let (admitted, mut checker, turn) = registry
            .admit_revalidating_at(
                &f.owner,
                &f.current,
                &budget,
                OwnerDataOperation::Replay,
                &proof,
                &request,
                &original,
                &TestClock::new(f.now),
            )
            .unwrap_or_else(|e| panic!("poll {index} refused: {e:?}"));
        let replay_reserved = before - budget.available_bytes();
        let (page, stats) = allocations_only(|| {
            replay_core(
                &f.owner,
                &f.current,
                &admitted,
                |owner, view, session| checker.check(owner, view, session),
                turn,
            )
        });
        let page = page.unwrap().expect("new group page");
        let replay_peak = usize::try_from(stats.peak).unwrap_or(0);
        assert!(
            replay_peak <= replay_reserved,
            "poll {index}: peak {replay_peak} > reserved {replay_reserved}"
        );
        if csv {
            eprintln!(
                "long-session-csv,{index},{current},{reserved},{peak},{replay_reserved},{replay_peak},{},{}",
                page.len(),
                initial - budget.available_bytes()
            );
        }
    }
    let actual = f.owner.actual_retained_bytes();
    let used = initial - budget.available_bytes();
    eprintln!(
        "long-session done accounted={} actual={actual} pool_used={used}",
        f.owner.accounted_retained_bytes()
    );
    assert_eq!(f.owner.accounted_retained_bytes(), actual);
    assert!(
        used <= actual + 4 * 1024 * 1024,
        "pool used {used} beyond owner custody {actual}"
    );
}
