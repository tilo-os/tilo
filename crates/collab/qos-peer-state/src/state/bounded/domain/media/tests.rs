use super::*;
use crate::native_footprint_vendor_tests::assert_precharged;
use crate::room::media::{self as policy, read::PolicyValue};
use automerge::{ROOT, ReadDoc, transaction::Transactable};
use ed25519_dalek::SigningKey;
fn fixture() -> (StatePeer, SystemTime) {
    let (peer, _, now) = backend::restore::tests::media_fixture();
    (peer, now)
}
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
#[test]
fn cached_call_matches_actual_signed_complete_policy_and_current_member() {
    let (peer, now) = fixture();
    let current = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let mut registry = PeerRegistry::new(NativeAllowance::new(4096).unwrap()).unwrap();
    let mut turn = registry
        .begin_turn(current.clone(), PeerLimits::default())
        .unwrap();
    let mut work = turn.work().unwrap();
    let native = restore(&peer, now, &mut work);
    let read = native.read(&work).unwrap();
    let policy = peer.validated_media_policy().unwrap().unwrap();
    let call_id = hex::encode([7; 32]);
    let call = assert_precharged(&current, || {
        read.media_call(peer.authority().device_public_key, &call_id, now)
    })
    .unwrap()
    .expect("actual active call");
    assert_eq!(call.epoch(), policy.calls[&[7; 32]].epoch);
    assert_eq!(call.authorization_revision(), policy.authorization_revision);
    assert!(call.publish() && call.subscribe());
    assert!(
        read.media_call([99; 32], &hex::encode([7; 32]), now)
            .is_err()
    );
    assert!(
        read.media_call(peer.authority().device_public_key, "bad", now)
            .unwrap()
            .is_none()
    );
}
#[test]
fn cached_policy_keeps_original_credit_and_new_call_work_uses_current_turn() {
    let (peer, now) = fixture();
    let resident = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let baseline = resident.retained_bytes();
    let mut registry = PeerRegistry::new(NativeAllowance::new(4096).unwrap()).unwrap();
    let native = {
        let mut turn = registry
            .begin_turn(resident.clone(), PeerLimits::default())
            .unwrap();
        restore(&peer, now, &mut turn.work().unwrap())
    };
    let retained = resident.retained_bytes();
    assert!(retained > 0);
    let current = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    {
        let mut limits = PeerLimits::default();
        limits.policy.allocation_bytes = 0;
        let mut turn = registry.begin_turn(current.clone(), limits).unwrap();
        let work = turn.work().unwrap();
        let read = native.read(&work).unwrap();
        let call_id = hex::encode([7; 32]);
        let before = current.retained_bytes();
        let bindings_before = backend::media::binding_checks();
        assert!(
            assert_precharged(&current, || read.media_call(
                peer.authority().device_public_key,
                &call_id,
                now
            ))
            .unwrap()
            .is_some()
        );
        assert_eq!(current.retained_bytes(), before);
        assert_eq!(resident.retained_bytes(), retained);
        for _ in 0..3 {
            assert!(
                read.media_call(peer.authority().device_public_key, &call_id, now)
                    .unwrap()
                    .is_some()
            );
        }
        assert_eq!(backend::media::binding_checks(), bindings_before);
        let all = work.reserve_output(current.available_bytes()).unwrap();
        assert!(matches!(
            read.media_call(peer.authority().device_public_key, &call_id, now),
            Err(BoundedPeerError::Resource(NativeResourceError::WorkBytes))
        ));
        drop(all);
    }
    drop(registry);
    assert!(resident.retained_bytes() > 0);
    drop(native);
    assert_eq!(resident.retained_bytes(), baseline);
}

fn publish_policy(
    peer: &mut StatePeer,
    key: &SigningKey,
    now: SystemTime,
    edit: impl FnOnce(&mut policy::RoomMediaV1),
) {
    let mut room = peer.admitted_room().unwrap().clone();
    let mut value = policy::parse(&room).unwrap().unwrap();
    edit(&mut value);
    room.put(ROOT, policy::FIELD, policy::encode(&value).unwrap())
        .unwrap();
    let changes = room
        .get_changes(&[])
        .iter()
        .map(|change| change.raw_bytes().to_vec())
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
}
#[test]
fn cache_preserves_disabled_missing_encrypted_and_independent_revocation_cases() {
    let (base, now) = fixture();
    for mode in 0..4 {
        let mut peer = base.clone();
        match mode {
            0 => {
                peer.core
                    .room_doc
                    .as_mut()
                    .unwrap()
                    .delete(ROOT, policy::FIELD)
                    .unwrap();
            }
            1 => peer.core.documents.clear(),
            2 => {
                peer.core
                    .documents
                    .values_mut()
                    .next()
                    .unwrap()
                    .encryption_required = true
            }
            _ => {
                let mut entry = tilo_user_identity::RevocationEntry {
                    user_public_key: peer.authority().user_public_key,
                    revoked_device_public_key: [71; 32],
                    revoked_at_ns: 1,
                    reason: tilo_user_identity::RevocationReason::Compromised,
                    signature: [0; 64],
                };
                tilo_user_identity::sign_revocation_entry(
                    &mut entry,
                    &SigningKey::from_bytes(&[1; 32]),
                )
                .unwrap();
                peer.core.revocations.insert(entry).unwrap();
            }
        }
        assert!(peer.validated_media_policy().ok().flatten().is_none());
        let current = NativeAllowance::new(64 * 1024 * 1024).unwrap();
        let mut registry = PeerRegistry::new(NativeAllowance::new(4096).unwrap()).unwrap();
        let mut turn = registry
            .begin_turn(current.clone(), PeerLimits::default())
            .unwrap();
        let mut work = turn.work().unwrap();
        let bytes = peer.durable().unwrap();
        let native = assert_precharged(&current, || {
            BoundedStatePeer::restore(
                &bytes,
                peer.room(),
                peer.authority(),
                &[],
                &RevocationList::new(),
                now,
                &mut work,
            )
        })
        .unwrap();
        assert!(
            native
                .read(&work)
                .unwrap()
                .media_call(
                    peer.authority().device_public_key,
                    &hex::encode([7; 32]),
                    now
                )
                .unwrap()
                .is_none()
        );
    }
}
#[test]
fn actual_published_denial_and_ended_call_match_legacy_epoch_and_directions() {
    for ended in [false, true] {
        let (mut peer, key, now) = backend::restore::tests::media_fixture();
        let root = peer.authority().user_public_key;
        publish_policy(&mut peer, &key, now, |value| {
            let call = value.calls.get_mut(&[7; 32]).unwrap();
            call.epoch += 1;
            if ended {
                call.phase = policy::CallPhaseV1::Ended;
            } else {
                call.denied.insert(
                    root,
                    policy::DirectionsV1 {
                        publish: true,
                        subscribe: false,
                    },
                );
            }
        });
        let legacy = peer.validated_media_policy().unwrap().unwrap();
        let current = NativeAllowance::new(64 * 1024 * 1024).unwrap();
        let mut registry = PeerRegistry::new(NativeAllowance::new(4096).unwrap()).unwrap();
        let mut turn = registry
            .begin_turn(current.clone(), PeerLimits::default())
            .unwrap();
        let mut work = turn.work().unwrap();
        let native = restore(&peer, now, &mut work);
        let view = native.read(&work).unwrap();
        let actual = view
            .media_call(
                peer.authority().device_public_key,
                &hex::encode([7; 32]),
                now,
            )
            .unwrap();
        if ended {
            assert!(actual.is_none());
        } else {
            let actual = actual.unwrap();
            assert_eq!(actual.epoch(), legacy.calls[&[7; 32]].epoch);
            assert!(!actual.publish() && actual.subscribe());
        }
    }
}
#[test]
fn cache_basis_resource_failure_is_not_downgraded_to_unavailable() {
    let (peer, now) = fixture();
    let current = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let mut registry = PeerRegistry::new(NativeAllowance::new(4096).unwrap()).unwrap();
    let mut turn = registry
        .begin_turn(current.clone(), PeerLimits::default())
        .unwrap();
    let mut work = turn.work().unwrap();
    let mut native = restore(&peer, now, &mut work);
    let backend::media::Cache::Ready(held) =
        std::mem::replace(&mut native.media, backend::media::Cache::Disabled)
    else {
        panic!("real cache")
    };
    assert_eq!(held.policy().authorization_revision, 1);
    let encoded = policy::encode(held.policy()).unwrap();
    drop(held);
    work.limits.governance_collection_bytes = 0;
    assert!(matches!(
        assert_precharged(&current, || {
            use policy::read::{BoundedDecode, PolicyDecode};
            let held = BoundedDecode {
                turn: work.turn,
                limits: work.limits.policy,
            }
            .decode(&encoded)
            .unwrap();
            backend::media::cache(&native.core, Some(held), &work)
        }),
        Err(BoundedPeerError::Resource(NativeResourceError::WorkBytes))
    ));
}

fn expiring_fixture(host_expires: bool) -> (StatePeer, [u8; 32], SystemTime) {
    let (old, key, now) = backend::restore::tests::media_fixture();
    let root = SigningKey::from_bytes(&[1; 32]);
    let expires = crate::state::now_ns(now + Duration::from_secs(1));
    let mut host = old.authority().clone();
    if host_expires {
        host.expires_at_ns = Some(expires);
    }
    tilo_user_identity::sign_device_cert(&mut host, &root).unwrap();
    let mut participant = host.clone();
    participant.device_public_key = SigningKey::from_bytes(&[3; 32]).verifying_key().to_bytes();
    participant.expires_at_ns = if host_expires { None } else { Some(expires) };
    tilo_user_identity::sign_device_cert(&mut participant, &root).unwrap();
    let mut peer = StatePeer::new(
        old.room(),
        host.clone(),
        vec![participant.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    peer.install_seed(
        SeedState {
            room_changes: crate::seed_room(old.room(), &host, &[host.clone(), participant.clone()])
                .unwrap(),
            documents: vec![SeedDocument {
                app_id: "dev.qos.chat".into(),
                record_id: "".into(),
                kind: "chat".into(),
                bytes: old.document("dev.qos.chat", "").unwrap(),
            }],
        },
        &key,
        now,
    )
    .unwrap();
    let mut room = peer.admitted_room().unwrap().clone();
    let mut value = old.validated_media_policy().unwrap().unwrap();
    value.authorization_basis = policy::security_basis(&room, &[]).unwrap();
    room.put(ROOT, policy::FIELD, policy::encode(&value).unwrap())
        .unwrap();
    let changes = room
        .get_changes(&[])
        .iter()
        .map(|change| change.raw_bytes().to_vec())
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
    (peer, participant.device_public_key, now)
}
#[test]
fn cached_call_rechecks_real_expiring_host_and_participant_and_current_turn() {
    for host_expires in [false, true] {
        let (peer, participant, now) = expiring_fixture(host_expires);
        let current = NativeAllowance::new(64 * 1024 * 1024).unwrap();
        let mut registry = PeerRegistry::new(NativeAllowance::new(4096).unwrap()).unwrap();
        let mut turn = registry
            .begin_turn(current.clone(), PeerLimits::default())
            .unwrap();
        let mut work = turn.work().unwrap();
        let native = restore(&peer, now, &mut work);
        let read = native.read(&work).unwrap();
        let call_id = hex::encode([7; 32]);
        assert!(
            read.media_call(participant, &call_id, now)
                .unwrap()
                .is_some()
        );
        let later = now + Duration::from_secs(2);
        if host_expires {
            assert!(peer.permissions(participant, later).is_ok());
        }
        assert!(
            peer.permissions(
                if host_expires {
                    peer.authority().device_public_key
                } else {
                    participant
                },
                later
            )
            .is_err()
        );
        assert!(read.media_call(participant, &call_id, later).is_err());
        let mut foreign = PeerRegistry::new(NativeAllowance::new(4096).unwrap()).unwrap();
        let mut foreign_turn = foreign
            .begin_turn(NativeAllowance::new(4096).unwrap(), PeerLimits::default())
            .unwrap();
        assert!(native.read(&foreign_turn.work().unwrap()).is_err());
        current.finish_turn();
        assert!(read.media_call(participant, &call_id, now).is_err());
    }
}
