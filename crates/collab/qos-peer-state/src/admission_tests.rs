use super::*;
use automerge::{AutoCommit, ROOT, transaction::Transactable};
use ed25519_dalek::SigningKey;
use qos_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};
use web_time::{Duration, SystemTime};
fn fixtures() -> (SigningKey, DeviceCert, SigningKey, DeviceCert, SystemTime) {
    let root = SigningKey::from_bytes(&[1; 32]);
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(2000);
    let make = |n: u8| {
        let key = SigningKey::from_bytes(&[n; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: format!("device-{n}"),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &root).unwrap();
        (key, cert)
    };
    let (a, ac) = make(2);
    let (b, bc) = make(3);
    (a, ac, b, bc, now)
}
#[test]
fn app_waits_for_governance_then_signed_state_converges_without_reexecuting_action() {
    let (a, ac, _, bc, now) = fixtures();
    let room = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let mut native = StatePeer::new(
        room,
        ac.clone(),
        vec![ac.clone(), bc.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    let mut document = AutoCommit::new();
    document.put(ROOT, "value", "native").unwrap();
    document.commit();
    let seed = SeedState {
        room_changes: seed_room(room, &ac, &[ac.clone(), bc.clone()]).unwrap(),
        documents: vec![SeedDocument {
            app_id: "test.app".into(),
            record_id: "tab:1".into(),
            kind: "test".into(),
            bytes: document.save(),
        }],
    };
    native.install_seed(seed, &a, now).unwrap();
    let mut browser =
        StatePeer::new(room, ac.clone(), vec![ac, bc], RevocationList::new(), now).unwrap();
    let app = native.signed_snapshots(&a, now).unwrap().remove(0);
    assert!(
        browser
            .receive(a.verifying_key().to_bytes(), app.clone(), now)
            .unwrap()
            .deferred
    );
    assert!(browser.rows().is_empty());
    browser
        .receive(a.verifying_key().to_bytes(), native.room_message(), now)
        .unwrap();
    assert_eq!(browser.rows(), native.rows());
    assert!(
        browser
            .receive(a.verifying_key().to_bytes(), app, now)
            .unwrap()
            .duplicate
    );
    let restored = StatePeer::restore(
        &browser.durable().unwrap(),
        room,
        native.authority().clone(),
        vec![native.authority().clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    assert_eq!(restored.rows(), browser.rows());
}
#[test]
fn source_room_signature_and_aged_checkpoint_fail_closed() {
    let (a, ac, _, bc, now) = fixtures();
    let room = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let mut native = StatePeer::new(
        room,
        ac.clone(),
        vec![ac.clone(), bc.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    let mut document = AutoCommit::new();
    document.put(ROOT, "value", 1).unwrap();
    document.commit();
    native
        .install_seed(
            SeedState {
                room_changes: seed_room(room, &ac, &[ac.clone(), bc.clone()]).unwrap(),
                documents: vec![SeedDocument {
                    app_id: "test.app".into(),
                    record_id: "tab:1".into(),
                    kind: "test".into(),
                    bytes: document.save(),
                }],
            },
            &a,
            now,
        )
        .unwrap();
    let mut receiver = StatePeer::new(
        room,
        ac.clone(),
        vec![ac, bc.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    receiver
        .receive(a.verifying_key().to_bytes(), native.room_message(), now)
        .unwrap();
    let message = native.signed_snapshots(&a, now).unwrap().remove(0);
    assert!(
        receiver
            .receive(bc.device_public_key, message.clone(), now)
            .is_err()
    );
    assert!(
        receiver
            .receive(
                a.verifying_key().to_bytes(),
                message.clone(),
                now + Duration::from_secs(301)
            )
            .is_err()
    );
    let mut forged = message;
    if let NeighbourMsg::AppDocument { cluster_id, .. } = &mut forged {
        *cluster_id = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".into();
    }
    assert!(
        receiver
            .receive(a.verifying_key().to_bytes(), forged, now)
            .is_err()
    );
    assert!(receiver.rows().is_empty());
}
fn seeded() -> (StatePeer, SigningKey, SigningKey, DeviceCert, SystemTime) {
    let (a, ac, b, bc, now) = fixtures();
    let room = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let mut native = StatePeer::new(
        room,
        ac.clone(),
        vec![ac.clone(), bc.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    let mut document = AutoCommit::new();
    document.put(ROOT, "value", "native").unwrap();
    document.commit();
    native
        .install_seed(
            SeedState {
                room_changes: seed_room(room, &ac, &[ac.clone(), bc.clone()]).unwrap(),
                documents: vec![SeedDocument {
                    app_id: "test.app".into(),
                    record_id: "tab:1".into(),
                    kind: "test".into(),
                    bytes: document.save(),
                }],
            },
            &a,
            now,
        )
        .unwrap();
    (native, a, b, bc, now)
}
#[test]
fn fresh_checkpoint_covers_compacted_other_author_and_restore_never_reuses_sequence() {
    let (mut native, a, b, bc, now) = seeded();
    let mut doc = AutoCommit::load(&native.document("test.app", "tab:1").unwrap()).unwrap();
    let before = doc.get_heads();
    doc.put(ROOT, "other", "B").unwrap();
    doc.commit();
    let old = native
        .publish_incremental("test.app", "tab:1", doc.save_after(&before), &b, now)
        .unwrap();
    let bootstrap = native.bootstrap_messages(&a, now).unwrap();
    let mut fresh = StatePeer::new(
        native.room(),
        native.authority().clone(),
        vec![bc.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    // Retained per-channel replay can arrive in either order.
    assert!(
        fresh
            .receive_from(
                TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
                bootstrap[1].clone(),
                now
            )
            .unwrap()
            .deferred
    );
    fresh
        .receive_from(
            TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
            bootstrap[0].clone(),
            now,
        )
        .unwrap();
    assert_eq!(fresh.rows(), native.rows());
    assert!(
        fresh
            .receive_from(
                TransportOrigin::RelayDelta(bc.device_public_key),
                old.clone(),
                now
            )
            .unwrap()
            .duplicate
    );
    let checkpoint = fresh.durable().unwrap();
    let mut reload = StatePeer::restore(
        &checkpoint,
        native.room(),
        native.authority().clone(),
        vec![bc],
        RevocationList::new(),
        now,
    )
    .unwrap();
    assert!(
        reload
            .receive_from(
                TransportOrigin::RelayDelta(b.verifying_key().to_bytes()),
                old,
                now
            )
            .unwrap()
            .duplicate
    );
    let mut doc = AutoCommit::load(&reload.document("test.app", "tab:1").unwrap()).unwrap();
    let before = doc.get_heads();
    doc.put(ROOT, "new", "B2").unwrap();
    doc.commit();
    let next = reload
        .publish_incremental("test.app", "tab:1", doc.save_after(&before), &b, now)
        .unwrap();
    assert!(matches!(next,NeighbourMsg::AppDocument{envelope,..} if envelope.seq==2));
}
#[test]
fn state_on_presence_cannot_install_governance_or_advance_watermarks() {
    let (mut native, a, _, bc, now) = seeded();
    let bootstrap = native.bootstrap_messages(&a, now).unwrap();
    let mut fresh = StatePeer::new(
        native.room(),
        native.authority().clone(),
        vec![bc],
        RevocationList::new(),
        now,
    )
    .unwrap();
    for message in &bootstrap {
        assert!(
            fresh
                .receive_from(
                    TransportOrigin::UntrustedPresence(a.verifying_key().to_bytes()),
                    message.clone(),
                    now
                )
                .is_err()
        );
    }
    assert!(!fresh.has_governance());
    assert!(fresh.rows().is_empty());
    for message in bootstrap {
        fresh
            .receive_from(
                TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
                message,
                now,
            )
            .unwrap();
    }
    assert_eq!(fresh.rows(), native.rows());
}
#[test]
fn forged_or_mismatched_coverage_cannot_install_snapshot() {
    let (mut native, a, _, bc, now) = seeded();
    let bootstrap = native.bootstrap_messages(&a, now).unwrap();
    let mut fresh = StatePeer::new(
        native.room(),
        native.authority().clone(),
        vec![bc.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    assert!(
        fresh
            .receive_from(
                TransportOrigin::RelayDelta(bc.device_public_key),
                bootstrap[0].clone(),
                now
            )
            .is_err()
    );
    fresh
        .receive_from(
            TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
            bootstrap[0].clone(),
            now,
        )
        .unwrap();
    let mut mismatch = bootstrap[1].clone();
    if let NeighbourMsg::AppDocument { envelope, .. } = &mut mismatch {
        let mut replacement = AutoCommit::new();
        replacement.put(ROOT, "attack", true).unwrap();
        replacement.commit();
        envelope.payload = collab_types::DocumentMutation::Snapshot(replacement.save())
            .encode()
            .unwrap();
        collab_types::sign_op(envelope, &a).unwrap();
    }
    assert!(
        fresh
            .receive_from(
                TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
                mismatch,
                now
            )
            .unwrap_err()
            .contains("coverage")
    );
    assert!(fresh.rows().is_empty());
    fresh
        .receive_from(
            TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
            bootstrap[1].clone(),
            now,
        )
        .unwrap();
    assert_eq!(fresh.rows(), native.rows());
}

#[test]
fn legacy_checkpoint_without_coverage_remains_deferred_and_metadata_cannot_regress() {
    let (mut native, a, _, bc, now) = seeded();
    let messages = native.bootstrap_messages(&a, now).unwrap();
    let NeighbourMsg::SyncResp { changes, room } = messages[0].clone() else {
        unreachable!()
    };
    let mut doc = room::candidate(None, &changes).unwrap();
    doc.delete(ROOT, coverage::FIELD).unwrap();
    doc.commit();
    let legacy = NeighbourMsg::SyncResp {
        room: room.clone(),
        changes: doc
            .get_changes(&[])
            .into_iter()
            .map(|c| c.raw_bytes().to_vec())
            .collect(),
    };
    let mut receiver = StatePeer::new(
        &room,
        native.authority().clone(),
        vec![bc],
        RevocationList::new(),
        now,
    )
    .unwrap();
    receiver
        .receive_from(
            TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
            legacy,
            now,
        )
        .unwrap();
    assert!(
        receiver
            .receive_from(
                TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
                messages[1].clone(),
                now
            )
            .unwrap()
            .deferred
    );
    assert!(receiver.rows().is_empty());
    let mut candidate = room::candidate(None, &changes).unwrap();
    let mut rows = coverage::entries(&candidate).unwrap();
    let entry = rows.values_mut().next().unwrap();
    entry.sequence = 0;
    candidate
        .put(
            ROOT,
            coverage::FIELD,
            automerge::ScalarValue::Bytes(postcard::to_allocvec(&rows).unwrap()),
        )
        .unwrap();
    candidate.commit();
    assert!(
        coverage::monotonic(Some(&room::candidate(None, &changes).unwrap()), &candidate).is_err()
    );
}
#[test]
fn fresh_same_head_checkpoint_is_still_a_durable_watermark_mutation() {
    let (mut native, a, _, bc, now) = seeded();
    let mut receiver = StatePeer::new(
        native.room(),
        native.authority().clone(),
        vec![bc],
        RevocationList::new(),
        now,
    )
    .unwrap();
    for message in native.bootstrap_messages(&a, now).unwrap() {
        receiver
            .receive_from(
                TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
                message,
                now,
            )
            .unwrap();
    }
    let heads = receiver.rows();
    let fresh = native.bootstrap_messages(&a, now).unwrap();
    receiver
        .receive_from(
            TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
            fresh[0].clone(),
            now,
        )
        .unwrap();
    let outcome = receiver
        .receive_from(
            TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
            fresh[1].clone(),
            now,
        )
        .unwrap();
    assert!(!outcome.changed);
    assert!(outcome.metadata_changed);
    assert_eq!(heads, receiver.rows());
}
#[test]
fn frozen_v1_sync_message_discriminants_and_bytes_are_unchanged() {
    assert_eq!(
        postcard::to_allocvec(&NeighbourMsg::SyncReq {
            since: vec![],
            room: "r".into(),
            invite_id: None
        })
        .unwrap(),
        vec![0, 0, 1, b'r', 0]
    );
    assert_eq!(
        postcard::to_allocvec(&NeighbourMsg::SyncResp {
            changes: vec![vec![7]],
            room: "r".into()
        })
        .unwrap(),
        vec![1, 1, 1, 7, 1, b'r']
    );
}

#[test]
fn authenticated_missing_mls_is_durable_and_later_plaintext_cannot_downgrade() {
    let (mut native, a, _, bc, now) = seeded();
    let mut receiver = StatePeer::new(
        native.room(),
        native.authority().clone(),
        vec![bc.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    for message in native.bootstrap_messages(&a, now).unwrap() {
        receiver
            .receive_from(
                TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
                message,
                now,
            )
            .unwrap();
    }
    let messages = native.bootstrap_messages(&a, now).unwrap();
    let mut encrypted = messages[1].clone();
    if let NeighbourMsg::AppDocument {
        envelope,
        app_id,
        cluster_id,
        record_id,
        ..
    } = &mut encrypted
    {
        envelope.encryption = Some(collab_types::EncryptionMetadata {
            epoch: 1,
            channel: collab_types::document_channel_id(app_id, cluster_id, record_id),
        });
        envelope.payload = vec![1, 2, 3];
        collab_types::sign_op(envelope, &a).unwrap();
    }
    let outcome = receiver
        .receive_from(
            TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
            encrypted,
            now,
        )
        .expect("authenticated unsupported encryption must persist its no-downgrade obligation");
    assert!(outcome.metadata_changed);
    let mut restored = StatePeer::restore(
        &receiver.durable().unwrap(),
        native.room(),
        native.authority().clone(),
        vec![bc],
        RevocationList::new(),
        now,
    )
    .unwrap();
    assert!(
        restored
            .receive_from(
                TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
                messages[1].clone(),
                now
            )
            .unwrap_err()
            .contains("downgrade")
    );
}
#[test]
fn self_issued_identity_presence_cannot_expand_the_admitted_roster() {
    let (mut receiver, _, _, _, now) = seeded();
    let rogue_root = SigningKey::from_bytes(&[99; 32]);
    let rogue = SigningKey::from_bytes(&[98; 32]);
    let mut cert = DeviceCert {
        user_public_key: rogue_root.verifying_key().to_bytes(),
        device_public_key: rogue.verifying_key().to_bytes(),
        device_label: "self issued".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &rogue_root).unwrap();
    assert!(
        receiver
            .receive_from(
                TransportOrigin::UntrustedPresence(cert.device_public_key),
                NeighbourMsg::IdentityHello {
                    cert: Box::new(cert),
                    revocations: vec![]
                },
                now
            )
            .is_err()
    );
}

#[test]
fn aged_retained_checkpoint_rejects_without_poisoning_fresh_sync_or_durable_ready_state() {
    let (mut native, key, _, browser_cert, created) = seeded();
    let delayed = created + Duration::from_secs(301);
    let stale = native.bootstrap_messages(&key, created).unwrap();
    let mut receiver = StatePeer::new(native.room(), native.authority().clone(),
        vec![native.authority().clone(), browser_cert], RevocationList::new(), delayed).unwrap();
    let origin = TransportOrigin::RelayDelta(key.verifying_key().to_bytes());
    receiver.receive_from(origin, stale[0].clone(), delayed).unwrap();
    let before = receiver.durable().unwrap();
    assert!(receiver.receive_from(origin, stale[1].clone(), delayed).unwrap_err().contains("timestamp"));
    assert_eq!(receiver.durable().unwrap(), before);
    assert!(receiver.rows().is_empty());
    assert_eq!(receiver.incomplete_reason(), None);
    for message in native.bootstrap_messages(&key, delayed).unwrap() {
        receiver.receive_from(origin, message, delayed).unwrap();
    }
    assert_eq!(receiver.rows(), native.rows());
    let admitted = receiver.durable().unwrap();
    assert!(receiver.receive_from(origin, stale[1].clone(), delayed).is_err());
    assert_eq!(receiver.durable().unwrap(), admitted);
    assert_eq!(receiver.incomplete_reason(), None);
}
