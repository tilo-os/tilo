use super::*;
use crate::TransportOrigin;
use automerge::AutoCommit;
use automerge::ROOT;
use collab_types::CommitNamed as _;
use tilo_user_identity::DeviceCert;
use tilo_user_identity::RevocationList;

fn fixture() -> (
    StatePeer,
    StatePeer,
    SigningKey,
    SigningKey,
    DeviceCert,
    SystemTime,
) {
    let (mut governor, governor_key, key, cert, now) = crate::admission_tests::seeded();
    governor.select_admission_v5().unwrap();
    let mut member = StatePeer::new(
        governor.room(),
        governor.authority().clone(),
        vec![cert.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    member.select_admission_v5().unwrap();
    for message in governor
        .publication_mut()
        .bootstrap_messages(&governor_key, now)
        .unwrap()
    {
        member
            .receive_from_report(
                TransportOrigin::RelayDelta(governor_key.verifying_key().to_bytes()),
                message,
                now,
            )
            .unwrap();
    }
    (governor, member, governor_key, key, cert, now)
}
fn local_edit(
    peer: &mut StatePeer,
    key: &SigningKey,
    now: SystemTime,
    snapshot: bool,
    name: &str,
) -> NeighbourMsg {
    let mut doc = AutoCommit::load(&peer.document("test.app", "tab:1").unwrap()).unwrap();
    doc.set_actor(automerge::ActorId::from(
        key.verifying_key().to_bytes().to_vec(),
    ));
    let heads = doc.get_heads();
    doc.put(ROOT, name, name).unwrap();
    doc.commit_edit("fixture", "fixture");
    if !snapshot {
        return peer
            .publication_mut()
            .publish_incremental("test.app", "tab:1", doc.save_after(&heads), key, now)
            .unwrap();
    }
    let author = Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes());
    let seq = peer.core.documents[&Route {
        app: "test.app".into(),
        record: "tab:1".into(),
    }]
        .channel
        .last_seq_per_author
        .get(&author)
        .copied()
        .unwrap_or(0)
        .checked_add(1)
        .unwrap();
    let mut envelope = SignedOp::new_unsigned(
        DocumentMutation::Snapshot(doc.save()).encode().unwrap(),
        collab_types::document_artifact_id("test.app", peer.room(), "tab:1"),
        author,
        collab_types::document_attestation_id("test.app", peer.room(), "tab:1"),
        OpKind::Change,
        now,
        seq,
    );
    collab_types::sign_op(&mut envelope, key).unwrap();
    peer.admit_local_app_document("test.app", "tab:1", envelope.clone(), now)
        .unwrap();
    NeighbourMsg::AppDocument {
        peer: peer_handle(&key.verifying_key().to_bytes()),
        app_id: "test.app".into(),
        cluster_id: peer.room().into(),
        record_id: "tab:1".into(),
        envelope: Box::new(envelope),
    }
}
fn checkpoint(
    governor: &mut StatePeer,
    member: &mut StatePeer,
    key: &SigningKey,
    now: SystemTime,
    durable: bool,
) {
    let messages = if durable {
        let generation = crate::durability::entries(governor.core.room_doc.as_deref().unwrap())
            .unwrap()
            .values()
            .map(|receipt| receipt.generation)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .unwrap();
        governor
            .publication_mut()
            .durable_bootstrap_messages(key, now, generation)
            .unwrap()
    } else {
        governor
            .publication_mut()
            .bootstrap_messages(key, now)
            .unwrap()
    };
    for message in messages {
        member
            .receive_from_report(
                TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
                message,
                now,
            )
            .unwrap();
    }
}
#[test]
fn local_outbox_revoked_custody_survives_restore_but_signing_and_current_lookup_refuse() {
    let (mut governor, mut member, governor_key, key, cert, now) = fixture();
    let original = local_edit(&mut member, &key, now, true, "revoked");
    let original_bytes = postcard::to_allocvec(&original).unwrap();
    let later = now + Duration::from_secs(301);
    checkpoint(&mut governor, &mut member, &governor_key, later, false);
    let mut revocation = tilo_user_identity::RevocationEntry {
        user_public_key: cert.user_public_key,
        revoked_device_public_key: cert.device_public_key,
        revoked_at_ns: 1,
        reason: tilo_user_identity::RevocationReason::Compromised,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut revocation, &SigningKey::from_bytes(&[1; 32]))
        .unwrap();
    member
        .receive_from_report(
            TransportOrigin::UntrustedPresence(governor_key.verifying_key().to_bytes()),
            NeighbourMsg::IdentityHello {
                protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
                cert: Box::new(governor.authority().clone()),
                revocations: vec![revocation],
                successions: Vec::new(),
            },
            later,
        )
        .unwrap();
    let history = member.clone();
    let mut member = StatePeer::restore(
        &member.durable().unwrap(),
        member.room(),
        member.authority().clone(),
        vec![cert],
        RevocationList::new(),
        later,
    )
    .unwrap();
    assert!(
        member
            .outbox()
            .lookup_retained_local_document(&original, key.verifying_key().to_bytes())
            .unwrap()
            .is_some()
    );
    let NeighbourMsg::AppDocument { envelope, .. } = &original else {
        panic!()
    };
    assert!(
        member
            .lookup_exact_local_document("test.app", "tab:1", envelope, later)
            .is_err()
    );
    let before = member.durable().unwrap();
    assert!(
        member
            .outbox_mut()
            .reconcile_local_recovery_projection(
                std::slice::from_ref(&original),
                Some(&history),
                &[],
                &key,
                later,
            )
            .is_err()
    );
    assert_eq!(member.durable().unwrap(), before);
    assert_eq!(postcard::to_allocvec(&original).unwrap(), original_bytes);
    assert!(
        !member
            .outbox()
            .retained_local_document_retired(&original, key.verifying_key().to_bytes())
            .unwrap()
    );
}

#[test]
fn local_outbox_structural_lookup_rejects_conflicting_unknown_and_cross_scope_evidence() {
    let (_, mut member, governor_key, key, _, now) = fixture();
    let original = local_edit(&mut member, &key, now, false, "identity");
    let before = member.durable().unwrap();
    assert!(
        member
            .outbox()
            .lookup_retained_local_document(&original, governor_key.verifying_key().to_bytes())
            .is_err()
    );
    for field in ["room", "record", "peer", "signature"] {
        let mut wrong = original.clone();
        let NeighbourMsg::AppDocument {
            peer,
            cluster_id,
            record_id,
            envelope,
            ..
        } = &mut wrong
        else {
            panic!()
        };
        match field {
            "room" => *cluster_id = "other-room".into(),
            "record" => *record_id = "other-record".into(),
            "peer" => *peer = peer.wrapping_add(1),
            _ => envelope.payload.push(1),
        }
        assert!(
            member
                .outbox()
                .lookup_retained_local_document(&wrong, key.verifying_key().to_bytes())
                .is_err(),
            "{field}"
        );
        assert!(
            member
                .outbox()
                .retained_local_document_retired(&wrong, key.verifying_key().to_bytes())
                .is_err(),
            "{field}"
        );
    }
    let mut unrecorded = original.clone();
    let NeighbourMsg::AppDocument { envelope, .. } = &mut unrecorded else {
        panic!()
    };
    envelope.seq += 100;
    collab_types::sign_op(envelope, &key).unwrap();
    assert_eq!(
        member
            .outbox()
            .lookup_retained_local_document(&unrecorded, key.verifying_key().to_bytes())
            .unwrap(),
        None
    );
    assert!(
        member
            .outbox()
            .retained_local_document_retired(&unrecorded, key.verifying_key().to_bytes())
            .is_err()
    );
    let mut conflict = original;
    let NeighbourMsg::AppDocument { envelope, .. } = &mut conflict else {
        panic!()
    };
    envelope.timestamp += Duration::from_secs(1);
    collab_types::sign_op(envelope, &key).unwrap();
    assert_eq!(
        member
            .outbox()
            .lookup_retained_local_document(&conflict, key.verifying_key().to_bytes()),
        Err(AdmissionError::ConflictingSequence)
    );
    assert_eq!(member.durable().unwrap(), before);
}

#[test]
fn local_outbox_watermark_and_half_checkpoint_never_acknowledge_missing_changes() {
    let (mut governor, mut member, governor_key, key, _, now) = fixture();
    let original = local_edit(&mut member, &key, now, true, "same-value");
    let mut unrelated = AutoCommit::load(&governor.document("test.app", "tab:1").unwrap()).unwrap();
    unrelated.set_actor(automerge::ActorId::from(vec![99; 32]));
    let before = unrelated.get_heads();
    unrelated.put(ROOT, "same-value", "same-value").unwrap();
    unrelated.commit_edit("fixture", "same-value");
    let second = member
        .publication_mut()
        .publish_incremental(
            "test.app",
            "tab:1",
            unrelated.save_after(&before),
            &key,
            now,
        )
        .unwrap();
    governor
        .receive_from_report(
            TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
            second.clone(),
            now,
        )
        .unwrap();
    let later = now + Duration::from_secs(301);
    let pair = governor
        .publication_mut()
        .durable_bootstrap_messages(&governor_key, later, 7)
        .unwrap();
    let mut half = member.clone();
    half.receive_from_report(
        TransportOrigin::RelayDelta(governor_key.verifying_key().to_bytes()),
        pair[0].clone(),
        later,
    )
    .unwrap();
    assert!(
        !half
            .outbox()
            .retained_local_document_retired(&original, key.verifying_key().to_bytes())
            .unwrap()
    );
    for message in pair {
        member
            .receive_from_report(
                TransportOrigin::RelayDelta(governor_key.verifying_key().to_bytes()),
                message,
                later,
            )
            .unwrap();
    }
    assert!(member.checkpoint_sequence_covers(&original));
    assert!(
        member
            .durable_coverage("test.app", "tab:1")
            .unwrap()
            .is_some()
    );
    assert!(
        !member
            .outbox()
            .retained_local_document_retired(&original, key.verifying_key().to_bytes())
            .unwrap(),
        "same visible text and a high author watermark do not cover missing change identity"
    );
    let renewed = member
        .outbox_mut()
        .reconcile_local_recovery_projection(&[original.clone(), second], None, &[], &key, later)
        .unwrap();
    assert!(!renewed.is_empty());
    for message in &renewed {
        governor
            .receive_from_report(
                TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
                message.clone(),
                later,
            )
            .unwrap();
    }
    checkpoint(&mut governor, &mut member, &governor_key, later, true);
    assert!(
        member
            .outbox()
            .retained_local_document_retired(&original, key.verifying_key().to_bytes())
            .unwrap()
    );
}

#[test]
fn local_outbox_builds_each_pending_publication_history_once() {
    let (_, mut member, _, key, _, now) = fixture();
    let selected = key.verifying_key().to_bytes();
    let mut originals = Vec::new();
    for edit in 0..6 {
        originals.push(local_edit(
            &mut member,
            &key,
            now,
            false,
            &format!("edit-{edit}"),
        ));
        let before = super::history::builds();
        let rebuilt = super::history::rebuilds();
        let reserved = member
            .outbox()
            .reserve_local_recovery(&originals, None, selected)
            .unwrap();
        assert_eq!(reserved.entries(), 1, "one route");
        member
            .outbox()
            .validate_local_recovery_projection(&originals, None, &[], selected)
            .unwrap();
        assert_eq!(
            member
                .outbox()
                .reserve_local_recovery(&originals, None, selected)
                .unwrap(),
            reserved
        );
        assert_eq!(
            super::history::builds().saturating_sub(before),
            1,
            "edit {edit}: only the new publication's history is built"
        );
        if edit > 0 {
            assert_eq!(
                super::history::rebuilds(),
                rebuilt,
                "edit {edit}: the new history extends the previous edit's"
            );
        }
        super::history::forget();
        assert_eq!(
            member
                .outbox()
                .reserve_local_recovery(&originals, None, selected)
                .unwrap(),
            reserved
        );
    }
}

#[path = "projection_tests.rs"]
mod projection_tests;
