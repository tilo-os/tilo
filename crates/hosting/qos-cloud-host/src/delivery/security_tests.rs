use super::*;
use crate::*;
use qos_user_identity::{RevocationEntry, RevocationReason, sign_revocation_entry};

#[test]
fn delivery_security_host_revocation_survives_without_revoked_signature_or_replay() {
    for media in [false, true] {
        let (dir, mut store, caller) = crate::actions::media_tests::voice_store();
        if media {
            store
                .action(
                    caller.device_public_key,
                    &ActionRequest {
                        version: 1,
                        room: store.state.room.clone(),
                        invocation_id: "media-before-revocation".into(),
                        operation: NativeOperation::MediaCreate(
                            qos_action_broker::collaboration_admin::MediaCreateInput {
                                app_id: "so.tilo.chat".into(),
                                record_id: "".into(),
                                channel_id: "cloud_voice".into(),
                                expected_authorization_revision: None,
                                allowed:
                                    qos_action_broker::collaboration_admin::MediaDirectionsInput {
                                        publish: true,
                                        subscribe: true,
                                    },
                            },
                        ),
                    },
                    qos_relay::clock::now_ns(),
                )
                .unwrap();
        }
        let source = crate::https_owner::CloudStoreSource::bind(&store).unwrap();
        let before = store.state.delivery.as_ref().unwrap().clone();
        let generation = store.state.generation;
        let root = SigningKey::from_bytes(&[7; 32]);
        let now = qos_relay::clock::now_ns();
        let mut entry = RevocationEntry {
            user_public_key: root.verifying_key().to_bytes(),
            revoked_device_public_key: store.key().verifying_key().to_bytes(),
            revoked_at_ns: now,
            reason: RevocationReason::Lost,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut entry, &root).unwrap();
        store.receive(TransportOrigin::RelayDelta(caller.device_public_key), NeighbourMsg::IdentityHello {
            cert: Box::new(caller), revocations: vec![entry.clone()],
        }, now + 1).expect("verified host revocation must reach the original barrier without the revoked host signing");
        let after = store.state.delivery.as_ref().unwrap();
        assert_eq!(store.state.generation, generation + 1);
        assert_eq!(after.policy_lineage, before.policy_lineage + 1);
        assert_eq!(after.ledger.resolved_through, generation + 1);
        assert_eq!(after.ledger.replay, before.ledger.replay);
        assert!(after.queue == before.queue);
        assert!(
            after.pool.encode(schema::POOL_LIMIT).unwrap()
                == before.pool.encode(schema::POOL_LIMIT).unwrap()
        );
        assert!(
            store
                .state
                .bound
                .as_ref()
                .unwrap()
                .revocations
                .contains(&entry)
        );
        let saved = std::fs::read(dir.path().join("state.json")).unwrap();
        drop(store);
        assert!(Store::open(dir.path(), "agent-admission").is_err());
        assert!(matches!(
            source.with_current(|_| ()),
            crate::https_owner::Current::Denied
        ));
        assert!(std::fs::read(dir.path().join("state.json")).unwrap() == saved);
    }
}

#[test]
fn delivery_security_document_change_without_publication_is_not_metadata() {
    let (dir, mut store, _) = crate::actions::media_tests::voice_store();
    let mut peer = store.peer().unwrap();
    let document = peer.document("so.tilo.chat", "").unwrap();
    peer.publish_document(
        "so.tilo.chat",
        "",
        document,
        &store.key(),
        now_time(qos_relay::clock::now_ns()),
    )
    .unwrap();
    let mut next = store.state.clone();
    next.bound.as_mut().unwrap().peer = peer.durable().unwrap();
    let bytes = std::fs::read(dir.path().join("state.json")).unwrap();
    assert!(store.commit(next).is_err());
    assert!(std::fs::read(dir.path().join("state.json")).unwrap() == bytes);
}

#[test]
fn delivery_security_proof_cannot_authorize_an_added_document_effect() {
    let (dir, mut store, caller) = crate::actions::media_tests::voice_store();
    let now = qos_relay::clock::now_ns();
    let before = store.peer_at(now).unwrap();
    let mut clean = before.clone();
    let mut tampered = before.clone();
    let document = tampered.document("so.tilo.chat", "").unwrap();
    tampered
        .publish_document("so.tilo.chat", "", document, &store.key(), now_time(now))
        .unwrap();
    let mut entry = RevocationEntry {
        user_public_key: caller.user_public_key,
        revoked_device_public_key: store.key().verifying_key().to_bytes(),
        revoked_at_ns: now,
        reason: RevocationReason::Lost,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut entry, &SigningKey::from_bytes(&[7; 32])).unwrap();
    for candidate in [&mut clean, &mut tampered] {
        candidate
            .receive_from(
                TransportOrigin::RelayDelta(caller.device_public_key),
                NeighbourMsg::IdentityHello {
                    cert: Box::new(caller.clone()),
                    revocations: vec![entry.clone()],
                },
                now_time(now + 1),
            )
            .unwrap();
    }
    let proof = before.verify_security_metadata_only(&clean).unwrap();
    let bytes = std::fs::read(dir.path().join("state.json")).unwrap();
    assert!(
        store
            .commit_verified_security_metadata_only(tampered, proof, now + 1)
            .is_err()
    );
    assert!(std::fs::read(dir.path().join("state.json")).unwrap() == bytes);
}
