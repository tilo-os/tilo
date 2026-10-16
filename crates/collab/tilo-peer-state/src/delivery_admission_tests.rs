use super::*;
use crate::{AdmissionError, InputDisposition, MissingPrerequisite};

fn incremental(
    author: &mut StatePeer,
    key: &SigningKey,
    now: SystemTime,
    name: &str,
) -> NeighbourMsg {
    let mut doc = AutoCommit::load(&author.document("test.app", "tab:1").unwrap()).unwrap();
    doc.set_actor(automerge::ActorId::from(
        key.verifying_key().to_bytes().to_vec(),
    ));
    let heads = doc.get_heads();
    doc.put(ROOT, name, name).unwrap();
    doc.commit_edit("fixture", "fixture");
    author
        .publish_incremental("test.app", "tab:1", doc.save_after(&heads), key, now)
        .unwrap()
}
fn restore(peer: &StatePeer, now: SystemTime) -> StatePeer {
    StatePeer::restore(
        &peer.durable().unwrap(),
        peer.room(),
        peer.authority().clone(),
        vec![],
        RevocationList::new(),
        now,
    )
    .unwrap()
}

#[test]
fn delivery_admission_selection_is_explicit_and_exact_retry_survives_restore() {
    let (mut receiver, _, key, _, now) = seeded();
    let mut author = receiver.clone();
    let message = incremental(&mut author, &key, now, "A");
    let origin = TransportOrigin::RelayDelta(key.verifying_key().to_bytes());
    let before = receiver.durable().unwrap();
    assert_eq!(before[0], 4);
    assert!(matches!(
        receiver.receive_from_report(origin, message.clone(), now),
        Err(AdmissionError::WriterSelectionRequired)
    ));
    assert_eq!(receiver.durable().unwrap(), before);
    receiver.select_admission_v5().unwrap();
    let report = receiver
        .receive_from_report(origin, message.clone(), now)
        .unwrap();
    let InputDisposition::Accepted(first) = report.primary() else {
        panic!("not accepted: {:?}", report.primary())
    };
    let first = first.clone();
    assert_eq!(report.settled(), std::slice::from_ref(&first));
    let encoded = receiver.durable().unwrap();
    assert_eq!(encoded[0], 5);
    let rows = receiver.rows();
    let later = now + Duration::from_secs(301);
    let mut receiver = restore(&receiver, later);
    assert_eq!(receiver.durable().unwrap()[0], 5);
    assert_eq!(receiver.rows(), rows);
    let duplicate = receiver
        .receive_from_report(origin, message, later)
        .unwrap();
    assert_eq!(duplicate.primary(), &InputDisposition::Duplicate(first));
    assert!(duplicate.settled().is_empty());
    assert_eq!(receiver.rows(), rows);
}

#[test]
fn delivery_admission_same_sequence_changed_body_conflicts_unknown_floor_does_not_duplicate() {
    let (mut receiver, _, key, _, now) = seeded();
    let mut author = receiver.clone();
    let message = incremental(&mut author, &key, now, "A");
    let origin = TransportOrigin::RelayDelta(key.verifying_key().to_bytes());
    let mut legacy = receiver.clone();
    legacy.receive_from(origin, message.clone(), now).unwrap();
    legacy = restore(&legacy, now);
    legacy.select_admission_v5().unwrap();
    let unknown = legacy
        .receive_from_report(origin, message.clone(), now)
        .unwrap();
    assert_eq!(
        unknown.primary(),
        &InputDisposition::RetryLater(MissingPrerequisite::ExactHistoricalEvidence)
    );
    receiver.select_admission_v5().unwrap();
    receiver
        .receive_from_report(origin, message.clone(), now)
        .unwrap();
    let before = receiver.durable().unwrap();
    let mut changed = message.clone();
    if let NeighbourMsg::AppDocument { envelope, .. } = &mut changed {
        envelope.timestamp += Duration::from_secs(1);
        collab_types::sign_op(envelope, &key).unwrap();
    }
    assert!(matches!(
        receiver.receive_from_report(origin, changed.clone(), now),
        Err(AdmissionError::ConflictingSequence)
    ));
    assert_eq!(receiver.durable().unwrap(), before);
    // Compatibility: legacy receive keeps its old floor/duplicate semantics.
    assert!(
        receiver
            .receive_from(origin, changed, now)
            .unwrap()
            .duplicate
    );
    let mut different_payload = incremental(&mut author, &key, now, "B");
    if let (
        NeighbourMsg::AppDocument {
            envelope: original, ..
        },
        NeighbourMsg::AppDocument {
            envelope: changed, ..
        },
    ) = (&message, &mut different_payload)
    {
        changed.seq = original.seq;
        collab_types::sign_op(changed, &key).unwrap();
    }
    assert!(matches!(
        receiver.receive_from_report(origin, different_payload.clone(), now),
        Err(AdmissionError::ConflictingSequence)
    ));
    assert_eq!(
        legacy
            .receive_from_report(origin, different_payload, now)
            .unwrap()
            .primary(),
        &InputDisposition::RetryLater(MissingPrerequisite::ExactHistoricalEvidence)
    );
}

#[test]
fn delivery_admission_new_same_heads_envelope_is_accepted() {
    let (mut receiver, _, key, _, now) = seeded();
    let mut author = receiver.clone();
    let first = incremental(&mut author, &key, now, "A");
    let mut second = first.clone();
    if let NeighbourMsg::AppDocument { envelope, .. } = &mut second {
        envelope.seq += 1;
        collab_types::sign_op(envelope, &key).unwrap();
    }
    let origin = TransportOrigin::RelayDelta(key.verifying_key().to_bytes());
    receiver.select_admission_v5().unwrap();
    let first = receiver.receive_from_report(origin, first, now).unwrap();
    let rows = receiver.rows();
    let second = receiver.receive_from_report(origin, second, now).unwrap();
    assert!(matches!(second.primary(), InputDisposition::Accepted(_)));
    assert_ne!(first.primary(), second.primary());
    assert_eq!(rows, receiver.rows());
    assert!(!second.summary().changed);
    assert!(second.summary().metadata_changed);
}

#[test]
fn delivery_admission_primary_is_separate_from_pending_drain() {
    let (mut receiver, _, key, _, now) = seeded();
    let mut author = receiver.clone();
    let first = incremental(&mut author, &key, now, "A");
    let second = incremental(&mut author, &key, now, "B");
    let origin = TransportOrigin::RelayDelta(key.verifying_key().to_bytes());
    receiver.select_admission_v5().unwrap();
    let deferred = receiver
        .receive_from_report(origin, second.clone(), now)
        .unwrap();
    assert_eq!(
        deferred.primary(),
        &InputDisposition::RetryLater(MissingPrerequisite::CausalDependencies)
    );
    assert!(deferred.settled().is_empty());
    receiver = restore(&receiver, now);
    let absent_key = SigningKey::from_bytes(&[9; 32]);
    let mut unrelated = second.clone();
    if let NeighbourMsg::AppDocument { peer, envelope, .. } = &mut unrelated {
        *peer = peer_handle(&absent_key.verifying_key().to_bytes());
        envelope.author =
            collab_types::Ed25519PublicKey::from_bytes(absent_key.verifying_key().to_bytes());
        collab_types::sign_op(envelope, &absent_key).unwrap();
    }
    let waiting = receiver
        .receive_from_report(
            TransportOrigin::RelayDelta(absent_key.verifying_key().to_bytes()),
            unrelated,
            now,
        )
        .unwrap();
    assert_eq!(
        waiting.primary(),
        &InputDisposition::RetryLater(MissingPrerequisite::Certificate)
    );
    let admitted = receiver.receive_from_report(origin, first, now).unwrap();
    assert!(matches!(admitted.primary(), InputDisposition::Accepted(_)));
    assert_eq!(admitted.settled().len(), 2);
    assert!(
        admitted.summary().deferred,
        "unrelated pending item remains separate from accepted primary"
    );
    assert_eq!(receiver.pending_count(), 1);
    assert!(matches!(
        receiver
            .receive_from_report(origin, second, now)
            .unwrap()
            .primary(),
        InputDisposition::Duplicate(_)
    ));
}

#[test]
fn delivery_admission_metadata_and_unsupported_families_have_no_operation_receipt() {
    let (mut receiver, key, _, _, now) = seeded();
    receiver.select_admission_v5().unwrap();
    let origin = TransportOrigin::RelayDelta(key.verifying_key().to_bytes());
    let message = receiver.room_message();
    let report = receiver.receive_from_report(origin, message, now).unwrap();
    assert_eq!(report.primary(), &InputDisposition::MetadataOnly);
    let request = NeighbourMsg::SyncReq {
        since: vec![],
        room: receiver.room().into(),
        invite_id: None,
        nonce: 0,
    };
    let before = receiver.durable().unwrap();
    assert_eq!(
        receiver
            .receive_from_report(origin, request, now)
            .unwrap()
            .primary(),
        &InputDisposition::UnsupportedFamily
    );
    assert_eq!(receiver.durable().unwrap(), before);
}
#[test]
fn delivery_admission_exact_retry_rechecks_signature_route_and_current_revocation() {
    let (mut receiver, governor, key, _, now) = seeded();
    let mut author = receiver.clone();
    let message = incremental(&mut author, &key, now, "A");
    let origin = TransportOrigin::RelayDelta(key.verifying_key().to_bytes());
    receiver.select_admission_v5().unwrap();
    receiver
        .receive_from_report(origin, message.clone(), now)
        .unwrap();
    let mut forged = message.clone();
    if let NeighbourMsg::AppDocument { envelope, .. } = &mut forged {
        envelope.signature = collab_types::Signature::from_bytes([0; 64]);
    }
    assert!(
        receiver
            .lookup_exact_document(origin, &forged, now)
            .is_err()
    );
    let mut wrong_route = message.clone();
    if let NeighbourMsg::AppDocument { record_id, .. } = &mut wrong_route {
        *record_id = "tab:2".into();
    }
    assert!(
        receiver
            .lookup_exact_document(origin, &wrong_route, now)
            .is_err()
    );
    assert!(
        receiver
            .lookup_exact_document(
                TransportOrigin::UntrustedPresence(key.verifying_key().to_bytes()),
                &message,
                now
            )
            .is_err()
    );
    let root = SigningKey::from_bytes(&[1; 32]);
    let mut revocation = tilo_user_identity::RevocationEntry {
        user_public_key: root.verifying_key().to_bytes(),
        revoked_device_public_key: key.verifying_key().to_bytes(),
        revoked_at_ns: 1,
        reason: tilo_user_identity::RevocationReason::Lost,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut revocation, &root).unwrap();
    receiver
        .governance_mut()
        .publish_governed_revocations(&[revocation], &governor, now)
        .unwrap();
    let later = now + Duration::from_secs(301);
    assert!(
        receiver
            .receive_from_report(origin, message, later)
            .is_err()
    );
}

#[test]
fn delivery_admission_local_candidate_delta_includes_all_routes_and_refuses_evidence_loss() {
    let (mut base, key, _, _, now) = seeded();
    base.select_admission_v5().unwrap();
    let mut candidate = base.clone();
    let bytes = candidate.document("test.app", "tab:1").unwrap();
    candidate
        .publish_document("test.app", "tab:1", bytes.clone(), &key, now)
        .unwrap();
    candidate
        .publish_document("test.app", "tab:2", bytes, &key, now)
        .unwrap();
    let delta = candidate.admissions_since(&base).unwrap();
    assert_eq!(delta.len(), 2);
    assert_eq!(
        delta
            .iter()
            .map(super::super::admission::DocumentAdmission::record)
            .collect::<Vec<_>>(),
        ["tab:1", "tab:2"]
    );
    assert!(base.admissions_since(&candidate).is_err());
    let restored = restore(&candidate, now);
    assert_eq!(restored.admissions_since(&base).unwrap(), delta);
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end scenario: its setup and assertions stay together"
)]
fn delivery_admission_literal_signed_bytes_and_v4_v5_migration_remain_exact() {
    static V4: std::sync::LazyLock<Vec<u8>> = std::sync::LazyLock::new(|| {
        include_str!("../tests/fixtures/delivery-admission/native-v4.postcard.hex")
            .split_ascii_whitespace()
            .map(|byte| {
                u8::from_str_radix(byte, 16).expect("checked-in fixture contains hexadecimal bytes")
            })
            .collect()
    });
    static V5: std::sync::LazyLock<Vec<u8>> = std::sync::LazyLock::new(|| {
        include_str!("../tests/fixtures/delivery-admission/native-v5.postcard.hex")
            .split_ascii_whitespace()
            .map(|byte| {
                u8::from_str_radix(byte, 16).expect("checked-in fixture contains hexadecimal bytes")
            })
            .collect()
    });
    static OP: std::sync::LazyLock<Vec<u8>> = std::sync::LazyLock::new(|| {
        include_str!("../tests/fixtures/delivery-admission/original-signed-op.postcard.hex")
            .split_ascii_whitespace()
            .map(|byte| {
                u8::from_str_radix(byte, 16).expect("checked-in fixture contains hexadecimal bytes")
            })
            .collect()
    });
    static ITEM: std::sync::LazyLock<Vec<u8>> = std::sync::LazyLock::new(|| {
        include_str!("../tests/fixtures/delivery-admission/original-app-document.postcard.hex")
            .split_ascii_whitespace()
            .map(|byte| {
                u8::from_str_radix(byte, 16).expect("checked-in fixture contains hexadecimal bytes")
            })
            .collect()
    });
    const ID: &str = include_str!("../tests/fixtures/delivery-admission/operation-id.hex");
    let (_, authority, key, _, now) = fixtures();
    let room = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let mut old = StatePeer::restore(
        V4.as_slice(),
        room,
        authority.clone(),
        vec![],
        RevocationList::new(),
        now,
    )
    .unwrap();
    assert!(!old.admission_v5_selected());
    let rewritten = old.durable().unwrap();
    if rewritten != V4.as_slice() {
        // Durable state re-saves its Automerge documents, whose compressed
        // columns depend on the flate2 backend unified into this build (a
        // workspace build selects zlib-rs; this crate alone uses miniz_oxide).
        // Without selection the rewrite must still carry the identical rows,
        // governance and writer, and be a fixed point under this backend.
        let reread = StatePeer::restore(
            &rewritten,
            room,
            authority.clone(),
            vec![],
            RevocationList::new(),
            now,
        )
        .unwrap();
        assert!(!reread.admission_v5_selected());
        assert_eq!(reread.rows(), old.rows(), "frozen v4 rows changed");
        assert_eq!(
            reread.admitted_room().map(|room| room.clone().get_heads()),
            old.admitted_room().map(|room| room.clone().get_heads()),
            "frozen v4 governance changed"
        );
        assert_eq!(
            reread.durable().unwrap(),
            rewritten,
            "frozen v4 bytes changed without selection"
        );
    }
    let item: NeighbourMsg =
        collab_types::canonical::from_canonical_bytes(ITEM.as_slice()).unwrap();
    assert_eq!(postcard::to_allocvec(&item).unwrap(), ITEM.as_slice());
    let NeighbourMsg::AppDocument { envelope, .. } = &item else {
        panic!("unexpected fixture shape")
    };
    assert_eq!(
        collab_types::canonical_bytes(envelope.as_ref()).unwrap(),
        OP.as_slice()
    );
    collab_types::verify_op(envelope.as_ref()).unwrap();
    assert_eq!(blake3::hash(OP.as_slice()).to_hex().as_str(), ID.trim());
    old.select_admission_v5().unwrap();
    let origin = TransportOrigin::RelayDelta(key.verifying_key().to_bytes());
    let report = old.receive_from_report(origin, item.clone(), now).unwrap();
    let InputDisposition::Accepted(admitted) = report.primary() else {
        panic!("fixture rejected")
    };
    assert_eq!(admitted.sequence(), 7);
    assert_eq!(hex::encode(admitted.operation_id()), ID.trim());
    let mut restored = StatePeer::restore(
        V5.as_slice(),
        room,
        authority.clone(),
        vec![],
        RevocationList::new(),
        now,
    )
    .unwrap();
    assert!(restored.admission_v5_selected());
    assert_eq!(
        restored
            .receive_from_report(origin, item, now)
            .unwrap()
            .primary(),
        &InputDisposition::Duplicate(admitted.clone())
    );
    for cut in [0, 1, 2, 3, V5.as_slice().len() / 2, V5.as_slice().len() - 1] {
        assert!(
            StatePeer::restore(
                &V5.as_slice()[..cut],
                room,
                authority.clone(),
                vec![],
                RevocationList::new(),
                now
            )
            .is_err()
        );
    }
    let mut trailing = V5.as_slice().to_vec();
    trailing.push(0);
    assert!(
        StatePeer::restore(
            &trailing,
            room,
            authority,
            vec![],
            RevocationList::new(),
            now
        )
        .is_err()
    );
}

#[test]
fn delivery_admission_review_unindexed_retained_input_requires_live_freshness() {
    let (receiver, _, key, _, now) = seeded();
    let mut author = receiver.clone();
    let message = incremental(&mut author, &key, now, "old-unindexed");
    let source = key.verifying_key().to_bytes();
    let later = now + Duration::from_secs(301);
    // Legacy retained replay remains supported, independently of the strict API.
    let mut legacy = receiver.clone();
    assert!(
        legacy
            .receive_from(
                TransportOrigin::RetainedRelayDelta(source),
                message.clone(),
                later
            )
            .unwrap()
            .changed
    );
    for origin in [
        TransportOrigin::RelayDelta(source),
        TransportOrigin::RetainedRelayDelta(source),
    ] {
        let mut strict = receiver.clone();
        strict.select_admission_v5().unwrap();
        let rows = strict.rows();
        assert!(
            strict
                .receive_from_report(origin, message.clone(), later)
                .is_err(),
            "unindexed stale input admitted through {origin:?}"
        );
        assert_eq!(strict.rows(), rows);
        assert!(
            strict
                .lookup_exact_document(origin, &message, later)
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn delivery_admission_review_strict_drain_rechecks_retained_pending_freshness() {
    let (mut receiver, authority, key, _, now) = seeded();
    let mut author = receiver.clone();
    let _first = incremental(&mut author, &key, now, "A");
    let second = incremental(&mut author, &key, now, "B");
    let origin = TransportOrigin::RetainedRelayDelta(key.verifying_key().to_bytes());
    // Queue through the legacy retained path to exercise persisted provenance.
    assert!(
        receiver
            .receive_from(origin, second.clone(), now)
            .unwrap()
            .deferred
    );
    receiver = restore(&receiver, now);
    receiver.select_admission_v5().unwrap();
    let later = now + Duration::from_secs(301);
    let room = receiver.room_message();
    let report = receiver
        .receive_from_report(
            TransportOrigin::RelayDelta(authority.verifying_key().to_bytes()),
            room,
            later,
        )
        .unwrap();
    assert_eq!(
        report.summary().rejected,
        1,
        "expired retained pending entry was not rejected"
    );
    assert_eq!(receiver.pending_count(), 0);
    assert!(report.settled().is_empty());
    assert!(
        receiver
            .lookup_exact_document(origin, &second, later)
            .unwrap()
            .is_none()
    );
}
