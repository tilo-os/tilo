//! Selective durable publication: a governor publishes only what changed
//! since its last committed peer, and never what would discard its basis.
use super::*;
use automerge::{AutoCommit, ROOT, transaction::Transactable};
use ed25519_dalek::SigningKey;

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end scenario: its setup and assertions stay together"
)]
fn selective_publication_covers_foreign_metadata_only_admission_and_new_records() {
    let (mut governor, a, b, bc, now) = seeded();
    let app = "test.app";
    let record = "tab:1";
    let bytes = governor.document(app, record).unwrap();
    governor
        .publication_mut()
        .publish_document("untouched.app", "other", bytes, &a, now)
        .unwrap();
    let baseline = governor
        .publication_mut()
        .durable_bootstrap_messages(&a, now, 1)
        .unwrap();
    let untouched = governor
        .durable_coverage("untouched.app", "other")
        .unwrap()
        .unwrap();
    let mut offline = governor.clone();
    let mut before = governor.clone();
    let mut doc = AutoCommit::load(&governor.document(app, record).unwrap()).unwrap();
    doc.set_actor(automerge::ActorId::from(
        b.verifying_key().to_bytes().to_vec(),
    ));
    let heads = doc.get_heads();
    doc.put(ROOT, "foreign", "original effect").unwrap();
    doc.commit_edit("fixture", "foreign");
    let change = doc.save_after(&heads);
    let original = governor
        .publication_mut()
        .publish_incremental(app, record, change.clone(), &b, now)
        .unwrap();
    let mut tail = governor
        .publication_mut()
        .durable_mutation_messages(&before, &a, now, 2)
        .unwrap();
    assert_eq!(tail.len(), 2);
    assert_eq!(
        governor.durable_coverage("untouched.app", "other").unwrap(),
        Some(untouched.clone())
    );
    let NeighbourMsg::SyncResp { changes, .. } = &tail[0] else {
        panic!("room delta first")
    };
    let NeighbourMsg::SyncResp { changes: all, .. } = governor.room_message() else {
        panic!("unexpected fixture shape")
    };
    assert!(
        changes.len() < all.len(),
        "ordinary room publication must exclude prior causal history"
    );
    before = governor.clone();
    let rows = governor.rows();
    let metadata_only = governor
        .publication_mut()
        .publish_incremental(app, record, change, &b, now)
        .unwrap();
    assert_eq!(governor.rows(), rows);
    let metadata = governor
        .publication_mut()
        .durable_mutation_messages(&before, &a, now, 3)
        .unwrap();
    assert_eq!(
        metadata.len(),
        2,
        "same-head watermark advance still needs a covered exact snapshot"
    );
    assert_eq!(
        governor
            .durable_coverage(app, record)
            .unwrap()
            .unwrap()
            .generation,
        3
    );
    tail.extend(metadata);
    before = governor.clone();
    let mut new = AutoCommit::new();
    new.put(ROOT, "created", true).unwrap();
    new.commit_edit("fixture", "created");
    governor
        .publication_mut()
        .publish_document("new.app", "new-record", new.save(), &a, now)
        .unwrap();
    let created = governor
        .publication_mut()
        .durable_mutation_messages(&before, &a, now, 4)
        .unwrap();
    assert_eq!(created.len(), 2);
    assert!(
        matches!(&created[1], NeighbourMsg::AppDocument { app_id, record_id, envelope, .. } if app_id == "new.app" && record_id == "new-record" && matches!(collab_types::DocumentMutation::decode(&envelope.payload).unwrap(), collab_types::DocumentMutation::Snapshot(_)))
    );
    tail.extend(created);
    let mut fresh = StatePeer::new(
        governor.room(),
        governor.authority().clone(),
        vec![bc],
        RevocationList::new(),
        now,
    )
    .unwrap();
    for message in baseline.into_iter().chain(tail.clone()) {
        fresh
            .receive(a.verifying_key().to_bytes(), message, now)
            .unwrap();
    }
    for message in tail.iter().chain(tail.iter()).cloned() {
        offline
            .receive(a.verifying_key().to_bytes(), message, now)
            .unwrap();
    }
    for receiver in [&fresh, &offline] {
        assert_eq!(receiver.rows(), governor.rows());
        assert!(receiver.covers(&original));
        assert!(receiver.covers(&metadata_only));
        assert_eq!(
            receiver.durable_coverage("untouched.app", "other").unwrap(),
            Some(untouched.clone())
        );
        assert!(
            receiver
                .durable_covers_current("new.app", "new-record")
                .unwrap()
        );
    }
    let restored = StatePeer::restore(
        &fresh.durable().unwrap(),
        governor.room(),
        governor.authority().clone(),
        governor.certificates(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    assert!(restored.covers(&original));
    assert!(restored.covers(&metadata_only));
    // The covered floor is not coverage of whatever bytes claim it. The
    // incremental is decoded in the isolate, which refuses a covered change
    // followed by a copy of it with a bad checksum rather than keeping the
    // valid prefix and reporting the checkpoint's heads unchanged.
    let NeighbourMsg::AppDocument { envelope, .. } = &original else {
        panic!("document publication")
    };
    let collab_types::DocumentMutation::Incremental(covered) =
        collab_types::DocumentMutation::decode(&envelope.payload).unwrap()
    else {
        panic!("incremental publication")
    };
    let mut bad_checksum = covered.clone();
    bad_checksum[4] ^= 0x01;
    let mut corrupt = covered;
    corrupt.extend_from_slice(&bad_checksum);
    let mut forged = original.clone();
    let NeighbourMsg::AppDocument { envelope, .. } = &mut forged else {
        panic!("document publication")
    };
    envelope.payload = collab_types::DocumentMutation::Incremental(corrupt)
        .encode()
        .unwrap();
    assert!(!restored.covers(&forged));
    let before = governor.clone();
    assert!(
        governor
            .publication_mut()
            .durable_mutation_messages(&before, &a, now, 5)
            .unwrap()
            .is_empty(),
        "no-op must not invent publication sequences or receipts"
    );
}

#[test]
fn selective_publication_compares_restored_checkpoints_by_exact_bytes() {
    let (mut governor, a, _, _, now) = seeded();
    governor
        .publication_mut()
        .durable_bootstrap_messages(&a, now, 1)
        .unwrap();
    let restored = StatePeer::restore(
        &governor.durable().unwrap(),
        governor.room(),
        governor.authority().clone(),
        governor.certificates(),
        RevocationList::new(),
        now,
    )
    .unwrap();

    assert!(
        governor
            .durable_coverage("test.app", "tab:1")
            .unwrap()
            .is_some(),
        "warm only the candidate checkpoint decode cache"
    );
    assert!(
        governor
            .publication_mut()
            .durable_mutation_messages(&restored, &a, now, 2)
            .unwrap()
            .is_empty(),
        "restored identical checkpoint bytes must not create a publication"
    );
}

#[test]
fn selective_room_only_publication_rechecks_current_governor_authority() {
    let (mut governor, key, _, _, now) = seeded();
    governor
        .publication_mut()
        .durable_bootstrap_messages(&key, now, 1)
        .unwrap();
    let root = SigningKey::from_bytes(&[1; 32]);
    let mut revoked = tilo_user_identity::RevocationEntry {
        user_public_key: root.verifying_key().to_bytes(),
        revoked_device_public_key: key.verifying_key().to_bytes(),
        revoked_at_ns: 1,
        reason: tilo_user_identity::RevocationReason::Retired,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut revoked, &root).unwrap();
    let mut revocations = RevocationList::new();
    revocations.insert(revoked).unwrap();
    assert!(
        StatePeer::restore(
            &governor.durable().unwrap(),
            governor.room(),
            governor.authority().clone(),
            governor.certificates(),
            revocations,
            now,
        )
        .err()
        .unwrap()
        .contains("authority device is revoked")
    );
}

#[test]
fn selective_publication_cannot_discard_prior_document_basis() {
    let (mut governor, key, _, _, now) = seeded();
    governor
        .publication_mut()
        .durable_bootstrap_messages(&key, now, 1)
        .unwrap();
    let mut candidate = StatePeer::new(
        governor.room(),
        governor.authority().clone(),
        governor.certificates(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    candidate
        .receive(key.verifying_key().to_bytes(), governor.room_message(), now)
        .unwrap();
    assert!(candidate.rows().is_empty());
    assert!(
        candidate
            .publication_mut()
            .durable_mutation_messages(&governor, &key, now, 2)
            .is_err(),
        "publication must preserve the prior record basis"
    );
}
