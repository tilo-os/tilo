use super::*;
use crate::native_footprint_vendor_tests::assert_precharged;
use automerge::{AutoCommit, ROOT, ReadDoc, transaction::Transactable};
fn restore(peer: &StatePeer, now: SystemTime, work: &mut PeerWork<'_>) -> BoundedStatePeer {
    BoundedStatePeer::restore(
        &peer.durable().unwrap(),
        peer.room(),
        peer.authority(),
        &[],
        &RevocationList::new(),
        now,
        work,
    )
    .unwrap()
}
fn expected(peer: &StatePeer, host: &DeviceCert, device: &DeviceCert, now: SystemTime) -> [u8; 32] {
    use crate::room::{
        delivery_policy::{self, Error, Metadata},
        read::LegacyRead,
    };
    let certificates = peer.certificates();
    let revocations = peer.revocations();
    delivery_policy::legacy(
        &LegacyRead(peer.admitted_room().unwrap()),
        Metadata {
            room: peer.room(),
            workspace: 37,
            authority: peer.authority(),
            host,
            device,
            certificates: certificates.iter(),
            revocations: revocations.iter(),
        },
        || {
            for row in peer.rows() {
                peer.check_document_read(
                    device.device_public_key,
                    &row.app_id,
                    &row.record_id,
                    now,
                )
                .map_err(|_| Error::CurrentDenied)?;
            }
            Ok(())
        },
    )
    .unwrap()
}
#[test]
fn actual_delivery_holder_preserves_distinct_host_device_and_checks_every_real_route() {
    let (mut peer, key, _, device, now) = crate::admission_tests::seeded();
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "text", "second record").unwrap();
    peer.publish_document("z.app", "tab:2", doc.save(), &key, now)
        .unwrap();
    let mut host = peer.authority().clone();
    host.device_label = "actual distinct host label".into();
    tilo_user_identity::sign_device_cert(
        &mut host,
        &ed25519_dalek::SigningKey::from_bytes(&[1; 32]),
    )
    .unwrap();
    let digest = expected(&peer, &host, &device, now);
    assert_eq!(
        peer.delivery_policy_unqualified(37, &host, &device, now)
            .unwrap()
            .digest(),
        digest
    );
    let current = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let mut registry = PeerRegistry::new(NativeAllowance::new(4096).unwrap()).unwrap();
    let mut turn = registry
        .begin_turn(current.clone(), PeerLimits::default())
        .unwrap();
    let mut work = turn.work().unwrap();
    let native = restore(&peer, now, &mut work);
    let read = native.read(&work).unwrap();
    ROUTE_CHECKS.with(|n| n.set(0));
    let before = current.retained_bytes();
    let got =
        assert_precharged(&current, || read.delivery_policy(37, &host, &device, now)).unwrap();
    assert_eq!(got.digest(), digest);
    assert_eq!(got.room(), peer.room());
    assert_eq!(got.authority_root(), peer.authority().user_public_key);
    assert_eq!(ROUTE_CHECKS.with(|n| n.get()), 2);
    assert_eq!(current.retained_bytes(), before);
    assert_ne!(
        got.digest(),
        read.delivery_policy(37, &device, &host, now)
            .unwrap()
            .digest()
    );
}
#[test]
fn actual_delivery_holder_pays_current_turn_and_keeps_only_borrowed_inline_data() {
    let (peer, _, _, device, now) = crate::admission_tests::seeded();
    let resident = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let mut registry = PeerRegistry::new(NativeAllowance::new(4096).unwrap()).unwrap();
    let native = {
        let mut turn = registry
            .begin_turn(resident.clone(), PeerLimits::default())
            .unwrap();
        restore(&peer, now, &mut turn.work().unwrap())
    };
    let residency = resident.retained_bytes();
    let current = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let before = current.retained_bytes();
    let policy = {
        let mut turn = registry
            .begin_turn(current.clone(), PeerLimits::default())
            .unwrap();
        let work = turn.work().unwrap();
        let read = native.read(&work).unwrap();
        let baseline = current.retained_bytes();
        let policy = assert_precharged(&current, || {
            read.delivery_policy(37, peer.authority(), &device, now)
        })
        .unwrap();
        assert_eq!(current.retained_bytes(), baseline);
        assert_eq!(resident.retained_bytes(), residency);
        let held = work.reserve_output(current.available_bytes()).unwrap();
        assert!(matches!(
            read.delivery_policy(37, peer.authority(), &device, now),
            Err(PeerDeliveryPolicyError::Peer(BoundedPeerError::Resource(
                NativeResourceError::WorkBytes
            )))
        ));
        drop(held);
        current.finish_turn();
        assert!(
            read.delivery_policy(37, peer.authority(), &device, now)
                .is_err()
        );
        policy
    };
    drop(registry);
    assert_eq!(current.retained_bytes(), before);
    assert_eq!(policy.room(), peer.room());
    assert_eq!(
        policy.digest(),
        expected(&peer, peer.authority(), &device, now)
    );
}

fn seed_with_expiring_member() -> (StatePeer, DeviceCert, SystemTime) {
    let (base, key, _, mut device, now) = crate::admission_tests::seeded();
    device.expires_at_ns = Some(crate::state::now_ns(now) + 1_000_000_000);
    tilo_user_identity::sign_device_cert(
        &mut device,
        &ed25519_dalek::SigningKey::from_bytes(&[1; 32]),
    )
    .unwrap();
    let roster = vec![base.authority().clone(), device.clone()];
    let mut peer = StatePeer::new(
        base.room(),
        base.authority().clone(),
        roster.clone(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "value", "record").unwrap();
    peer.install_seed(
        SeedState {
            room_changes: seed_room(base.room(), base.authority(), &roster).unwrap(),
            documents: vec![SeedDocument {
                app_id: "test.app".into(),
                record_id: "tab:1".into(),
                kind: "test".into(),
                bytes: doc.save(),
            }],
        },
        &key,
        now,
    )
    .unwrap();
    (peer, device, now)
}
#[test]
fn actual_delivery_holder_denies_current_expired_and_revoked_members() {
    use tilo_user_identity::{RevocationEntry, RevocationReason, sign_revocation_entry};
    for revoked in [false, true] {
        let (mut peer, device, now) = seed_with_expiring_member();
        let at = if revoked {
            now
        } else {
            now + Duration::from_secs(1)
        };
        if revoked {
            let mut entry = RevocationEntry {
                user_public_key: device.user_public_key,
                revoked_device_public_key: device.device_public_key,
                revoked_at_ns: 1,
                reason: RevocationReason::Compromised,
                signature: [0; 64],
            };
            sign_revocation_entry(&mut entry, &ed25519_dalek::SigningKey::from_bytes(&[1; 32]))
                .unwrap();
            peer.receive_from(
                TransportOrigin::UntrustedPresence(peer.authority().device_public_key),
                NeighbourMsg::IdentityHello {
                    cert: Box::new(peer.authority().clone()),
                    revocations: vec![entry],
                },
                now,
            )
            .unwrap();
        }
        assert!(matches!(
            peer.delivery_policy_unqualified(37, peer.authority(), &device, at),
            Err(PeerDeliveryPolicyError::CurrentDenied)
        ));
        let current = NativeAllowance::new(64 * 1024 * 1024).unwrap();
        let mut registry = PeerRegistry::new(NativeAllowance::new(4096).unwrap()).unwrap();
        let mut turn = registry
            .begin_turn(current.clone(), PeerLimits::default())
            .unwrap();
        let mut work = turn.work().unwrap();
        let native = restore(&peer, now, &mut work);
        let read = native.read(&work).unwrap();
        assert!(matches!(
            assert_precharged(&current, || read.delivery_policy(
                37,
                peer.authority(),
                &device,
                at
            )),
            Err(PeerDeliveryPolicyError::CurrentDenied)
        ));
    }
}
#[test]
fn actual_delivery_holder_preserves_empty_room_semantics_and_resource_refusals() {
    let (source, key, _, device, now) = crate::admission_tests::seeded();
    let mut empty = StatePeer::new(
        source.room(),
        source.authority().clone(),
        source.certificates(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    let missing = empty.clone();
    empty
        .install_seed(
            SeedState {
                room_changes: seed_room(source.room(), source.authority(), &source.certificates())
                    .unwrap(),
                documents: vec![],
            },
            &key,
            now,
        )
        .unwrap();
    let mut registry = PeerRegistry::new(NativeAllowance::new(4096).unwrap()).unwrap();
    let (native, missing, with_routes) = {
        let mut turn = registry
            .begin_turn(
                NativeAllowance::new(64 * 1024 * 1024).unwrap(),
                PeerLimits::default(),
            )
            .unwrap();
        let mut work = turn.work().unwrap();
        (
            restore(&empty, now, &mut work),
            restore(&missing, now, &mut work),
            restore(&source, now, &mut work),
        )
    };
    let current = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    {
        let mut turn = registry
            .begin_turn(current.clone(), PeerLimits::default())
            .unwrap();
        let work = turn.work().unwrap();
        let read = native.read(&work).unwrap();
        // The old projection has no independent membership gate with no routes.
        // This is why its result cannot stand in for authenticated session proof.
        let mut unknown = device.clone();
        unknown.device_public_key = [99; 32];
        ROUTE_CHECKS.with(|n| n.set(0));
        assert!(
            read.delivery_policy(37, empty.authority(), &unknown, now)
                .is_ok()
        );
        assert_eq!(ROUTE_CHECKS.with(|n| n.get()), 0);
        let absent = missing.read(&work).unwrap();
        assert!(matches!(
            absent.delivery_policy(37, empty.authority(), &device, now),
            Err(PeerDeliveryPolicyError::Unavailable)
        ));
        current.finish_turn();
        for view in [&read, &absent] {
            assert!(matches!(
                view.delivery_policy(37, empty.authority(), &device, now),
                Err(PeerDeliveryPolicyError::Peer(BoundedPeerError::Resource(
                    NativeResourceError::WorkBytes
                )))
            ));
        }
    }
    let mut foreign = PeerRegistry::new(NativeAllowance::new(4096).unwrap()).unwrap();
    let mut foreign_turn = foreign
        .begin_turn(
            NativeAllowance::new(64 * 1024 * 1024).unwrap(),
            PeerLimits::default(),
        )
        .unwrap();
    let foreign_work = foreign_turn.work().unwrap();
    assert!(native.read(&foreign_work).is_err());
    assert!(missing.read(&foreign_work).is_err());
    for json in [false, true] {
        let current = NativeAllowance::new(16 * 1024 * 1024).unwrap();
        let mut limits = PeerLimits::default();
        if json {
            limits.governance_json.allocation_bytes = 1;
        } else {
            limits.governance_collection_bytes = 1;
        }
        let mut turn = registry.begin_turn(current.clone(), limits).unwrap();
        let work = turn.work().unwrap();
        let read = with_routes.read(&work).unwrap();
        let before = current.retained_bytes();
        let result = assert_precharged(&current, || {
            read.delivery_policy(37, source.authority(), &device, now)
        });
        if json {
            assert!(matches!(
                result,
                Err(PeerDeliveryPolicyError::Peer(BoundedPeerError::Decode(_)))
            ))
        } else {
            assert!(matches!(
                result,
                Err(PeerDeliveryPolicyError::Peer(BoundedPeerError::Resource(
                    NativeResourceError::WorkBytes
                )))
            ))
        }
        assert_eq!(current.retained_bytes(), before);
    }
}
#[test]
fn actual_delivery_holder_uses_verified_metadata_and_ignores_document_heads() {
    use tilo_user_identity::{
        RevocationEntry, RevocationReason, sign_device_cert, sign_revocation_entry,
    };
    let (mut peer, key, _, device, now) = crate::admission_tests::seeded();
    let original = expected(&peer, peer.authority(), &device, now);
    let mut changed = AutoCommit::new();
    changed
        .put(ROOT, "value", "different body and heads")
        .unwrap();
    peer.publish_document("test.app", "tab:1", changed.save(), &key, now)
        .unwrap();
    assert_eq!(expected(&peer, peer.authority(), &device, now), original);
    let mut enrollment = Vec::new();
    let mut entries = Vec::new();
    for byte in [39, 12] {
        let root = ed25519_dalek::SigningKey::from_bytes(&[byte; 32]);
        let mut cert = device.clone();
        cert.user_public_key = root.verifying_key().to_bytes();
        cert.device_public_key = ed25519_dalek::SigningKey::from_bytes(&[byte + 1; 32])
            .verifying_key()
            .to_bytes();
        cert.device_label = format!("independent signed root {byte}: {}", "label".repeat(256));
        sign_device_cert(&mut cert, &root).unwrap();
        enrollment.push(cert);
        let mut entry = RevocationEntry {
            user_public_key: root.verifying_key().to_bytes(),
            revoked_device_public_key: [byte + 2; 32],
            revoked_at_ns: byte as u64,
            reason: RevocationReason::Compromised,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut entry, &root).unwrap();
        entries.push(entry);
    }
    let mut verified = RevocationList::new();
    for entry in entries {
        verified.insert(entry).unwrap();
    }
    peer = StatePeer::restore(
        &peer.durable().unwrap(),
        peer.room(),
        peer.authority().clone(),
        enrollment,
        verified,
        now,
    )
    .unwrap();
    assert_eq!(peer.certificates().len(), 4);
    assert_eq!(peer.revocations().len(), 2);
    let digest = expected(&peer, peer.authority(), &device, now);
    // Other members' certificates and device revocations are outside this
    // consumer's authorization view (v2): admitting or revoking them must not
    // reset this consumer's delivery session.
    assert_eq!(digest, original);
    let current = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let mut registry = PeerRegistry::new(NativeAllowance::new(4096).unwrap()).unwrap();
    let mut turn = registry
        .begin_turn(current.clone(), PeerLimits::default())
        .unwrap();
    let mut work = turn.work().unwrap();
    let native = restore(&peer, now, &mut work);
    let read = native.read(&work).unwrap();
    let policy = assert_precharged(&current, || {
        read.delivery_policy(37, peer.authority(), &device, now)
    })
    .unwrap();
    assert_eq!(policy.digest(), digest);
    assert_ne!(
        policy.digest(),
        read.delivery_policy(38, peer.authority(), &device, now)
            .unwrap()
            .digest()
    );
}

#[test]
fn actual_delivery_holder_preserves_policy_errors_before_current_route_denial() {
    use collab_types::caps::{CapOverride, Scope, Subject, Verb};
    for malformed in [false, true] {
        let (mut peer, key, _, mut device, now) = crate::admission_tests::seeded();
        let mut room = peer.admitted_room().unwrap().clone();
        let caps = room.get(ROOT, "capabilities").unwrap().unwrap().1;
        let bytes = if malformed {
            b"not-json".to_vec()
        } else {
            serde_json::to_vec(&CapOverride::grant(
                Subject::Role(collab_types::Role::Editor),
                Scope::App("unsupported.app".into()),
                Verb::Read,
            ))
            .unwrap()
        };
        room.put(&caps, "extra", bytes).unwrap();
        let changes = room
            .get_changes(&[])
            .iter()
            .map(|c| c.raw_bytes().to_vec())
            .collect();
        peer.receive_from(
            TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
            NeighbourMsg::SyncResp {
                room: peer.room().into(),
                changes,
            },
            now,
        )
        .unwrap();
        device.device_public_key = [99; 32];
        let legacy = peer
            .delivery_policy_unqualified(37, peer.authority(), &device, now)
            .err()
            .unwrap();
        assert_eq!(
            legacy,
            if malformed {
                PeerDeliveryPolicyError::Unavailable
            } else {
                PeerDeliveryPolicyError::UnsupportedPolicy
            }
        );
        let current = NativeAllowance::new(64 * 1024 * 1024).unwrap();
        let mut registry = PeerRegistry::new(NativeAllowance::new(4096).unwrap()).unwrap();
        let mut turn = registry
            .begin_turn(current.clone(), PeerLimits::default())
            .unwrap();
        let mut work = turn.work().unwrap();
        let native = restore(&peer, now, &mut work);
        let read = native.read(&work).unwrap();
        ROUTE_CHECKS.with(|n| n.set(0));
        let result = assert_precharged(&current, || {
            read.delivery_policy(37, peer.authority(), &device, now)
        });
        assert_eq!(result.err().unwrap(), legacy);
        assert_eq!(ROUTE_CHECKS.with(|n| n.get()), 0);
    }
}
