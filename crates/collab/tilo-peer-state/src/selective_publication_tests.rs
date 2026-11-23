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
    doc.commit_edit("fixture", "foreign", &person(&governor, &b));
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
        panic!("workspace delta first")
    };
    let NeighbourMsg::SyncResp { changes: all, .. } = governor.workspace_message() else {
        panic!("unexpected fixture shape")
    };
    assert!(
        changes.len() < all.len(),
        "ordinary workspace publication must exclude prior causal history"
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
    new.commit_edit("fixture", "created", &person(&governor, &a));
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
        matches!(&created[1], NeighbourMsg::AppDocument { app_id, document_id, envelope, .. } if app_id == "new.app" && document_id == "new-record" && matches!(collab_types::DocumentMutation::decode(&envelope.payload).unwrap(), collab_types::DocumentMutation::Snapshot(_)))
    );
    tail.extend(created);
    let mut fresh = StatePeer::new(
        governor.workspace_id(),
        governor.owner_certificates()[0].clone(),
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
        governor.workspace_id(),
        governor.owner_certificates()[0].clone(),
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

/// The person whose certified device `key` is in `peer`'s workspace.
fn person(peer: &StatePeer, key: &SigningKey) -> [u8; 32] {
    let device = key.verifying_key().to_bytes();
    peer.certificates()
        .into_iter()
        .find(|cert| cert.device_public_key == device)
        .expect("the workspace holds the device's certificate")
        .user_public_key
}

/// The change `device` makes to the document `app` keeps under `named`,
/// adding `value`, as an incremental over it as `peer` holds it.
fn written_by(
    peer: &StatePeer,
    app: &str,
    named: &str,
    device: &SigningKey,
    value: &str,
) -> Vec<u8> {
    let mut doc = AutoCommit::load(&peer.document(app, named).unwrap()).unwrap();
    doc.set_actor(automerge::ActorId::from(
        device.verifying_key().to_bytes().to_vec(),
    ));
    let heads = doc.get_heads();
    doc.put(ROOT, value, true).unwrap();
    doc.commit_edit("fixture", value, &person(peer, device));
    doc.save_after(&heads)
}

tilo_test_env::scenario! {
    fn selective_publication_publishes_only_what_its_basis_allows;

    phase selective_publication_compares_restored_checkpoints_by_exact_bytes;
    {
        let (mut governor, a, _, _, now) = seeded();
        governor
            .publication_mut()
            .durable_bootstrap_messages(&a, now, 1)
            .unwrap();
        let restored = StatePeer::restore(
            &governor.durable().unwrap(),
            governor.workspace_id(),
            governor.owner_certificates()[0].clone(),
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

    phase selective_workspace_only_publication_refuses_a_revoked_owner_device;
    {
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
        let restored = StatePeer::restore(
            &governor.durable().unwrap(),
            governor.workspace_id(),
            governor.owner_certificates()[0].clone(),
            governor.certificates(),
            revocations,
            now,
        )
        .unwrap();
        let mut candidate = restored.clone();
        assert!(
            candidate
                .publication_mut()
                .durable_mutation_messages(&restored, &key, now, 2)
                .is_err(),
            "a revoked owner device no longer writes the workspace's governance"
        );
    }

    phase selective_publication_cannot_discard_prior_document_basis;
    {
        let (mut governor, key, _, _, now) = seeded();
        governor
            .publication_mut()
            .durable_bootstrap_messages(&key, now, 1)
            .unwrap();
        let mut candidate = StatePeer::new(
            governor.workspace_id(),
            governor.owner_certificates()[0].clone(),
            governor.certificates(),
            RevocationList::new(),
            now,
        )
        .unwrap();
        candidate
            .receive(
                key.verifying_key().to_bytes(),
                governor.workspace_message(),
                now,
            )
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

    phase a_record_publishes_only_under_an_application_id;
    // A replica publishes records under application ids only: a string that is
    // not one names no app, and no member would read it as that app's record.
    {
        let (mut governor, a, _b, _bc, now) = seeded();
        let bytes = AutoCommit::new().save();
        assert!(
            governor
                .publication_mut()
                .publish_document("so.tilo.notes\n", "", bytes.clone(), &a, now)
                .is_err(),
            "a malformed application id is no app's record"
        );
        governor
            .publication_mut()
            .publish_document("so.tilo.notes", "", bytes, &a, now)
            .expect("an application id publishes");
    }

    phase a_routes_document_keeps_the_receipt_of_the_device_its_coverage_names;
    // Two devices of the person's each write the same document. Only the
    // device the route's coverage row names writes its receipt: the other's
    // receipt would carry its own commit's generation, and each holder's
    // receipt then reads as the other's regression, so neither copy takes the
    // other's change.
    {
        let (mut first, a, b, _, now) = seeded();
        let app = "test.app";
        let named = "tab:1";
        let bytes = first.document(app, named).unwrap();
        first
            .publication_mut()
            .publish_document(app, named, bytes, &a, now)
            .unwrap();
        first
            .publication_mut()
            .durable_bootstrap_messages(&a, now, 2)
            .unwrap();
        let covered = first.durable_coverage(app, named).unwrap().unwrap();
        assert_eq!(covered.coverage.author, a.verifying_key().to_bytes());

        // The other of the owner's devices writes the document, and publishes
        // its change at its own commit's generation.
        let before = first.clone();
        let second = written_by(&first, app, named, &b, "second");
        first
            .publication_mut()
            .publish_incremental(app, named, second, &b, now)
            .unwrap();
        let published = first
            .publication_mut()
            .durable_mutation_messages(&before, &b, now, 3)
            .unwrap();
        assert!(!published.is_empty(), "the second device publishes its write");
        assert_eq!(
            first.durable_coverage(app, named).unwrap().unwrap(),
            covered,
            "the second device writes its write, not a receipt of its own"
        );
        // A bootstrap rewrites no other device's receipt either.
        first
            .publication_mut()
            .durable_bootstrap_messages(&b, now, 3)
            .unwrap();
        assert_eq!(
            first.durable_coverage(app, named).unwrap().unwrap(),
            covered,
            "a bootstrap at another device keeps the covering device's receipt"
        );
        // The covering device's own receipt advances with its coverage.
        let before = first.clone();
        let third = written_by(&first, app, named, &b, "third");
        first
            .publication_mut()
            .publish_incremental(app, named, third, &b, now)
            .unwrap();
        first
            .publication_mut()
            .durable_mutation_messages(&before, &a, now, 4)
            .unwrap();
        let advanced = first.durable_coverage(app, named).unwrap().unwrap();
        assert_eq!(advanced.coverage.author, a.verifying_key().to_bytes());
        assert!(advanced.coverage.sequence > covered.coverage.sequence);
        assert!(advanced.generation > covered.generation);
    }
}
