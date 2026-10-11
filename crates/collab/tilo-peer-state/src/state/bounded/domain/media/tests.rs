use super::*;
use crate::NeighbourMsg;
use crate::PeerLimits;
use crate::SeedDocument;
use crate::SeedState;
use crate::StatePeer;
use crate::TransportOrigin;
use crate::room::media::{self as policy, read::PolicyValue};
use crate::state::backend::restore::tests::restore_bounded;
use automerge::{ROOT, transaction::Transactable};
use ed25519_dalek::SigningKey;
use tilo_user_identity::RevocationList;
use web_time::Duration;
fn fixture() -> (StatePeer, SystemTime) {
    let (peer, _, now) = backend::restore::tests::media_fixture();
    (peer, now)
}
#[test]
fn cached_call_matches_actual_signed_complete_policy_and_current_member() {
    let (peer, now) = fixture();
    let native = restore_bounded(&peer, now, &PeerLimits::default());
    let read = native.read();
    let policy = peer.validated_media_policy().unwrap().unwrap();
    let call_id = hex::encode([7; 32]);
    let call = read
        .media_call(peer.authority().device_public_key, &call_id, now)
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
fn cached_call_reuses_restored_readiness_without_rechecking_bindings() {
    let (peer, now) = fixture();
    let native = restore_bounded(&peer, now, &PeerLimits::default());
    let read = native.read();
    let call_id = hex::encode([7; 32]);
    let bindings_before = backend::media::binding_checks();
    for _ in 0..3 {
        assert!(
            read.media_call(peer.authority().device_public_key, &call_id, now)
                .unwrap()
                .is_some()
        );
    }
    assert_eq!(backend::media::binding_checks(), bindings_before);
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
                    .encryption_required = true;
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
        let native = restore_bounded(&peer, now, &PeerLimits::default());
        assert!(
            native
                .read()
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
        let native = restore_bounded(&peer, now, &PeerLimits::default());
        let view = native.read();
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
fn cache_basis_capacity_failure_is_not_downgraded_to_unavailable() {
    let (peer, now) = fixture();
    let mut native = restore_bounded(&peer, now, &PeerLimits::default());
    let backend::media::Cache::Ready(held) =
        std::mem::replace(&mut native.media, backend::media::Cache::Disabled)
    else {
        panic!("real cache")
    };
    assert_eq!(held.policy().authorization_revision, 1);
    let encoded = policy::encode(held.policy()).unwrap();
    drop(held);
    let limits = PeerLimits {
        governance_collection_bytes: 0,
        ..PeerLimits::default()
    };
    let held = {
        use policy::read::{BoundedDecode, PolicyDecode};
        BoundedDecode {
            limits: limits.policy,
        }
        .decode(&encoded)
        .unwrap()
    };
    assert!(matches!(
        backend::media::cache(&native.core, Some(held), &limits),
        Err(BoundedPeerError::Capacity)
    ));
}

#[expect(
    clippy::similar_names,
    reason = "`root` and `room` are the domain's own names"
)]
fn expiring_fixture(host_expires: bool) -> (StatePeer, [u8; 32], SystemTime) {
    let (old, key, now) = backend::restore::tests::media_fixture();
    let root = SigningKey::from_bytes(&[1; 32]);
    let expires = crate::state::now_ns(now.checked_add(Duration::from_secs(1)).unwrap());
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
                app_id: "so.tilo.chat".into(),
                record_id: String::new(),
                kind: "chat".into(),
                bytes: old.document("so.tilo.chat", "").unwrap(),
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
fn cached_call_rechecks_real_expiring_host_and_participant() {
    for host_expires in [false, true] {
        let (peer, participant, now) = expiring_fixture(host_expires);
        let native = restore_bounded(&peer, now, &PeerLimits::default());
        let read = native.read();
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
        assert!(
            read.media_call(participant, &call_id, now)
                .unwrap()
                .is_some()
        );
    }
}
