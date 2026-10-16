use super::*;
use crate::InputDisposition;
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
    for message in governor.bootstrap_messages(&governor_key, now).unwrap() {
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
            .durable_bootstrap_messages(key, now, generation)
            .unwrap()
    } else {
        governor.bootstrap_messages(key, now).unwrap()
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
fn local_outbox_snapshot_and_expired_incremental_recover_without_rewriting_original() {
    for snapshot in [false, true] {
        let (mut governor, mut member, governor_key, key, cert, now) = fixture();
        let original = local_edit(&mut member, &key, now, snapshot, "offline");
        let bytes = postcard::to_allocvec(&original).unwrap();
        let later = now + Duration::from_secs(301);
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
        assert_eq!(
            member
                .outbox_mut()
                .reconcile_local_outbox(std::slice::from_ref(&original), &key, later)
                .unwrap(),
            vec![original.clone()],
            "reopen is not a fresh checkpoint"
        );
        checkpoint(&mut governor, &mut member, &governor_key, later, false);
        let recovered = member
            .outbox_mut()
            .reconcile_local_outbox(std::slice::from_ref(&original), &key, later)
            .unwrap();
        assert_eq!(recovered.len(), 2);
        assert_eq!(postcard::to_allocvec(&recovered[0]).unwrap(), bytes);
        assert_eq!(
            member
                .outbox_mut()
                .reconcile_local_outbox(&recovered, &key, later)
                .unwrap(),
            recovered,
            "fresh actual successor deduplicates"
        );
        let NeighbourMsg::AppDocument { envelope, .. } = &recovered[1] else {
            panic!()
        };
        assert!(matches!(
            DocumentMutation::decode(&envelope.payload).unwrap(),
            DocumentMutation::Incremental(_)
        ));
        let origin = TransportOrigin::RelayDelta(key.verifying_key().to_bytes());
        assert!(matches!(
            governor
                .receive_from_report(origin, recovered[1].clone(), later)
                .unwrap()
                .primary(),
            InputDisposition::Accepted(_)
        ));
        assert!(matches!(
            governor
                .receive_from_report(origin, recovered[1].clone(), later)
                .unwrap()
                .primary(),
            InputDisposition::Duplicate(_)
        ));
        assert!(
            !member
                .outbox()
                .retained_local_document_retired(&original, key.verifying_key().to_bytes())
                .unwrap()
        );
        checkpoint(&mut governor, &mut member, &governor_key, later, false);
        assert!(
            !member
                .outbox()
                .retained_local_document_retired(&original, key.verifying_key().to_bytes())
                .unwrap(),
            "receiptless causal coverage is not acknowledgement"
        );
        checkpoint(&mut governor, &mut member, &governor_key, later, true);
        assert!(
            member
                .outbox()
                .retained_local_document_retired(&original, key.verifying_key().to_bytes())
                .unwrap()
        );
        assert_eq!(
            member
                .outbox_mut()
                .reconcile_local_outbox(&recovered, &key, later)
                .unwrap(),
            recovered,
            "reconciliation never deletes originals"
        );
        assert_eq!(member.rows(), governor.rows());
    }
}
#[test]
fn local_outbox_legacy_history_recovers_real_changes_without_original_envelopes() {
    let (mut governor, mut member, governor_key, key, _, now) = fixture();
    let _missing_original = local_edit(&mut member, &key, now, true, "legacy");
    let history = member.clone();
    let raw = history.durable().unwrap();
    let later = now + Duration::from_secs(301);
    assert!(
        !member
            .outbox()
            .retained_local_history_retired(&history)
            .unwrap()
    );
    checkpoint(&mut governor, &mut member, &governor_key, later, false);
    let messages = member
        .outbox_mut()
        .recover_local_history(&history, &key, later)
        .unwrap();
    assert_eq!(messages.len(), 1);
    for message in messages {
        governor
            .receive_from_report(
                TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
                message,
                later,
            )
            .unwrap();
    }
    checkpoint(&mut governor, &mut member, &governor_key, later, true);
    assert!(
        member
            .outbox()
            .retained_local_history_retired(&history)
            .unwrap()
    );
    assert_eq!(history.durable().unwrap(), raw);
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
                cert: Box::new(governor.authority().clone()),
                revocations: vec![revocation],
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
            .reconcile_local_outbox(std::slice::from_ref(&original), &key, later)
            .is_err()
    );
    assert!(
        member
            .outbox_mut()
            .recover_local_history(&history, &key, later)
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
    assert_eq!(
        half.outbox_mut()
            .reconcile_local_outbox(std::slice::from_ref(&original), &key, later)
            .unwrap(),
        vec![original.clone()],
        "governance half is not a fresh matching checkpoint"
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
        .reconcile_local_outbox(&[original.clone(), second], &key, later)
        .unwrap();
    assert!(renewed.len() > 2);
    for message in &renewed[2..] {
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
fn local_outbox_late_original_and_distinct_renewal_converge_in_both_delivery_orders() {
    for delivered in [false, true] {
        let (mut governor, mut member, governor_key, key, _, now) = fixture();
        let original = local_edit(&mut member, &key, now, false, "late");
        let origin = TransportOrigin::RelayDelta(key.verifying_key().to_bytes());
        if delivered {
            governor
                .receive_from_report(origin, original.clone(), now)
                .unwrap();
        }
        let later = now + Duration::from_secs(301);
        checkpoint(&mut governor, &mut member, &governor_key, later, false);
        let recovered = member
            .outbox_mut()
            .reconcile_local_outbox(std::slice::from_ref(&original), &key, later)
            .unwrap();
        assert_eq!(recovered.len(), 2);
        governor
            .receive_from_report(origin, recovered[1].clone(), later)
            .unwrap();
        let late = governor.receive_from_report(origin, original.clone(), later);
        if delivered {
            assert!(matches!(
                late.unwrap().primary(),
                InputDisposition::Duplicate(_)
            ));
        } else {
            assert!(
                late.is_err(),
                "new historical envelope is still rejected by strict remote admission"
            );
        }
        checkpoint(&mut governor, &mut member, &governor_key, later, true);
        assert!(
            member
                .outbox()
                .retained_local_document_retired(&original, key.verifying_key().to_bytes())
                .unwrap()
        );
        assert_eq!(member.rows(), governor.rows());
    }
}

#[test]
fn local_outbox_expired_predecessor_and_fresh_dependent_keep_prefix_until_real_ack() {
    let (mut governor, mut member, governor_key, key, _, now) = fixture();
    let first = local_edit(&mut member, &key, now, false, "first");
    let second = local_edit(
        &mut member,
        &key,
        now + Duration::from_secs(290),
        false,
        "dependent",
    );
    let originals = vec![first, second];
    let original_bytes = postcard::to_allocvec(&originals).unwrap();
    let later = now + Duration::from_secs(301);
    checkpoint(&mut governor, &mut member, &governor_key, later, false);
    let mut retained = member
        .outbox_mut()
        .reconcile_local_outbox(&originals, &key, later)
        .unwrap();
    assert_eq!(retained.len(), 3);
    let mut sent = std::collections::BTreeSet::new();
    for _ in 0..4 {
        let batch = crate::outbox::publication_batch(&retained, later)
            .cloned()
            .collect::<Vec<_>>();
        for message in batch {
            if !sent.insert(postcard::to_allocvec(&message).unwrap()) {
                continue;
            }
            let report = governor
                .receive_from_report(
                    TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
                    message,
                    later,
                )
                .unwrap();
            if report.changed {
                checkpoint(&mut governor, &mut member, &governor_key, later, true);
                retained = member
                    .outbox_mut()
                    .reconcile_local_outbox(&retained, &key, later)
                    .unwrap();
            }
        }
    }
    assert_eq!(
        postcard::to_allocvec(&retained[..2]).unwrap(),
        original_bytes
    );
    assert!(originals.iter().all(|original| {
        member
            .outbox()
            .retained_local_document_retired(original, key.verifying_key().to_bytes())
            .unwrap()
    }));
    assert_eq!(member.rows(), governor.rows());
}

#[test]
fn local_outbox_successor_dedup_uses_real_change_hashes_across_native_encodings() {
    let (mut governor, mut member, governor_key, key, _, now) = fixture();
    let original = local_edit(&mut member, &key, now, true, "equivalent");
    let later = now + Duration::from_secs(301);
    checkpoint(&mut governor, &mut member, &governor_key, later, false);
    let mut doc = AutoCommit::load(&member.document("test.app", "tab:1").unwrap()).unwrap();
    let changes = doc
        .get_changes(&[])
        .iter()
        .flat_map(|change| change.raw_bytes().to_vec())
        .collect::<Vec<_>>();
    let successor = member
        .publish_incremental("test.app", "tab:1", changes, &key, later)
        .unwrap();
    let retained = vec![original, successor];
    assert_eq!(
        member
            .outbox_mut()
            .reconcile_local_outbox(&retained, &key, later)
            .unwrap(),
        retained
    );
}

#[test]
fn local_outbox_quota_refusal_rolls_back_candidate_and_preserves_original_custody() {
    let (mut governor, mut member, governor_key, key, _, now) = fixture();
    let mut originals = vec![];
    for index in 0..63 {
        originals.push(local_edit(
            &mut member,
            &key,
            now,
            false,
            &format!("entry-{index}"),
        ));
    }
    let later = now + Duration::from_secs(301);
    checkpoint(&mut governor, &mut member, &governor_key, later, false);
    let before = member.durable().unwrap();
    let original_bytes = postcard::to_allocvec(&originals).unwrap();
    assert!(
        member
            .outbox_mut()
            .reconcile_local_outbox(&originals, &key, later)
            .unwrap_err()
            .contains("count limit")
    );
    assert_eq!(member.durable().unwrap(), before);
    assert_eq!(postcard::to_allocvec(&originals).unwrap(), original_bytes);
    assert!(
        member
            .outbox_mut()
            .reconcile_local_outbox(&vec![originals[0].clone(); 65], &key, later)
            .is_err()
    );
    let mut oversized = originals[0].clone();
    let NeighbourMsg::AppDocument { envelope, .. } = &mut oversized else {
        panic!()
    };
    envelope.payload = vec![0; MAX_PUBLICATION_BYTES + 1];
    assert!(
        member
            .outbox_mut()
            .reconcile_local_outbox(&[oversized], &key, later)
            .unwrap_err()
            .contains("byte limit")
    );
    assert_eq!(member.durable().unwrap(), before);
}

#[test]
fn local_outbox_history_queue_deduplicates_per_route_and_renews_after_expiry() {
    let (mut governor, mut member, governor_key, key, _, now) = fixture();
    let mut second = AutoCommit::new();
    second.put(ROOT, "second-seed", true).unwrap();
    second.commit_edit("fixture", "second-seed");
    governor
        .publish_document("test.app", "tab:2", second.save(), &governor_key, now)
        .unwrap();
    checkpoint(&mut governor, &mut member, &governor_key, now, false);
    let _missing_first = local_edit(&mut member, &key, now, true, "legacy-first");
    let mut second = AutoCommit::load(&member.document("test.app", "tab:2").unwrap()).unwrap();
    second.set_actor(automerge::ActorId::from(
        key.verifying_key().to_bytes().to_vec(),
    ));
    let heads = second.get_heads();
    second.put(ROOT, "legacy-second", true).unwrap();
    second.commit_edit("fixture", "legacy-second");
    let _missing_second = member
        .publish_incremental("test.app", "tab:2", second.save_after(&heads), &key, now)
        .unwrap();
    let history = member.clone();
    let raw = history.durable().unwrap();
    let later = now + Duration::from_secs(301);
    checkpoint(&mut governor, &mut member, &governor_key, later, false);
    let initial = member
        .outbox_mut()
        .recover_local_history(&history, &key, later)
        .unwrap();
    assert_eq!(initial.len(), 2);
    let mut queue = initial
        .into_iter()
        .filter(|message| {
            matches!(message,
                NeighbourMsg::AppDocument { record_id, .. } if record_id == "tab:1"
            )
        })
        .collect::<Vec<_>>();
    let queue_bytes = postcard::to_allocvec(&queue).unwrap();
    let additional = member
        .outbox_mut()
        .recover_local_history_with_outbox(&history, &queue, &key, later)
        .unwrap();
    assert_eq!(
        additional.len(),
        1,
        "a fresh publication for one route cannot duplicate it or stall another route"
    );
    assert!(
        matches!(&additional[0], NeighbourMsg::AppDocument { record_id, .. } if record_id == "tab:2")
    );
    assert_eq!(postcard::to_allocvec(&queue).unwrap(), queue_bytes);
    queue.extend(additional);
    let before = member.durable().unwrap();
    assert!(
        member
            .outbox_mut()
            .recover_local_history_with_outbox(&history, &queue, &key, later)
            .unwrap()
            .is_empty()
    );
    assert_eq!(member.durable().unwrap(), before);
    let expired = later + Duration::from_secs(301);
    checkpoint(&mut governor, &mut member, &governor_key, expired, false);
    let renewed = member
        .outbox_mut()
        .recover_local_history_with_outbox(&history, &queue, &key, expired)
        .unwrap();
    assert_eq!(renewed.len(), 2);
    assert_ne!(renewed, queue);
    for message in renewed {
        governor
            .receive_from_report(
                TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
                message,
                expired,
            )
            .unwrap();
    }
    checkpoint(&mut governor, &mut member, &governor_key, expired, true);
    assert!(
        member
            .outbox()
            .retained_local_history_retired(&history)
            .unwrap()
    );
    assert_eq!(history.durable().unwrap(), raw);
    assert_eq!(member.rows(), governor.rows());
}

/// What a committed edit costs while earlier ones wait for the governor: the
/// owner reserves recovery for every pending publication, then validates that
/// reservation. Each publication's history is built once, when it is new; a
/// history built per pending publication per check made every edit cost the
/// whole record again for each edit still waiting.
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
        // What it remembered is what it computes from nothing.
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
