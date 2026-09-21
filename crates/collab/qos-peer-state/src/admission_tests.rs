use super::*;
use automerge::{AutoCommit, ROOT, transaction::Transactable};
use ed25519_dalek::SigningKey;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};
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
fn unchanged_permission_queries_share_policy_data_across_peer_clones() {
    let (peer, _, _, cert, now) = seeded();
    crate::room::POLICY_READS.with(|count| count.set(0));
    let expected = peer.permissions(cert.device_public_key, now).unwrap();
    for _ in 0..100 {
        let actual = peer
            .clone()
            .permissions(cert.device_public_key, now)
            .unwrap();
        assert_eq!(actual.0, expected.0);
        assert_eq!(
            serde_json::to_vec(&actual.1).unwrap(),
            serde_json::to_vec(&expected.1).unwrap()
        );
        assert_eq!(actual.2, expected.2);
    }
    assert!(
        crate::room::POLICY_READS.with(|count| count.get()) <= 1,
        "unchanged policy data is decoded at most once"
    );
}
#[test]
fn admitted_immutable_certificate_does_not_repeat_signature_checks_per_permission_query() {
    let (peer, _, _, cert, now) = seeded();
    crate::state::CERTIFICATE_VERIFICATIONS.with(|count| count.set(0));
    let expected = peer.permissions(cert.device_public_key, now).unwrap();
    for _ in 0..100 {
        let actual = peer
            .clone()
            .permissions(cert.device_public_key, now)
            .unwrap();
        assert_eq!(actual.0, expected.0);
        assert_eq!(
            serde_json::to_vec(&actual.1).unwrap(),
            serde_json::to_vec(&expected.1).unwrap()
        );
        assert_eq!(actual.2, expected.2);
    }
    assert_eq!(
        crate::state::CERTIFICATE_VERIFICATIONS.with(|count| count.get()),
        0,
        "immutable signatures were already verified when the certificate entered the roster"
    );
}
#[test]
fn exported_certificate_mutation_cannot_change_verified_roster_or_replace_it_via_presence() {
    let (mut peer, _, _, cert, now) = seeded();
    let old = peer.clone();
    let mut exported = peer
        .certificates()
        .into_iter()
        .find(|c| c.device_public_key == cert.device_public_key)
        .unwrap();
    exported.signature[0] ^= 1;
    assert!(peer.permissions(cert.device_public_key, now).is_ok());
    assert!(old.permissions(cert.device_public_key, now).is_ok());
    let before = peer.durable().unwrap();
    assert!(
        peer.receive_from(
            TransportOrigin::UntrustedPresence(cert.device_public_key),
            NeighbourMsg::IdentityHello {
                cert: Box::new(exported),
                revocations: vec![]
            },
            now
        )
        .is_err()
    );
    assert_eq!(peer.durable().unwrap(), before);
    assert!(peer.certificates().contains(&cert));
}
#[test]
fn cached_policy_rechecks_membership_expiry_and_governance_replacement() {
    use automerge::ReadDoc;
    use collab_types::caps::{CapOverride, Principal, Scope, Subject, Verb};
    let (key, cert, _, other, now) = fixtures();
    let room_id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let mut doc = crate::room::candidate(
        None,
        &seed_room(room_id, &cert, &[cert.clone(), other.clone()]).unwrap(),
    )
    .unwrap();
    doc.put(ROOT, "owner", "another-person").unwrap();
    let caps = doc.get(ROOT, "capabilities").unwrap().unwrap().1;
    let person = Principal::person_from_key(collab_types::Ed25519PublicKey::from_bytes(
        cert.user_public_key,
    ));
    let grant = CapOverride::grant(Subject::Principal(person), Scope::Workspace, Verb::Write)
        .with_expiry(2001);
    doc.put(
        &caps,
        "membership",
        automerge::ScalarValue::Bytes(serde_json::to_vec(&grant).unwrap()),
    )
    .unwrap();
    let changes = doc
        .get_changes(&[])
        .into_iter()
        .map(|c| c.raw_bytes().to_vec())
        .collect();
    let mut peer = StatePeer::new(
        room_id,
        cert.clone(),
        vec![cert.clone(), other],
        RevocationList::new(),
        now,
    )
    .unwrap();
    peer.install_seed(
        SeedState {
            room_changes: changes,
            documents: vec![],
        },
        &key,
        now,
    )
    .unwrap();
    assert_eq!(
        peer.permissions(cert.device_public_key, now).unwrap().0,
        collab_types::Role::Editor
    );
    assert!(
        peer.permissions(cert.device_public_key, now + Duration::from_secs(2))
            .unwrap_err()
            .contains("membership")
    );
    let old = peer.clone();
    doc.delete(&caps, "membership").unwrap();
    let changes = doc
        .get_changes(&[])
        .into_iter()
        .map(|c| c.raw_bytes().to_vec())
        .collect();
    peer.receive(
        cert.device_public_key,
        NeighbourMsg::SyncResp {
            room: room_id.into(),
            changes,
        },
        now,
    )
    .unwrap();
    assert!(
        peer.permissions(cert.device_public_key, now)
            .unwrap_err()
            .contains("membership")
    );
    assert_eq!(
        old.permissions(cert.device_public_key, now).unwrap().0,
        collab_types::Role::Editor
    );
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
fn retained_relay_replay_accepts_certified_document_after_live_freshness_window() {
    let (mut governor, authority, _, browser_cert, authored_at) = seeded();
    let replayed_at = authored_at + Duration::from_secs(301);
    let mut browser = StatePeer::new(
        governor.room(),
        governor.authority().clone(),
        vec![browser_cert],
        RevocationList::new(),
        replayed_at,
    )
    .unwrap();
    let origin = TransportOrigin::RetainedRelayDelta(authority.verifying_key().to_bytes());

    for message in governor
        .bootstrap_messages(&authority, authored_at)
        .unwrap()
    {
        browser.receive_from(origin, message, replayed_at).unwrap();
    }

    assert_eq!(browser.rows(), governor.rows());
    assert_eq!(
        browser.document_heads("test.app", "tab:1"),
        governor.document_heads("test.app", "tab:1")
    );
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
    let mut receiver = StatePeer::new(
        native.room(),
        native.authority().clone(),
        vec![native.authority().clone(), browser_cert],
        RevocationList::new(),
        delayed,
    )
    .unwrap();
    let origin = TransportOrigin::RelayDelta(key.verifying_key().to_bytes());
    receiver
        .receive_from(origin, stale[0].clone(), delayed)
        .unwrap();
    let before = receiver.durable().unwrap();
    assert!(
        receiver
            .receive_from(origin, stale[1].clone(), delayed)
            .unwrap_err()
            .contains("timestamp")
    );
    assert_eq!(receiver.durable().unwrap(), before);
    assert!(receiver.rows().is_empty());
    assert_eq!(receiver.incomplete_reason(), None);
    for message in native.bootstrap_messages(&key, delayed).unwrap() {
        receiver.receive_from(origin, message, delayed).unwrap();
    }
    assert_eq!(receiver.rows(), native.rows());
    let admitted = receiver.durable().unwrap();
    assert!(
        receiver
            .receive_from(origin, stale[1].clone(), delayed)
            .is_err()
    );
    assert_eq!(receiver.durable().unwrap(), admitted);
    assert_eq!(receiver.incomplete_reason(), None);
}

#[test]
fn learned_revocation_and_governed_identity_survive_stale_enrollment_restore() {
    use tilo_user_identity::{RevocationEntry, RevocationReason, sign_revocation_entry};
    let (mut native, a, b, bc, now) = seeded();
    let mut receiver = StatePeer::new(
        native.room(),
        native.authority().clone(),
        vec![],
        RevocationList::new(),
        now,
    )
    .unwrap();
    for msg in native.bootstrap_messages(&a, now).unwrap() {
        receiver
            .receive_from(
                TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
                msg,
                now,
            )
            .unwrap();
    }
    let learned = receiver
        .receive_from(
            TransportOrigin::UntrustedPresence(bc.device_public_key),
            NeighbourMsg::IdentityHello {
                cert: Box::new(bc.clone()),
                revocations: vec![],
            },
            now,
        )
        .unwrap();
    assert!(
        !learned.metadata_changed,
        "governed SyncResp already imported certificate"
    );
    assert!(receiver.certificates().contains(&bc));
    let restored = StatePeer::restore(
        &receiver.durable().unwrap(),
        native.room(),
        native.authority().clone(),
        vec![],
        RevocationList::new(),
        now,
    )
    .unwrap();
    assert!(restored.permissions(bc.device_public_key, now).is_ok());
    assert!(receiver.permissions(bc.device_public_key, now).is_ok());
    let mut revocation = RevocationEntry {
        user_public_key: bc.user_public_key,
        revoked_device_public_key: bc.device_public_key,
        revoked_at_ns: 1,
        reason: RevocationReason::Compromised,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut revocation, &SigningKey::from_bytes(&[1; 32])).unwrap();
    let learned = receiver
        .receive_from(
            TransportOrigin::UntrustedPresence(a.verifying_key().to_bytes()),
            NeighbourMsg::IdentityHello {
                cert: Box::new(native.authority().clone()),
                revocations: vec![revocation],
            },
            now,
        )
        .unwrap();
    assert!(learned.metadata_changed);
    assert!(
        receiver
            .permissions(bc.device_public_key, now)
            .unwrap_err()
            .contains("revoked"),
        "external revocation must take effect with a warm policy cache"
    );
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
            .permissions(b.verifying_key().to_bytes(), now)
            .unwrap_err()
            .contains("revoked")
    );
    let mut doc = AutoCommit::load(&native.document("test.app", "tab:1").unwrap()).unwrap();
    let heads = doc.get_heads();
    doc.put(ROOT, "new", "forbidden").unwrap();
    doc.commit();
    let op = native
        .publish_incremental("test.app", "tab:1", doc.save_after(&heads), &b, now)
        .unwrap();
    assert!(
        restored
            .receive_from(
                TransportOrigin::RelayDelta(b.verifying_key().to_bytes()),
                op,
                now
            )
            .unwrap_err()
            .contains("revoked")
    );
}

#[test]
fn deferred_encryption_obligation_is_journaled_on_fresh_and_unchanged_governance() {
    for existing in [false, true] {
        let (mut native, a, b, bc, now) = seeded();
        let mut receiver = StatePeer::new(
            native.room(),
            native.authority().clone(),
            vec![],
            RevocationList::new(),
            now,
        )
        .unwrap();
        if existing {
            for msg in native.bootstrap_messages(&a, now).unwrap() {
                receiver
                    .receive_from(
                        TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
                        msg,
                        now,
                    )
                    .unwrap();
            }
        }
        let messages = native.bootstrap_messages(&a, now).unwrap();
        // Install exactly the room we will drain again: the existing path then
        // has no changed room heads, only the deferred metadata obligation.
        if existing {
            receiver
                .receive_from(
                    TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
                    messages[0].clone(),
                    now,
                )
                .unwrap();
        }
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
        let author = if existing {
            b.verifying_key().to_bytes()
        } else {
            a.verifying_key().to_bytes()
        };
        if existing {
            if let NeighbourMsg::AppDocument { peer, envelope, .. } = &mut encrypted {
                *peer = peer_handle(&author);
                envelope.author = collab_types::Ed25519PublicKey::from_bytes(author);
                envelope.seq = 1;
                collab_types::sign_op(envelope, &b).unwrap();
            }
        }
        let encrypted_outcome = receiver
            .receive_from(TransportOrigin::RelayDelta(author), encrypted, now)
            .unwrap();
        assert_eq!(encrypted_outcome.deferred, !existing);
        if existing {
            assert!(encrypted_outcome.metadata_changed);
            assert!(encrypted_outcome.incomplete.is_some());
        }
        if existing {
            receiver
                .receive_from(
                    TransportOrigin::UntrustedPresence(author),
                    NeighbourMsg::IdentityHello {
                        cert: Box::new(bc),
                        revocations: vec![],
                    },
                    now,
                )
                .unwrap();
        }
        let outcome = receiver
            .receive_from(
                TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
                messages[0].clone(),
                now,
            )
            .unwrap();
        assert!(
            outcome.metadata_changed || (existing && encrypted_outcome.metadata_changed),
            "immediate or deferred metadata must be journaled"
        );
        assert!(outcome.incomplete.is_some());
        if existing {
            assert!(!outcome.changed);
        }
        let mut restored = StatePeer::restore(
            &receiver.durable().unwrap(),
            native.room(),
            native.authority().clone(),
            vec![],
            RevocationList::new(),
            now,
        )
        .unwrap();
        assert!(restored.incomplete_reason().is_some());
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
}

#[test]
fn expired_durable_outbox_reconciles_against_fresh_governor_without_duplicate_changes() {
    for delivered in [false, true] {
        let (mut governor, a, b, bc, now) = seeded();
        let mut client = StatePeer::new(
            governor.room(),
            governor.authority().clone(),
            vec![bc.clone()],
            RevocationList::new(),
            now,
        )
        .unwrap();
        for msg in governor.bootstrap_messages(&a, now).unwrap() {
            client
                .receive_from(
                    TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
                    msg,
                    now,
                )
                .unwrap();
        }
        let mut doc = AutoCommit::load(&client.document("test.app", "tab:1").unwrap()).unwrap();
        doc.set_actor(automerge::ActorId::from(
            b.verifying_key().to_bytes().to_vec(),
        ));
        let before = doc.get_heads();
        let text = doc
            .put_object(ROOT, "text", automerge::ObjType::Text)
            .unwrap();
        doc.splice_text(&text, 0, 0, "saved once").unwrap();
        doc.commit();
        let original = client
            .publish_incremental("test.app", "tab:1", doc.save_after(&before), &b, now)
            .unwrap();
        if delivered {
            governor
                .receive_from(
                    TransportOrigin::RelayDelta(b.verifying_key().to_bytes()),
                    original.clone(),
                    now,
                )
                .unwrap();
        }
        let later = now + Duration::from_secs(301);
        let mut client = StatePeer::restore(
            &client.durable().unwrap(),
            governor.room(),
            governor.authority().clone(),
            vec![bc],
            RevocationList::new(),
            later,
        )
        .unwrap();
        let old_bytes = postcard::to_allocvec(&original).unwrap();
        let mut outbox = vec![original.clone()];
        assert_eq!(client.outbox_status(&outbox, later).blocked_expired, 1);
        // A reload alone cannot authorize re-signing or turn persistence into sync.
        assert_eq!(client.reconcile_outbox(&outbox, &b, later).unwrap(), outbox);
        for msg in governor.bootstrap_messages(&a, later).unwrap() {
            client
                .receive_from(
                    TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
                    msg,
                    later,
                )
                .unwrap();
        }
        outbox = client.reconcile_outbox(&outbox, &b, later).unwrap();
        if delivered {
            assert!(outbox.is_empty());
        } else {
            assert_eq!(outbox.len(), 2);
            assert_eq!(postcard::to_allocvec(&outbox[0]).unwrap(), old_bytes);
            let (
                NeighbourMsg::AppDocument { envelope: old, .. },
                NeighbourMsg::AppDocument { envelope: new, .. },
            ) = (&outbox[0], &outbox[1])
            else {
                panic!()
            };
            assert_eq!(old.payload, new.payload);
            assert_eq!(new.seq, old.seq + 1);
            assert_eq!(old.timestamp, now);
            assert_eq!(new.timestamp, later);
            assert_eq!(client.outbox_status(&outbox, later).blocked_expired, 0);
            governor
                .receive_from(
                    TransportOrigin::RelayDelta(b.verifying_key().to_bytes()),
                    outbox[1].clone(),
                    later,
                )
                .unwrap();
            assert!(
                governor
                    .receive_from(
                        TransportOrigin::RelayDelta(b.verifying_key().to_bytes()),
                        outbox[1].clone(),
                        later
                    )
                    .unwrap()
                    .duplicate
            );
            assert!(
                governor
                    .receive_from(
                        TransportOrigin::RelayDelta(b.verifying_key().to_bytes()),
                        original,
                        later
                    )
                    .is_err()
            );
            for msg in governor.bootstrap_messages(&a, later).unwrap() {
                client
                    .receive_from(
                        TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
                        msg,
                        later,
                    )
                    .unwrap();
            }
            outbox = client.reconcile_outbox(&outbox, &b, later).unwrap();
            assert!(outbox.is_empty());
        }
        assert_eq!(client.rows(), governor.rows());
        let final_doc = AutoCommit::load(&governor.document("test.app", "tab:1").unwrap()).unwrap();
        use automerge::ReadDoc;
        let (_, text) = final_doc.get(ROOT, "text").unwrap().unwrap();
        assert_eq!(final_doc.text(text).unwrap(), "saved once");
        assert_eq!(client.outbox_status(&outbox, later).pending, 0);
    }
}

#[test]
fn legacy_durable_identity_metadata_is_not_silently_assumed_empty() {
    let (peer, _, _, bc, now) = seeded();
    let mut bytes = peer.durable().unwrap();
    bytes[0] = 1;
    let unchanged = bytes.clone();
    let error = StatePeer::restore(
        &bytes,
        peer.room(),
        peer.authority().clone(),
        vec![bc],
        RevocationList::new(),
        now,
    )
    .err()
    .unwrap();
    assert!(error.contains("incomplete"));
    assert!(error.contains("identity/revocation"));
    assert_eq!(bytes, unchanged);
}

#[test]
fn higher_governor_floor_without_causal_changes_does_not_acknowledge_outbox() {
    let (mut governor, a, b, bc, now) = seeded();
    let mut client = StatePeer::new(
        governor.room(),
        governor.authority().clone(),
        vec![bc],
        RevocationList::new(),
        now,
    )
    .unwrap();
    for message in governor.bootstrap_messages(&a, now).unwrap() {
        client
            .receive_from(
                TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
                message,
                now,
            )
            .unwrap();
    }
    let mut doc = AutoCommit::load(&client.document("test.app", "tab:1").unwrap()).unwrap();
    doc.set_actor(automerge::ActorId::from(
        b.verifying_key().to_bytes().to_vec(),
    ));
    let before = doc.get_heads();
    let text = doc
        .put_object(ROOT, "text", automerge::ObjType::Text)
        .unwrap();
    doc.splice_text(&text, 0, 0, "one ").unwrap();
    doc.commit();
    let first = client
        .publish_incremental("test.app", "tab:1", doc.save_after(&before), &b, now)
        .unwrap();
    let before = doc.get_heads();
    doc.splice_text(&text, 4, 0, "two").unwrap();
    doc.commit();
    let second = client
        .publish_incremental("test.app", "tab:1", doc.save_after(&before), &b, now)
        .unwrap();
    let outcome = governor
        .receive_from(
            TransportOrigin::RelayDelta(b.verifying_key().to_bytes()),
            second.clone(),
            now,
        )
        .unwrap();
    assert!(!outcome.changed);
    let later = now + Duration::from_secs(301);
    for message in governor.bootstrap_messages(&a, later).unwrap() {
        client
            .receive_from(
                TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
                message,
                later,
            )
            .unwrap();
    }
    assert!(
        !client.covers(&first),
        "higher author floor cannot acknowledge a missing causal change"
    );
    assert!(
        !client.covers(&second),
        "missing dependencies are not materialized checkpoint coverage"
    );
    let outbox = client
        .reconcile_outbox(&[first, second], &b, later)
        .unwrap();
    assert_eq!(outbox.len(), 4);
    for message in &outbox[2..] {
        governor
            .receive_from(
                TransportOrigin::RelayDelta(b.verifying_key().to_bytes()),
                message.clone(),
                later,
            )
            .unwrap();
    }
    for message in governor.bootstrap_messages(&a, later).unwrap() {
        client
            .receive_from(
                TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
                message,
                later,
            )
            .unwrap();
    }
    assert!(
        client
            .reconcile_outbox(&outbox, &b, later)
            .unwrap()
            .is_empty()
    );
    assert_eq!(client.rows(), governor.rows());
    use automerge::ReadDoc;
    let final_doc = AutoCommit::load(&governor.document("test.app", "tab:1").unwrap()).unwrap();
    let (_, text) = final_doc.get(ROOT, "text").unwrap().unwrap();
    assert_eq!(final_doc.text(text).unwrap(), "one two");
}

#[test]
fn expired_predecessor_and_fresh_dependent_converge_without_waiting_for_expiry() {
    let (mut governor, a, b, bc, now) = seeded();
    let origin = TransportOrigin::RelayDelta(a.verifying_key().to_bytes());
    let browser_origin = TransportOrigin::RelayDelta(b.verifying_key().to_bytes());
    let mut client = StatePeer::new(
        governor.room(),
        governor.authority().clone(),
        vec![bc],
        RevocationList::new(),
        now,
    )
    .unwrap();
    for message in governor.bootstrap_messages(&a, now).unwrap() {
        client.receive_from(origin, message, now).unwrap();
    }
    let mut doc = AutoCommit::load(&client.document("test.app", "tab:1").unwrap()).unwrap();
    doc.set_actor(automerge::ActorId::from(
        b.verifying_key().to_bytes().to_vec(),
    ));
    let before = doc.get_heads();
    let text = doc
        .put_object(ROOT, "text", automerge::ObjType::Text)
        .unwrap();
    doc.splice_text(&text, 0, 0, "one ").unwrap();
    doc.commit();
    let first = client
        .publish_incremental("test.app", "tab:1", doc.save_after(&before), &b, now)
        .unwrap();
    let recent = now + Duration::from_secs(290);
    let before = doc.get_heads();
    doc.splice_text(&text, 4, 0, "two").unwrap();
    doc.commit();
    let second = client
        .publish_incremental("test.app", "tab:1", doc.save_after(&before), &b, recent)
        .unwrap();
    let original_first = postcard::to_allocvec(&first).unwrap();
    let original_second = postcard::to_allocvec(&second).unwrap();
    let reconnect = now + Duration::from_secs(301);
    for message in governor.bootstrap_messages(&a, reconnect).unwrap() {
        client.receive_from(origin, message, reconnect).unwrap();
    }
    let mut outbox = client
        .reconcile_outbox(&[first, second.clone()], &b, reconnect)
        .unwrap();
    assert_eq!(outbox.len(), 3);
    assert_eq!(postcard::to_allocvec(&outbox[0]).unwrap(), original_first);
    assert_eq!(postcard::to_allocvec(&outbox[1]).unwrap(), original_second);
    // Execute the exact production publication filter/order and sent-byte guard.
    // Native emits fresh covered checkpoints only after an app changes, just as
    // the real fixture loop does. No manual reconnect or time advance occurs.
    let mut published = std::collections::BTreeSet::new();
    let mut delivered_sequences = vec![];
    for _ in 0..4 {
        let batch = crate::outbox::publication_batch(&outbox, reconnect)
            .cloned()
            .collect::<Vec<_>>();
        for message in batch {
            if !published.insert(postcard::to_allocvec(&message).unwrap()) {
                continue;
            }
            if let NeighbourMsg::AppDocument { envelope, .. } = &message {
                delivered_sequences.push(envelope.seq);
            }
            let outcome = governor
                .receive_from(browser_origin, message, reconnect)
                .unwrap();
            if outcome.changed {
                for checkpoint in governor.bootstrap_messages(&a, reconnect).unwrap() {
                    client.receive_from(origin, checkpoint, reconnect).unwrap();
                    outbox = client.reconcile_outbox(&outbox, &b, reconnect).unwrap();
                }
            }
        }
        if outbox.is_empty() {
            break;
        }
    }
    assert!(
        crate::outbox::fresh(&second, reconnect),
        "dependent must still be fresh throughout recovery"
    );
    assert!(
        outbox.is_empty(),
        "fresh dependent must be renewed after predecessor overtakes its sequence, without waiting for expiry"
    );
    assert_eq!(delivered_sequences, vec![2, 3, 4]);
    assert_eq!(client.rows(), governor.rows());
    use automerge::ReadDoc;
    let final_doc = AutoCommit::load(&governor.document("test.app", "tab:1").unwrap()).unwrap();
    let (_, text) = final_doc.get(ROOT, "text").unwrap().unwrap();
    assert_eq!(final_doc.text(text).unwrap(), "one two");
    assert_eq!(client.outbox_status(&outbox, reconnect).pending, 0);
}

#[test]
fn dynamic_governed_identity_import_preserves_history_and_requires_authority() {
    let (mut governor, a, b, _, now) = seeded();
    let root = SigningKey::from_bytes(&[1; 32]);
    let new_key = SigningKey::from_bytes(&[8; 32]);
    let mut cert = governor.authority().clone();
    cert.device_public_key = new_key.verifying_key().to_bytes();
    cert.device_label = "dynamic client".into();
    sign_device_cert(&mut cert, &root).unwrap();
    let mut existing = governor.clone();
    let before = governor.durable().unwrap();
    assert!(governor.admit_certificate(cert.clone(), &b, now).is_err());
    assert_eq!(before, governor.durable().unwrap());
    let rows = governor.rows();
    let floors = governor.replay_floors();
    assert!(governor.admit_certificate(cert.clone(), &a, now).unwrap());
    assert!(!governor.admit_certificate(cert.clone(), &a, now).unwrap());
    assert_eq!(rows, governor.rows());
    assert_eq!(floors, governor.replay_floors());
    let outcome = existing
        .receive_from(
            TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
            governor.room_message(),
            now,
        )
        .unwrap();
    assert!(outcome.metadata_changed);
    assert!(existing.certificates().contains(&cert));
    assert!(
        existing
            .permissions(new_key.verifying_key().to_bytes(), now)
            .is_ok()
    );
    assert_eq!(rows, existing.rows());
    let mut doc = AutoCommit::load(&existing.document("test.app", "tab:1").unwrap()).unwrap();
    doc.set_actor(automerge::ActorId::from(
        new_key.verifying_key().to_bytes().to_vec(),
    ));
    let before = doc.get_heads();
    doc.put(ROOT, "joined", true).unwrap();
    doc.commit();
    let message = existing
        .publish_incremental("test.app", "tab:1", doc.save_after(&before), &new_key, now)
        .unwrap();
    assert!(
        governor
            .receive_from(
                TransportOrigin::RelayDelta(new_key.verifying_key().to_bytes()),
                message,
                now
            )
            .unwrap()
            .changed
    );
    let restored = StatePeer::restore(
        &existing.durable().unwrap(),
        existing.room(),
        existing.authority().clone(),
        vec![],
        RevocationList::new(),
        now,
    )
    .unwrap();
    assert!(restored.certificates().contains(&cert));
    let other_root = SigningKey::from_bytes(&[9; 32]);
    cert.user_public_key = other_root.verifying_key().to_bytes();
    sign_device_cert(&mut cert, &other_root).unwrap();
    assert!(governor.admit_certificate(cert, &a, now).is_err());
}

#[test]
fn expired_known_identity_does_not_block_governed_join() {
    let (mut governor, a, _, _, now) = seeded();
    let root = SigningKey::from_bytes(&[1; 32]);
    let mut cert = governor.authority().clone();
    cert.device_public_key = SigningKey::from_bytes(&[8; 32]).verifying_key().to_bytes();
    cert.expires_at_ns = Some(
        (now + Duration::from_secs(1))
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64,
    );
    sign_device_cert(&mut cert, &root).unwrap();
    governor.admit_certificate(cert.clone(), &a, now).unwrap();
    let mut existing = governor.clone();
    assert!(existing.permissions(cert.device_public_key, now).is_ok());
    let later = now + Duration::from_secs(2);
    assert!(
        existing
            .permissions(cert.device_public_key, later)
            .unwrap_err()
            .contains("expired"),
        "a warm policy cache must not cache certificate validity"
    );
    let mut next = governor.authority().clone();
    next.device_public_key = SigningKey::from_bytes(&[9; 32]).verifying_key().to_bytes();
    sign_device_cert(&mut next, &root).unwrap();
    assert!(governor.admit_certificate(next.clone(), &a, later).unwrap());
    assert!(
        existing
            .receive(a.verifying_key().to_bytes(), governor.room_message(), later)
            .unwrap()
            .metadata_changed
    );
    assert!(
        existing
            .permissions(cert.device_public_key, later)
            .unwrap_err()
            .contains("expired")
    );
    assert!(existing.permissions(next.device_public_key, later).is_ok());
    let mut empty = StatePeer::new(
        governor.room(),
        governor.authority().clone(),
        vec![],
        RevocationList::new(),
        later,
    )
    .unwrap();
    assert!(
        empty
            .receive(a.verifying_key().to_bytes(), governor.room_message(), later)
            .is_err(),
        "previously unknown expired certificate cannot be imported"
    );
}
#[test]
fn governed_import_is_atomic_even_for_direct_admission() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let (mut source, a, _, _, now) = seeded();
    let mut receiver = source.clone();
    let mut valid = source.authority().clone();
    valid.device_public_key = SigningKey::from_bytes(&[8; 32]).verifying_key().to_bytes();
    sign_device_cert(&mut valid, &root).unwrap();
    source.admit_certificate(valid, &a, now).unwrap();
    let NeighbourMsg::SyncResp { changes, room } = source.room_message() else {
        unreachable!()
    };
    let mut candidate = room::candidate(None, &changes).unwrap();
    use automerge::ReadDoc;
    let (_, identities) = candidate.get(ROOT, "peer_identities_v1").unwrap().unwrap();
    candidate
        .put(
            &identities,
            "zz-invalid",
            automerge::ScalarValue::Bytes(vec![0]),
        )
        .unwrap();
    candidate.commit();
    let changes = candidate
        .get_changes(&[])
        .into_iter()
        .map(|c| c.raw_bytes().to_vec())
        .collect();
    let before = receiver.durable().unwrap();
    assert!(
        receiver
            .receive(
                a.verifying_key().to_bytes(),
                NeighbourMsg::SyncResp { changes, room },
                now
            )
            .is_err()
    );
    assert_eq!(
        before,
        receiver.durable().unwrap(),
        "direct receive cannot partly import certificate metadata"
    );
}

#[test]
fn durability_receipt_is_distinct_from_acceptance_and_survives_restore() {
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
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "value", 1).unwrap();
    doc.commit();
    native
        .install_seed(
            SeedState {
                room_changes: seed_room(room, &ac, &[ac.clone(), bc.clone()]).unwrap(),
                documents: vec![SeedDocument {
                    app_id: "test.app".into(),
                    record_id: "tab:1".into(),
                    kind: "test".into(),
                    bytes: doc.save(),
                }],
            },
            &a,
            now,
        )
        .unwrap();
    let mut browser = StatePeer::new(
        room,
        ac.clone(),
        vec![ac.clone(), bc.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    for message in native.bootstrap_messages(&a, now).unwrap() {
        browser
            .receive(a.verifying_key().to_bytes(), message, now)
            .unwrap();
    }
    assert!(
        browser
            .durable_coverage("test.app", "tab:1")
            .unwrap()
            .is_none(),
        "legacy accepted coverage is not a durable receipt"
    );
    let messages = native.durable_bootstrap_messages(&a, now, 7).unwrap();
    browser
        .receive(a.verifying_key().to_bytes(), messages[0].clone(), now)
        .unwrap();
    assert!(
        browser
            .durable_coverage("test.app", "tab:1")
            .unwrap()
            .is_none(),
        "receipt cannot cover a checkpoint that is not accepted yet"
    );
    for message in messages.into_iter().skip(1) {
        browser
            .receive(a.verifying_key().to_bytes(), message, now)
            .unwrap();
    }
    let receipt = browser
        .durable_coverage("test.app", "tab:1")
        .unwrap()
        .unwrap();
    assert_eq!(receipt.generation, 7);
    assert!(browser.durable_covers_current("test.app", "tab:1").unwrap());
    assert_eq!(
        browser
            .durable_coverage_with_current("test.app", "tab:1")
            .unwrap(),
        Some((receipt.clone(), true))
    );
    let bytes = browser.durable().unwrap();
    let mut browser =
        StatePeer::restore(&bytes, room, ac, vec![bc], RevocationList::new(), now).unwrap();
    crate::state::CHECKPOINT_DECODES.with(|count| count.set(0));
    for _ in 0..100 {
        let fork = browser.clone();
        assert_eq!(
            fork.durable_coverage("test.app", "tab:1").unwrap().unwrap(),
            receipt
        );
        assert_eq!(
            fork.durable_coverage_with_current("test.app", "tab:1")
                .unwrap(),
            Some((receipt.clone(), true))
        );
    }
    assert_eq!(
        crate::state::CHECKPOINT_DECODES.with(|count| count.get()),
        1,
        "peer clones must share immutable checkpoint decoding"
    );
    assert_eq!(
        browser
            .durable_coverage("test.app", "tab:1")
            .unwrap()
            .unwrap(),
        receipt
    );
    let mut doc = AutoCommit::load(&browser.document("test.app", "tab:1").unwrap()).unwrap();
    let heads = browser.document_heads("test.app", "tab:1").unwrap();
    assert_eq!(heads, doc.get_heads());
    doc.put(ROOT, "value", 2).unwrap();
    doc.commit();
    let local = browser
        .publish_incremental("test.app", "tab:1", doc.save_after(&heads), &b, now)
        .unwrap();
    assert!(
        !browser.durable_covers_current("test.app", "tab:1").unwrap(),
        "an older receipt never covers a newer local edit"
    );
    assert_eq!(
        browser
            .durable_coverage_with_current("test.app", "tab:1")
            .unwrap(),
        Some((receipt.clone(), false))
    );
    assert!(
        browser
            .durable_coverage_with_current("test.app", "missing")
            .unwrap()
            .is_none()
    );
    let previous = browser.clone();
    native
        .receive(b.verifying_key().to_bytes(), local, now)
        .unwrap();
    for message in native.durable_bootstrap_messages(&a, now, 8).unwrap() {
        browser
            .receive(a.verifying_key().to_bytes(), message, now)
            .unwrap();
    }
    let (next, current) = browser
        .durable_coverage_with_current("test.app", "tab:1")
        .unwrap()
        .unwrap();
    assert_eq!(next.generation, 8);
    assert!(current, "replacement checkpoint covers the newer edit");
    assert_eq!(
        previous
            .durable_coverage_with_current("test.app", "tab:1")
            .unwrap(),
        Some((receipt, false)),
        "checkpoint replacement must not mutate older peer clones"
    );
}

#[test]
fn legacy_v2_journal_restores_coverage_and_reemits_the_authenticated_wire_version() {
    let (authority_key, authority, member_key, member, now) = fixtures();
    let room = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let app = "test.app";
    let record = "tab:1";
    let mut governor = StatePeer::new(
        room,
        authority.clone(),
        vec![authority.clone(), member.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    let mut document = AutoCommit::new();
    document.put(ROOT, "value", "legacy").unwrap();
    document.commit();
    governor
        .install_seed(
            SeedState {
                room_changes: seed_room(room, &authority, &[authority.clone(), member.clone()])
                    .unwrap(),
                documents: vec![SeedDocument {
                    app_id: app.into(),
                    record_id: record.into(),
                    kind: "test".into(),
                    bytes: document.save(),
                }],
            },
            &authority_key,
            now,
        )
        .unwrap();
    let mut messages = governor
        .durable_bootstrap_messages(&authority_key, now, 7)
        .unwrap();
    let NeighbourMsg::AppDocument { envelope, .. } = &mut messages[1] else {
        panic!("expected document checkpoint")
    };
    let mutation = collab_types::DocumentMutation::decode(&envelope.payload).unwrap();
    let version = collab_types::DocumentWireVersion::QosV1;
    envelope.payload = mutation.encode_for(version).unwrap();
    envelope.artifact = collab_types::document_artifact_id_for_version(version, app, room, record);
    envelope.attestation_id =
        collab_types::document_attestation_id_for_version(version, app, room, record);
    collab_types::sign_op(envelope, &authority_key).unwrap();
    let payload_hash = *blake3::hash(&envelope.payload).as_bytes();

    let NeighbourMsg::SyncResp { changes, .. } = &messages[0] else {
        panic!("expected room checkpoint")
    };
    let mut room_doc = crate::room::candidate(None, changes).unwrap();
    let route_key = crate::coverage::route_key(app, record);
    let mut coverage = crate::coverage::entries(&room_doc)
        .unwrap()
        .remove(&route_key)
        .unwrap();
    coverage.payload_hash = payload_hash;
    crate::coverage::put(&mut room_doc, coverage.clone()).unwrap();
    let mut receipts = crate::durability::entries(&room_doc).unwrap();
    receipts.insert(
        route_key,
        crate::durability::DurableReceipt::new(coverage, 7).unwrap(),
    );
    crate::durability::put(&mut room_doc, receipts).unwrap();
    messages[0] = NeighbourMsg::SyncResp {
        changes: room_doc
            .get_changes(&[])
            .into_iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect(),
        room: room.into(),
    };

    let mut receiver = StatePeer::new(
        room,
        authority.clone(),
        vec![authority.clone(), member.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    for message in messages {
        receiver
            .receive(authority.device_public_key, message, now)
            .unwrap();
    }
    assert!(receiver.durable_coverage(app, record).unwrap().is_some());

    let old_journal = receiver.durable_v2_for_test().unwrap();
    let mut restored = StatePeer::restore(
        &old_journal,
        room,
        authority.clone(),
        vec![member.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    assert!(restored.durable_coverage(app, record).unwrap().is_some());

    let mut edited = AutoCommit::load(&restored.document(app, record).unwrap()).unwrap();
    let heads = edited.get_heads();
    edited.put(ROOT, "member", "edit").unwrap();
    edited.commit();
    let emitted = restored
        .publish_incremental(app, record, edited.save_after(&heads), &member_key, now)
        .unwrap();
    let NeighbourMsg::AppDocument { envelope, .. } = &emitted else {
        panic!("expected emitted document mutation")
    };
    let decoded = collab_types::DocumentMutation::decode_versioned(&envelope.payload).unwrap();
    assert_eq!(decoded.version, collab_types::DocumentWireVersion::QosV1);
    assert_eq!(
        envelope.artifact,
        collab_types::document_artifact_id_for_version(version, app, room, record)
    );
    assert_eq!(
        envelope.attestation_id,
        collab_types::document_attestation_id_for_version(version, app, room, record)
    );

    let mut old_decoder = StatePeer::restore(
        &old_journal,
        room,
        authority.clone(),
        vec![member.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    let mut renewal_governor = old_decoder.clone();
    let mut renewal_client = restored.clone();
    let mut dependent = AutoCommit::load(&renewal_client.document(app, record).unwrap()).unwrap();
    let dependent_heads = dependent.get_heads();
    dependent.put(ROOT, "dependent", "edit").unwrap();
    dependent.commit();
    let second = renewal_client
        .publish_incremental(
            app,
            record,
            dependent.save_after(&dependent_heads),
            &member_key,
            now,
        )
        .unwrap();
    let outcome = renewal_governor
        .receive(member_key.verifying_key().to_bytes(), second.clone(), now)
        .unwrap();
    assert!(
        !outcome.changed,
        "the governor cannot apply a dependent change before its predecessor"
    );
    let reconnect = now + Duration::from_secs(301);
    for message in renewal_governor
        .durable_bootstrap_messages(&authority_key, reconnect, 8)
        .unwrap()
    {
        renewal_client
            .receive(authority.device_public_key, message, reconnect)
            .unwrap();
    }
    assert!(!renewal_client.covers(&emitted));
    assert!(!renewal_client.covers(&second));

    let mut mixed_version = emitted.clone();
    let NeighbourMsg::AppDocument {
        envelope: mixed_envelope,
        ..
    } = &mut mixed_version
    else {
        panic!("expected emitted document mutation")
    };
    mixed_envelope.payload = decoded
        .mutation
        .encode_for(collab_types::DocumentWireVersion::QosV2)
        .unwrap();
    collab_types::sign_op(mixed_envelope, &member_key).unwrap();
    assert_eq!(
        renewal_client
            .reconcile_outbox(&[mixed_version], &member_key, reconnect)
            .unwrap_err(),
        "document route cannot change its authenticated wire version"
    );

    let foreign_room = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    let mut foreign_room_message = emitted.clone();
    let NeighbourMsg::AppDocument {
        cluster_id,
        envelope: foreign_envelope,
        ..
    } = &mut foreign_room_message
    else {
        panic!("expected emitted document mutation")
    };
    *cluster_id = foreign_room.into();
    foreign_envelope.artifact =
        collab_types::document_artifact_id_for_version(version, app, foreign_room, record);
    foreign_envelope.attestation_id =
        collab_types::document_attestation_id_for_version(version, app, foreign_room, record);
    collab_types::sign_op(foreign_envelope, &member_key).unwrap();
    assert!(!renewal_client.covers(&foreign_room_message));
    let unchanged = renewal_client.durable().unwrap();
    assert_eq!(
        renewal_client
            .reconcile_outbox(&[foreign_room_message], &member_key, reconnect)
            .unwrap_err(),
        "document outbox room mismatch"
    );
    assert_eq!(renewal_client.durable().unwrap(), unchanged);

    let renewed = renewal_client
        .reconcile_outbox(&[emitted.clone(), second.clone()], &member_key, reconnect)
        .unwrap();
    assert_eq!(renewed.len(), 4);
    for message in &renewed[2..] {
        let NeighbourMsg::AppDocument {
            cluster_id,
            envelope,
            ..
        } = message
        else {
            panic!("expected renewed document mutation")
        };
        assert_eq!(cluster_id, room);
        assert_eq!(
            collab_types::DocumentMutation::decode_versioned(&envelope.payload)
                .unwrap()
                .version,
            collab_types::DocumentWireVersion::QosV1
        );
        renewal_governor
            .receive(
                member_key.verifying_key().to_bytes(),
                message.clone(),
                reconnect,
            )
            .unwrap();
    }
    for message in renewal_governor
        .durable_bootstrap_messages(&authority_key, reconnect, 9)
        .unwrap()
    {
        renewal_client
            .receive(authority.device_public_key, message, reconnect)
            .unwrap();
    }
    assert!(
        renewal_client
            .reconcile_outbox(&renewed, &member_key, reconnect)
            .unwrap()
            .is_empty(),
        "the governed legacy checkpoint must finally drain both original and renewed evidence"
    );

    let mut mismatched = emitted.clone();
    let NeighbourMsg::AppDocument { envelope, .. } = &mut mismatched else {
        panic!("expected emitted document mutation")
    };
    envelope.payload = decoded
        .mutation
        .encode_for(collab_types::DocumentWireVersion::QosV2)
        .unwrap();
    collab_types::sign_op(envelope, &member_key).unwrap();
    assert_eq!(
        old_decoder
            .receive(member_key.verifying_key().to_bytes(), mismatched, now)
            .unwrap_err(),
        "document route cannot change its authenticated wire version"
    );
    assert!(
        old_decoder
            .receive(member_key.verifying_key().to_bytes(), emitted.clone(), now,)
            .unwrap()
            .changed
    );

    for message in old_decoder
        .durable_bootstrap_messages(&authority_key, now, 8)
        .unwrap()
    {
        restored
            .receive(authority.device_public_key, message, now)
            .unwrap();
    }
    assert!(
        restored.covers(&emitted),
        "the authenticated legacy checkpoint causally covers its legacy incremental"
    );
    assert_eq!(restored.outbox_status(&[emitted.clone()], now).pending, 0);
    assert!(
        restored
            .reconcile_outbox(&[emitted.clone()], &member_key, now)
            .unwrap()
            .is_empty(),
        "covered legacy evidence must retire without current-namespace renewal"
    );

    let unresolved_journal = receiver
        .durable_v2_without_covered_snapshot_for_test()
        .unwrap();
    let mut unresolved = StatePeer::restore(
        &unresolved_journal,
        room,
        authority.clone(),
        vec![member.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    assert_eq!(
        unresolved
            .receive(member_key.verifying_key().to_bytes(), emitted, now)
            .unwrap_err(),
        "document wire version unavailable; fresh authenticated checkpoint required"
    );
}

#[test]
fn nonowner_topology_and_record_route_changes_are_rejected_before_admission() {
    use automerge::{ObjType, ReadDoc};
    let (a, ac, b, bc, now) = fixtures();
    let room = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee";
    let changes = seed_room(room, &ac, &[ac.clone(), bc.clone()]).unwrap();
    let mut doc = room::candidate(None, &changes).unwrap();
    let owners = doc
        .put_object(ROOT, "contents_owners_v1", ObjType::Map)
        .unwrap();
    doc.put(&owners, "0002", hex::encode(ac.device_public_key))
        .unwrap();
    let contents = doc.put_object(ROOT, "contents", ObjType::Map).unwrap();
    let strip = doc
        .put_object(contents, "0002000000000001", ObjType::Map)
        .unwrap();
    doc.put(&strip, "name", "Shared").unwrap();
    doc.put(&strip, "order", 0_u64).unwrap();
    let groups = doc.put_object(&strip, "groups", ObjType::Map).unwrap();
    let group = doc
        .put_object(groups, "0002000000000002", ObjType::Map)
        .unwrap();
    let tabs = doc.put_object(&group, "tabs", ObjType::Map).unwrap();
    let tab = doc
        .put_object(tabs, "0002000000000003", ObjType::Map)
        .unwrap();
    doc.put(&tab, "title", "Original").unwrap();
    doc.put(&tab, "record_id", "tab:0000000000000001").unwrap();
    doc.commit();
    let mut peer =
        StatePeer::new(room, ac.clone(), vec![ac, bc], RevocationList::new(), now).unwrap();
    peer.install_seed(
        SeedState {
            room_changes: doc
                .get_changes(&[])
                .iter()
                .map(|c| c.raw_bytes().to_vec())
                .collect(),
            documents: vec![],
        },
        &a,
        now,
    )
    .unwrap();
    let before = peer.durable().unwrap();
    doc.set_actor(automerge::ActorId::from(
        b.verifying_key().to_bytes().to_vec(),
    ));
    doc.put(&tab, "title", "Stolen").unwrap();
    doc.put(&tab, "record_id", "tab:0000000000000012").unwrap();
    doc.commit();
    let result = peer.receive(
        b.verifying_key().to_bytes(),
        NeighbourMsg::SyncResp {
            room: room.into(),
            changes: doc
                .get_changes(&[])
                .iter()
                .map(|c| c.raw_bytes().to_vec())
                .collect(),
        },
        now,
    );
    assert!(
        result.is_err(),
        "another certified device must not overwrite owned topology"
    );
    assert_eq!(
        peer.durable().unwrap(),
        before,
        "unadmitted topology must never become visible"
    );
    let _ = doc.get(ROOT, "contents").unwrap();
}

#[test]
fn selective_publication_covers_foreign_metadata_only_admission_and_new_records() {
    let (mut governor, a, b, bc, now) = seeded();
    let app = "test.app";
    let record = "tab:1";
    let bytes = governor.document(app, record).unwrap();
    governor
        .publish_document("untouched.app", "other", bytes, &a, now)
        .unwrap();
    let baseline = governor.durable_bootstrap_messages(&a, now, 1).unwrap();
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
    doc.commit();
    let change = doc.save_after(&heads);
    let original = governor
        .publish_incremental(app, record, change.clone(), &b, now)
        .unwrap();
    let mut tail = governor
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
        unreachable!()
    };
    assert!(
        changes.len() < all.len(),
        "ordinary room publication must exclude prior causal history"
    );
    before = governor.clone();
    let rows = governor.rows();
    let metadata_only = governor
        .publish_incremental(app, record, change, &b, now)
        .unwrap();
    assert_eq!(governor.rows(), rows);
    let metadata = governor
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
    new.commit();
    governor
        .publish_document("new.app", "new-record", new.save(), &a, now)
        .unwrap();
    let created = governor
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
    let before = governor.clone();
    assert!(
        governor
            .durable_mutation_messages(&before, &a, now, 5)
            .unwrap()
            .is_empty(),
        "no-op must not invent publication sequences or receipts"
    );
}

#[test]
fn selective_publication_compares_restored_checkpoints_by_exact_bytes() {
    let (mut governor, a, _, _, now) = seeded();
    governor.durable_bootstrap_messages(&a, now, 1).unwrap();
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
            .durable_mutation_messages(&restored, &a, now, 2)
            .unwrap()
            .is_empty(),
        "restored identical checkpoint bytes must not create a publication"
    );
}

#[test]
fn selective_room_only_publication_rechecks_current_governor_authority() {
    let (mut governor, key, _, _, now) = seeded();
    governor.durable_bootstrap_messages(&key, now, 1).unwrap();
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
    let mut candidate = StatePeer::restore(
        &governor.durable().unwrap(),
        governor.room(),
        governor.authority().clone(),
        governor.certificates(),
        revocations,
        now,
    )
    .unwrap();
    let exact = candidate.durable().unwrap();
    assert!(
        candidate
            .durable_mutation_messages(&governor, &key, now, 2)
            .is_err(),
        "room-only/no-op publication cannot skip current membership validation"
    );
    assert_eq!(candidate.durable().unwrap(), exact);
}

#[test]
fn selective_publication_cannot_discard_prior_document_basis() {
    let (mut governor, key, _, _, now) = seeded();
    governor.durable_bootstrap_messages(&key, now, 1).unwrap();
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
            .durable_mutation_messages(&governor, &key, now, 2)
            .is_err(),
        "publication must preserve the prior record basis"
    );
}
