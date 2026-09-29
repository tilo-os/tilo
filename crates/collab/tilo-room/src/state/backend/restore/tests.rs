use super::*;
use crate::NeighbourMsg;
use crate::PeerLimits;
use crate::SeedDocument;
use crate::SeedState;
use crate::StatePeer;
use crate::TransportOrigin;
use crate::seed_room;
use crate::state::DurableDocument;
use crate::state::DurableV4;
use crate::state::DurableV5;
use crate::state::GovernedIdentity;
use automerge::AutoCommit;
use automerge::ObjType;
use automerge::ROOT;
use automerge::ReadDoc;
use automerge::transaction::Transactable;
use ed25519_dalek::SigningKey;

#[test]
fn complete_bounded_restore_preserves_v4_v5_documents_and_metadata() {
    let (mut peer, key, _, _, now) = crate::admission_tests::seeded();
    let v4 = peer.durable().unwrap();
    peer.select_admission_v5().unwrap();
    peer.publish_document(
        "test.app",
        "tab:1",
        peer.document("test.app", "tab:1").unwrap(),
        &key,
        now,
    )
    .unwrap();
    let v5 = peer.durable().unwrap();
    for bytes in [v4, v5] {
        let expected = StatePeer::restore(
            &bytes,
            peer.room(),
            peer.authority().clone(),
            peer.certificates(),
            RevocationList::new(),
            now,
        )
        .unwrap();
        let roster = peer.certificates();
        let restored = BoundedStatePeer::restore(
            &bytes,
            peer.room(),
            peer.authority(),
            &roster,
            &RevocationList::new(),
            now,
            &PeerLimits::default(),
        )
        .unwrap();
        assert_eq!(restored.core.room, expected.room());
        assert_eq!(&restored.core.authority, expected.authority());
        assert_eq!(restored.core.documents.len(), expected.core.documents.len());
        assert_eq!(restored.core.pending_bytes, expected.core.pending_bytes);
        assert_eq!(restored.core.admission_v5, expected.core.admission_v5);
        for (route, doc) in &restored.core.documents {
            let mut old = expected.core.documents[route].doc.clone();
            assert_eq!(doc.doc.heads(), old.get_heads());
            assert_eq!(
                doc.channel.last_seq_per_author,
                expected.core.documents[route].channel.last_seq_per_author
            );
        }
        drop(bytes);
        drop(roster);
        drop(expected);
        assert_eq!(restored.core.room, peer.room());
    }
}

#[test]
fn complete_bounded_restore_retains_an_actual_empty_enrolled_peer() {
    let (peer, _, _, _, now) = crate::admission_tests::seeded();
    let empty = StatePeer::new(
        peer.room(),
        peer.authority().clone(),
        peer.certificates(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    let bytes = empty.durable().unwrap();
    let restored = BoundedStatePeer::restore(
        &bytes,
        peer.room(),
        peer.authority(),
        &[],
        &RevocationList::new(),
        now,
        &PeerLimits::default(),
    )
    .unwrap();
    assert!(restored.core.room_doc.is_none());
    assert!(restored.core.documents.is_empty());
    assert_eq!(restored.core.authority, *peer.authority());
}

#[expect(
    clippy::panic_in_result_fn,
    reason = "test helper: asserts legacy parity before returning the restore result"
)]
fn checked(
    bytes: &[u8],
    peer: &StatePeer,
    now: SystemTime,
) -> Result<BoundedStatePeer, BoundedPeerError> {
    let legacy = StatePeer::restore(
        bytes,
        peer.room(),
        peer.authority().clone(),
        vec![],
        RevocationList::new(),
        now,
    );
    let native = BoundedStatePeer::restore(
        bytes,
        peer.room(),
        peer.authority(),
        &[],
        &RevocationList::new(),
        now,
        &PeerLimits::default(),
    );
    assert_eq!(
        native.is_ok(),
        legacy.is_ok(),
        "native={:?}, legacy={:?}",
        native.as_ref().err(),
        legacy.as_ref().err()
    );
    native
}
pub(in crate::state) fn media_fixture() -> (StatePeer, SigningKey, SystemTime) {
    let (source, key, _, _, now) = crate::admission_tests::seeded();
    let cert = source.authority().clone();
    let mut peer = StatePeer::new(
        source.room(),
        cert.clone(),
        vec![],
        RevocationList::new(),
        now,
    )
    .unwrap();
    let mut chat = AutoCommit::new();
    let channels = chat.put_object(ROOT, "channels", ObjType::Map).unwrap();
    let voice = chat.put_object(&channels, "voice", ObjType::Map).unwrap();
    chat.put(&voice, "kind", "voice").unwrap();
    chat.put(&voice, "workspace", "home").unwrap();
    peer.install_seed(
        SeedState {
            room_changes: seed_room(source.room(), &cert, std::slice::from_ref(&cert)).unwrap(),
            documents: vec![SeedDocument {
                app_id: "so.tilo.chat".into(),
                record_id: String::new(),
                kind: "chat".into(),
                bytes: chat.save(),
            }],
        },
        &key,
        now,
    )
    .unwrap();
    let binding = media::resolve_chat_binding("so.tilo.chat", "", "voice", &chat).unwrap();
    let mut room = peer.admitted_room().unwrap().clone();
    let policy = media::RoomMediaV1 {
        version: 1,
        room: *uuid::Uuid::parse_str(peer.room()).unwrap().as_bytes(),
        authorization_revision: 1,
        authorization_basis: media::security_basis(&room, &[]).unwrap(),
        calls: BTreeMap::from([(
            [7; 32],
            media::CallV1 {
                binding,
                epoch: 1,
                phase: media::CallPhaseV1::Active,
                allowed: media::DirectionsV1 {
                    publish: true,
                    subscribe: true,
                },
                denied: BTreeMap::new(),
            },
        )]),
    };
    room.put(ROOT, media::FIELD, media::encode(&policy).unwrap())
        .unwrap();
    room.commit();
    let changes = room
        .get_changes(&[])
        .into_iter()
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
    (peer, key, now)
}
#[test]
fn complete_bounded_restore_validates_present_media_and_exact_chat_without_requiring_missing_chat()
{
    let (peer, _, now) = media_fixture();
    let bytes = peer.durable().unwrap();
    let good = checked(&bytes, &peer, now).unwrap();
    assert_eq!(good.core.documents.len(), 1);
    // A valid room-first snapshot can restore data before its Chat arrives.
    let mut durable: DurableV4 = postcard::from_bytes(&bytes).unwrap();
    durable.base.documents.clear();
    checked(&postcard::to_allocvec(&durable).unwrap(), &peer, now).unwrap();
    // Existing lazy checkpoints remain original, even when the live Chat is
    // changed. Replacing the actual selected channel object must still fail.
    let mut durable: DurableV4 = postcard::from_bytes(&bytes).unwrap();
    let mut chat = AutoCommit::load(&durable.base.documents[0].bytes).unwrap();
    let channels = chat.get(ROOT, "channels").unwrap().unwrap().1;
    chat.delete(&channels, "voice").unwrap();
    let voice = chat.put_object(&channels, "voice", ObjType::Map).unwrap();
    chat.put(&voice, "kind", "voice").unwrap();
    chat.put(&voice, "workspace", "home").unwrap();
    durable.base.documents[0].bytes = chat.save();
    assert!(matches!(
        checked(&postcard::to_allocvec(&durable).unwrap(), &peer, now),
        Err(BoundedPeerError::Invalid(BoundedPeerInvalid::Media))
    ));
    for mode in 0..3 {
        let mut durable: DurableV4 = postcard::from_bytes(&bytes).unwrap();
        let mut room = AutoCommit::load(durable.base.room_bytes.as_ref().unwrap()).unwrap();
        if mode == 0 {
            room.put(ROOT, media::FIELD, "not policy").unwrap();
        } else {
            let mut policy = media::parse(&room).unwrap().unwrap();
            if mode == 1 {
                policy.authorization_basis[0] ^= 1;
            } else {
                policy.room = [6; 16];
            }
            room.put(ROOT, media::FIELD, media::encode(&policy).unwrap())
                .unwrap();
        }
        durable.base.room_bytes = Some(room.save());
        assert!(matches!(
            checked(&postcard::to_allocvec(&durable).unwrap(), &peer, now),
            Err(BoundedPeerError::Invalid(BoundedPeerInvalid::Media))
        ));
    }
}
#[test]
fn complete_bounded_restore_rejects_namespace_certificates_pending_coverage_and_exact_floor_failures()
 {
    let (mut peer, key, _, _, now) = crate::admission_tests::seeded();
    let bytes = peer.durable().unwrap();
    for mode in 0..5 {
        let mut durable: DurableV4 = postcard::from_bytes(&bytes).unwrap();
        match mode {
            0 => durable.base.room = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".into(),
            1 => durable.base.certificates[0].signature[0] ^= 1,
            2 => durable.pending_bytes = 1,
            3 => durable.base.documents[0].bytes = vec![1, 2, 3],
            _ => durable.base.documents[0].covered_snapshot = Some(vec![9, 8, 7]),
        }
        assert!(checked(&postcard::to_allocvec(&durable).unwrap(), &peer, now).is_err());
    }
    peer.select_admission_v5().unwrap();
    peer.publish_document(
        "test.app",
        "tab:1",
        peer.document("test.app", "tab:1").unwrap(),
        &key,
        now,
    )
    .unwrap();
    let bytes = peer.durable().unwrap();
    let mut durable: DurableV5<'_> = postcard::from_bytes(&bytes).unwrap();
    for doc in &mut durable.base.base.documents {
        doc.watermarks.clear();
    }
    assert!(matches!(
        checked(&postcard::to_allocvec(&durable).unwrap(), &peer, now),
        Err(BoundedPeerError::Invalid(BoundedPeerInvalid::ExactFloor))
    ));
}
#[test]
fn complete_bounded_restore_keeps_original_lazy_checkpoint_bytes_and_historical_expiry() {
    let (peer, _, _, _, now) = crate::admission_tests::seeded();
    let mut durable: DurableV4 = postcard::from_bytes(&peer.durable().unwrap()).unwrap();
    let mut room = AutoCommit::load(durable.base.room_bytes.as_ref().unwrap()).unwrap();
    room.delete(ROOT, crate::coverage::FIELD_V2).unwrap();
    durable.base.room_bytes = Some(room.save());
    let opaque = vec![99; 512];
    durable.base.documents[0].covered_snapshot = Some(opaque.clone());
    let restored = checked(&postcard::to_allocvec(&durable).unwrap(), &peer, now).unwrap();
    assert_eq!(
        restored
            .core
            .documents
            .values()
            .next()
            .unwrap()
            .covered_snapshot
            .as_ref()
            .unwrap()
            .original,
        opaque
    );
    // Restore uses historical signed certificates; actual current access will
    // independently recheck expiry. A bound does not silently rewrite that rule.
    let mut durable: DurableV4 = postcard::from_bytes(&peer.durable().unwrap()).unwrap();
    for cert in &mut durable.base.certificates {
        if cert.device_public_key != peer.authority().device_public_key {
            cert.expires_at_ns = Some(now_ns(now));
            tilo_user_identity::sign_device_cert(cert, &SigningKey::from_bytes(&[1; 32])).unwrap();
        }
    }
    checked(&postcard::to_allocvec(&durable).unwrap(), &peer, now).unwrap();
}
#[test]
fn complete_bounded_restore_document_byte_limit_spans_room_and_all_documents() {
    let (peer, _, _, _, now) = crate::admission_tests::seeded();
    let mut durable: DurableV4 = postcard::from_bytes(&peer.durable().unwrap()).unwrap();
    let first = &durable.base.documents[0];
    durable.base.documents.push(DurableDocument {
        route: Route {
            app: first.route.app.clone(),
            record: "second".into(),
        },
        bytes: first.bytes.clone(),
        watermarks: first.watermarks.clone(),
        encryption_required: first.encryption_required,
        covered: first.covered.clone(),
        covered_snapshot: None,
    });
    let bytes = postcard::to_allocvec(&durable).unwrap();
    assert!(
        StatePeer::restore(
            &bytes,
            peer.room(),
            peer.authority().clone(),
            vec![],
            RevocationList::new(),
            now
        )
        .is_ok()
    );
    let total = durable
        .base
        .room_bytes
        .iter()
        .map(Vec::len)
        .chain(durable.base.documents.iter().map(|d| d.bytes.len()))
        .sum::<usize>();
    let restore = |normalized_bytes| {
        BoundedStatePeer::restore(
            &bytes,
            peer.room(),
            peer.authority(),
            &[],
            &RevocationList::new(),
            now,
            &PeerLimits {
                normalized_bytes,
                ..PeerLimits::default()
            },
        )
    };
    assert!(matches!(
        restore(total - 1),
        Err(BoundedPeerError::Capacity)
    ));
    assert_eq!(restore(total).unwrap().core.documents.len(), 2);
}

#[test]
#[expect(
    clippy::similar_names,
    reason = "`root` and `room` are the domain's own names"
)]
fn complete_bounded_restore_preserves_local_revocation_union_and_rejects_governed_host_revocation()
{
    let (peer, _, now) = media_fixture();
    let bytes = peer.durable().unwrap();
    let root = SigningKey::from_bytes(&[1; 32]);
    let mut entry = tilo_user_identity::RevocationEntry {
        user_public_key: peer.authority().user_public_key,
        revoked_device_public_key: [71; 32],
        revoked_at_ns: 1,
        reason: tilo_user_identity::RevocationReason::Compromised,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut entry, &root).unwrap();
    let mut durable: DurableV4 = postcard::from_bytes(&bytes).unwrap();
    durable.base.revocations.push(entry.clone());
    let local_bytes = postcard::to_allocvec(&durable).unwrap();
    let restored = checked(&local_bytes, &peer, now).unwrap();
    assert!(
        restored
            .core
            .revocations
            .is_revoked(&entry.revoked_device_public_key)
    );
    let legacy = StatePeer::restore(
        &local_bytes,
        peer.room(),
        peer.authority().clone(),
        vec![],
        RevocationList::new(),
        now,
    )
    .unwrap();
    assert!(
        legacy.validated_media_policy().is_err(),
        "local union restores data but requires real media reconciliation"
    );
    let mut durable: DurableV4 = postcard::from_bytes(&bytes).unwrap();
    let mut room = AutoCommit::load(durable.base.room_bytes.as_ref().unwrap()).unwrap();
    let identities = room.get(ROOT, "peer_identities_v1").unwrap().unwrap().1;
    let id = hex::encode(peer.authority().device_public_key);
    let (value, _) = room.get(&identities, &id).unwrap().unwrap();
    let mut row: GovernedIdentity = serde_json::from_slice(value.as_bytes().unwrap()).unwrap();
    entry.revoked_device_public_key = peer.authority().device_public_key;
    tilo_user_identity::sign_revocation_entry(&mut entry, &root).unwrap();
    row.revocations.push(entry);
    room.put(&identities, &id, serde_json::to_vec(&row).unwrap())
        .unwrap();
    room.commit();
    let mut policy = media::parse(&room).unwrap().unwrap();
    policy.authorization_basis = media::security_basis(&room, &[]).unwrap();
    room.put(ROOT, media::FIELD, media::encode(&policy).unwrap())
        .unwrap();
    durable.base.room_bytes = Some(room.save());
    assert!(matches!(
        checked(&postcard::to_allocvec(&durable).unwrap(), &peer, now),
        Err(BoundedPeerError::Invalid(
            BoundedPeerInvalid::AuthorityRevoked
        ))
    ));
}
