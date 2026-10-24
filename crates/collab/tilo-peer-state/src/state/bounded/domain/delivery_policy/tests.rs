use super::*;
use crate::NeighbourMsg;
use crate::PeerLimits;
use crate::SeedDocument;
use crate::SeedState;
use crate::StatePeer;
use crate::TransportOrigin;
use crate::seed_room;
use crate::state::backend::restore::tests::restore_bounded;
use automerge::{AutoCommit, ROOT, ReadDoc, transaction::Transactable};
use tilo_user_identity::RevocationList;
use web_time::Duration;
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
    peer.publication_mut()
        .publish_document("z.app", "tab:2", doc.save(), &key, now)
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
    let native = restore_bounded(&peer, now, &PeerLimits::default());
    let read = native.read();
    ROUTE_CHECKS.with(|n| n.set(0));
    let got = read.delivery_policy(37, &host, &device, now).unwrap();
    assert_eq!(got.digest(), digest);
    assert_eq!(got.room(), peer.room());
    assert_eq!(got.authority_root(), peer.authority().user_public_key);
    assert_eq!(ROUTE_CHECKS.with(std::cell::Cell::get), 2);
    assert_ne!(
        got.digest(),
        read.delivery_policy(37, &device, &host, now)
            .unwrap()
            .digest()
    );
}
fn seed_with_expiring_member() -> (StatePeer, DeviceCert, SystemTime) {
    let (base, key, _, mut device, now) = crate::admission_tests::seeded();
    device.expires_at_ns = Some(
        crate::state::now_ns(now)
            .checked_add(1_000_000_000)
            .unwrap(),
    );
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
                    protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
                    cert: Box::new(peer.authority().clone()),
                    revocations: vec![entry],
                    successions: Vec::new(),
                },
                now,
            )
            .unwrap();
        }
        assert!(matches!(
            peer.delivery_policy_unqualified(37, peer.authority(), &device, at),
            Err(PeerDeliveryPolicyError::CurrentDenied)
        ));
        let native = restore_bounded(&peer, now, &PeerLimits::default());
        let read = native.read();
        assert!(matches!(
            read.delivery_policy(37, peer.authority(), &device, at),
            Err(PeerDeliveryPolicyError::CurrentDenied)
        ));
    }
}
#[test]
fn actual_delivery_holder_preserves_empty_room_semantics_and_limit_refusals() {
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
    let native = restore_bounded(&empty, now, &PeerLimits::default());
    let missing = restore_bounded(&missing, now, &PeerLimits::default());
    let read = native.read();
    // The old projection has no independent membership gate with no routes.
    // This is why its result cannot stand in for authenticated session proof.
    let mut unknown = device.clone();
    unknown.device_public_key = [99; 32];
    ROUTE_CHECKS.with(|n| n.set(0));
    assert!(
        read.delivery_policy(37, empty.authority(), &unknown, now)
            .is_ok()
    );
    assert_eq!(ROUTE_CHECKS.with(std::cell::Cell::get), 0);
    assert!(matches!(
        missing
            .read()
            .delivery_policy(37, empty.authority(), &device, now),
        Err(PeerDeliveryPolicyError::Unavailable)
    ));
    for json in [false, true] {
        let mut limits = PeerLimits::default();
        if json {
            limits.governance_json.allocation_bytes = 1;
        } else {
            limits.governance_collection_bytes = 1;
        }
        // Restoring this room reads no governance JSON; the policy read does.
        let with_routes = restore_bounded(&source, now, &limits);
        let result = with_routes
            .read()
            .delivery_policy(37, source.authority(), &device, now);
        if json {
            assert!(matches!(
                result,
                Err(PeerDeliveryPolicyError::Peer(BoundedPeerError::Decode(_)))
            ));
        } else {
            assert!(matches!(result, Err(PeerDeliveryPolicyError::Capacity)));
        }
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
    peer.publication_mut()
        .publish_document("test.app", "tab:1", changed.save(), &key, now)
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
            revoked_at_ns: u64::from(byte),
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
    let native = restore_bounded(&peer, now, &PeerLimits::default());
    let read = native.read();
    let policy = read
        .delivery_policy(37, peer.authority(), &device, now)
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
    {
        let (mut peer, key, _, mut device, now) = crate::admission_tests::seeded();
        let mut room = peer.admitted_room().unwrap().clone();
        let caps = room.get(ROOT, "grants").unwrap().unwrap().1;
        room.put(&caps, "extra", b"not-json".to_vec()).unwrap();
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
        assert_eq!(legacy, PeerDeliveryPolicyError::Unavailable);
        let native = restore_bounded(&peer, now, &PeerLimits::default());
        let read = native.read();
        ROUTE_CHECKS.with(|n| n.set(0));
        let result = read.delivery_policy(37, peer.authority(), &device, now);
        assert_eq!(result.err().unwrap(), legacy);
        assert_eq!(ROUTE_CHECKS.with(std::cell::Cell::get), 0);
    }
}

#[test]
fn file_and_action_grants_to_apps_leave_the_delivery_policy_available() {
    use collab_types::caps::{Access, CapOverride, Scope, Subject};
    let (mut peer, key, _, device, now) = crate::admission_tests::seeded();
    let mut room = peer.admitted_room().unwrap().clone();
    let sandbox = room
        .put_object(ROOT, "sandbox", automerge::ObjType::Map)
        .unwrap();
    let grant = CapOverride::grant(
        Subject::app("so.tilo.notes"),
        Scope::FsPath("/data/out".into()),
        Access::Write,
    );
    room.put(
        &sandbox,
        "profile:files",
        serde_json::to_vec(&grant).unwrap(),
    )
    .unwrap();
    let caps = room.get(ROOT, "grants").unwrap().unwrap().1;
    let action = tilo_action_policy::Grant::new(
        "app-action",
        tilo_action_policy::Subject::app("so.tilo.notes"),
        tilo_action_policy::TreePath::room(peer.room())
            .app("so.tilo.word-processor")
            .kind("word-processor.document")
            .any(tilo_action_policy::Level::Resource)
            .action("read-text"),
        tilo_action_policy::KindSet::READ,
        tilo_action_policy::Answer::Allow,
        tilo_action_policy::Subject::person("owner"),
    );
    room.put(&caps, "app-action", serde_json::to_vec(&action).unwrap())
        .unwrap();
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
    peer.delivery_policy_unqualified(37, peer.authority(), &device, now)
        .unwrap();
    let native = restore_bounded(&peer, now, &PeerLimits::default());
    native
        .read()
        .delivery_policy(37, peer.authority(), &device, now)
        .unwrap();
}
