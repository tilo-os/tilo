use super::*;
use crate::*;
use atomic_file::test_support::{Checkpoint, with_failure};
use qos_peer_state::owner_commit::PrivateAgentState;

#[test]
fn delivery_v6_baseline_generation_is_explicit_and_old_inner_preserves_bytes() {
    let (dir, mut store, _) = crate::agent_owner_tests::enrolled();
    let activation = store.state.generation + 1;
    store
        .activate_delivery_storage(qos_relay::clock::now_ns())
        .unwrap();
    let selected = serde_json::to_value(&store.state).unwrap();
    assert_eq!(selected["delivery"]["version"], 2);
    assert_eq!(selected["delivery"]["baseline_generation"], activation);
    let mut next = store.state.clone();
    next.consumed.insert(
        "baseline-progress".into(),
        ("metadata".into(), serde_json::Value::Null),
    );
    store.commit(next).unwrap();
    assert_eq!(
        serde_json::to_value(&store.state).unwrap()["delivery"]["baseline_generation"],
        activation
    );
    let current = serde_json::to_value(&store.state).unwrap();
    drop(store);
    for case in 0..5 {
        let mut invalid = current.clone();
        match case {
            0 => {
                invalid["delivery"]["version"] = 1.into();
                invalid["delivery"]
                    .as_object_mut()
                    .unwrap()
                    .remove("baseline_generation");
            }
            1 => {
                invalid["delivery"]
                    .as_object_mut()
                    .unwrap()
                    .remove("baseline_generation");
            }
            2 => invalid["delivery"]["baseline_generation"] = 0.into(),
            3 => invalid["delivery"]["baseline_generation"] = (activation + 1).into(),
            4 => invalid["delivery"]["baseline_generation"] = u64::MAX.into(),
            _ => unreachable!(),
        }
        let bytes = serde_json::to_vec(&invalid).unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, &bytes).unwrap();
        assert!(Store::open(dir.path(), "agent-admission").is_err());
        assert!(
            std::fs::read(path).unwrap() == bytes,
            "unsupported baseline metadata must not be rewritten"
        );
    }
}

#[test]
fn delivery_v6_literal_v4_v5_open_preserves_bytes_and_private_selection() {
    for (version, bytes) in [
        (4, include_bytes!("fixtures/v4.json").as_slice()),
        (5, include_bytes!("fixtures/v5.json").as_slice()),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let store = Store::open(dir.path(), "delivery-literal").unwrap();
        assert_eq!(store.state.version, version);
        assert_eq!(store.state.room, "11111111-2222-4333-8444-555555555555");
        assert_eq!(store.state.host_secret, [1; 32]);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert!(matches!(
            (&store.state.private_agent_ledger, version),
            (None, 4) | (Some(PrivateAgentState::V1(_)), 5)
        ));
        assert!(
            serde_json::to_value(&store.state)
                .unwrap()
                .get("delivery")
                .is_none()
        );
    }
}

#[test]
fn delivery_v6_explicit_activation_preserves_each_private_schema_and_reopens() {
    for private in [0, 1, 2] {
        let (dir, mut store, device) = crate::agent_owner_tests::enrolled();
        let caller = store.agent_caller(device).unwrap();
        match private {
            1 => store.activate_private_agent_ledger(&caller).unwrap(),
            2 => store.activate_private_agent_runtime_v2(&caller).unwrap(),
            _ => (),
        }
        let original_private = serde_json::to_value(&store.state.private_agent_ledger).unwrap();
        let original_room = store.state.room.clone();
        let original_key = store.state.host_secret;
        let generation = store.state.generation;
        store
            .activate_delivery_storage(qos_relay::clock::now_ns())
            .unwrap();
        assert_eq!(store.state.version, 6);
        assert_eq!(store.state.generation, generation + 1);
        assert_eq!(
            serde_json::to_value(&store.state.private_agent_ledger).unwrap(),
            original_private
        );
        assert_eq!(store.state.room, original_room);
        assert_eq!(store.state.host_secret, original_key);
        let state = serde_json::to_value(&store.state).unwrap();
        assert_eq!(
            state["delivery"]["prior_private_schema"],
            if private == 0 { 4 } else { 5 }
        );
        assert!(
            state["delivery"]["transport_hint"].is_null(),
            "storage is not installation"
        );
        assert!(store.peer().unwrap().admission_v5_selected());
        let bytes = std::fs::read(dir.path().join("state.json")).unwrap();
        store
            .activate_delivery_storage(qos_relay::clock::now_ns())
            .unwrap();
        assert_eq!(std::fs::read(dir.path().join("state.json")).unwrap(), bytes);
        drop(store);
        let reopened = Store::open(dir.path(), "agent-admission").unwrap();
        assert_eq!(reopened.state.version, 6);
        assert_eq!(
            serde_json::to_value(&reopened.state.private_agent_ledger).unwrap(),
            original_private
        );
        assert_eq!(std::fs::read(dir.path().join("state.json")).unwrap(), bytes);
    }
}

#[test]
fn delivery_v6_activation_failure_uses_real_store_barrier_and_reconciliation() {
    for (checkpoint, published) in [
        (Checkpoint::BeforePublish, false),
        (Checkpoint::BeforeCommit, true),
    ] {
        let (dir, mut store, _) = crate::agent_owner_tests::enrolled();
        let before = store.state.generation;
        let error = with_failure(checkpoint, || {
            store.activate_delivery_storage(qos_relay::clock::now_ns())
        })
        .unwrap_err();
        assert!(
            error.downcast_ref::<StoreCommitFailure>().is_some(),
            "actual Store barrier failure: {error}"
        );
        assert!(store.peer().is_err(), "uncertain owner must not authorize");
        drop(store);
        let reopened = Store::open(dir.path(), "agent-admission").unwrap();
        assert_eq!(reopened.state.version, if published { 6 } else { 4 });
        assert_eq!(reopened.state.generation, before + u64::from(published));
    }
}

#[test]
fn delivery_v6_later_private_selection_keeps_outer_version_and_public_lineage() {
    let (_dir, mut store, device) = crate::agent_owner_tests::enrolled();
    store
        .activate_delivery_storage(qos_relay::clock::now_ns())
        .unwrap();
    let caller = store.agent_caller(device).unwrap();
    let before = store.state.delivery.as_ref().unwrap();
    let replay_count = before.ledger.replay.len();
    let lineage = before.policy_lineage;
    store.activate_private_agent_ledger(&caller).unwrap();
    assert_eq!(store.state.version, VERSION);
    assert_eq!(store.state.private_schema_version().unwrap(), 5);
    assert!(matches!(
        store.state.private_agent_ledger,
        Some(PrivateAgentState::V1(_))
    ));
    let (caller, request) = crate::agent_owner_tests::prepare(&store, device, "v6-private");
    store.commit_agent_request(&request, &caller).unwrap();
    store.activate_private_agent_runtime_v2(&caller).unwrap();
    assert!(matches!(
        store.state.private_agent_ledger,
        Some(PrivateAgentState::V2(_))
    ));
    assert_eq!(store.state.version, VERSION);
    let after = store.state.delivery.as_ref().unwrap();
    assert_eq!(
        after.ledger.replay.len(),
        replay_count,
        "private facts are not public replay"
    );
    assert_eq!(after.policy_lineage, lineage);
    assert_eq!(after.ledger.resolved_through, store.state.generation);
}

fn admit_guest(store: &mut Store, source: [u8; 32], now: u64) -> [u8; 32] {
    use qos_peer_state::join::*;
    use qos_user_identity::{DeviceCapabilities, sign_device_cert};
    let root = SigningKey::from_bytes(&[61; 32]);
    let key = SigningKey::from_bytes(&[62; 32]);
    let output = store
        .action(
            source,
            &ActionRequest {
                version: 1,
                room: store.state.room.clone(),
                invocation_id: "delivery-guest".into(),
                operation: NativeOperation::CollaborationInvite {
                    role: MemberRole::Editor,
                    recipient_user_public_key: Some(root.verifying_key().to_bytes()),
                    invitee_name: Some("Delivery guest".into()),
                    expires_at_ns: now + 600_000_000_000,
                },
            },
            now,
        )
        .unwrap()
        .output
        .unwrap();
    let invitation = decode_and_verify_invitation_text(
        output["signed_invitation"].as_str().unwrap(),
        Some(root.verifying_key().to_bytes()),
        &RevocationList::new(),
        now,
    )
    .unwrap()
    .signed_invitation()
    .to_vec();
    let mut certificate = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "Delivery guest".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: now,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut certificate, &root).unwrap();
    let challenge = store
        .join_challenge(
            certificate.device_public_key,
            JoinChallengeRequest {
                signed_invitation: invitation,
                device_cert: certificate.clone(),
                request_id: [61; 32],
            },
            now,
        )
        .unwrap();
    let proof = sign_join_proof(challenge.challenge, &key).unwrap();
    store
        .join_complete(
            certificate.device_public_key,
            JoinCompleteRequest { proof },
            now,
        )
        .unwrap();
    root.verifying_key().to_bytes()
}

#[test]
fn delivery_v6_policy_lineage_fences_cross_writer_aba_but_not_documents() {
    use qos_peer_state::join::MemberRole;
    let (dir, mut store, source) = crate::agent_owner_tests::enrolled();
    let now = qos_relay::clock::now_ns();
    let guest = admit_guest(&mut store, source, now);
    store.activate_delivery_storage(now).unwrap();
    let original = store
        .state
        .delivery
        .as_ref()
        .unwrap()
        .policy_revision
        .clone();
    let lineage = store.state.delivery.as_ref().unwrap().policy_lineage;
    let room = store.state.room.clone();
    store
        .action(
            source,
            &ActionRequest {
                version: 1,
                room: room.clone(),
                invocation_id: "delivery-doc".into(),
                operation: NativeOperation::WriterInsert {
                    text: "does not change policy".into(),
                },
            },
            now,
        )
        .unwrap();
    assert_eq!(
        store.state.delivery.as_ref().unwrap().policy_lineage,
        lineage
    );
    assert_eq!(
        store.state.delivery.as_ref().unwrap().policy_revision,
        original
    );
    drop(store);
    for (invocation, role, advance) in [
        ("delivery-demote", MemberRole::Viewer, 1),
        ("delivery-restore", MemberRole::Editor, 2),
    ] {
        // Different Store transactions model actual independently reopening writer paths.
        let mut writer = Store::open(dir.path(), "agent-admission").unwrap();
        writer
            .action(
                source,
                &ActionRequest {
                    version: 1,
                    room: room.clone(),
                    invocation_id: invocation.into(),
                    operation: NativeOperation::CollaborationSetRole {
                        user_public_key: guest,
                        role,
                    },
                },
                now,
            )
            .unwrap();
        assert_eq!(
            writer.state.delivery.as_ref().unwrap().policy_lineage,
            lineage + advance
        );
        if advance == 2 {
            assert_eq!(
                writer.state.delivery.as_ref().unwrap().policy_revision,
                original,
                "actual policy returned to A"
            );
        }
        assert!(
            writer.state.outbox.is_empty(),
            "one durable publication pool"
        );
    }
    let final_store = Store::open(dir.path(), "agent-admission").unwrap();
    assert_eq!(
        final_store.state.delivery.as_ref().unwrap().policy_lineage,
        lineage + 2
    );
}

#[test]
fn delivery_v6_queue_drain_retains_original_pool_and_replay() {
    let (_dir, mut store, _) = crate::agent_owner_tests::enrolled();
    let pending = store
        .state
        .outbox
        .iter()
        .map(|item| (postcard::to_stdvec(&item.message).unwrap(), item.kind))
        .collect::<Vec<_>>();
    store
        .activate_delivery_storage(qos_relay::clock::now_ns())
        .unwrap();
    let delivery = store.state.delivery.as_ref().unwrap();
    for ((bytes, kind), queued) in pending.iter().zip(&delivery.queue) {
        assert_eq!(delivery.pool.resolve(&queued.payload).unwrap(), bytes);
        assert_eq!(&queued.kind, kind);
    }
    let original_pool = delivery.pool.encode(schema::POOL_LIMIT).unwrap();
    let replay = delivery.ledger.replay.clone();
    let incarnation = delivery.ledger.store_generation;
    let mut next = store.state.clone();
    next.delivery.as_mut().unwrap().queue.clear();
    store.commit(next).unwrap();
    let delivery = store.state.delivery.as_ref().unwrap();
    assert!(delivery.queue.is_empty());
    assert_eq!(
        delivery.pool.encode(schema::POOL_LIMIT).unwrap(),
        original_pool
    );
    assert_eq!(delivery.ledger.replay, replay);
    assert_eq!(delivery.ledger.store_generation, incarnation);
    assert!(store.state.outbox.is_empty());
}

#[test]
fn delivery_v6_malformed_or_downgraded_metadata_is_preserved_and_rejected() {
    let (dir, mut store, source) = crate::agent_owner_tests::enrolled();
    let caller = store.agent_caller(source).unwrap();
    store.activate_private_agent_ledger(&caller).unwrap();
    store
        .activate_delivery_storage(qos_relay::clock::now_ns())
        .unwrap();
    let original = serde_json::to_value(&store.state).unwrap();
    drop(store);
    for kind in 0..6 {
        let mut invalid = original.clone();
        match kind {
            0 => invalid["delivery"]["prior_private_schema"] = 4.into(),
            1 => invalid["delivery"]["version"] = 3.into(),
            2 => invalid["version"] = 5.into(),
            3 => {
                invalid.as_object_mut().unwrap().remove("delivery");
            }
            4 => invalid["delivery"]["unexpected"] = true.into(),
            5 => {
                invalid["delivery"]["queue"][0]["payload"] =
                    serde_json::to_value([0u8; 32]).unwrap()
            }
            _ => unreachable!(),
        }
        let bytes = serde_json::to_vec(&invalid).unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, &bytes).unwrap();
        assert!(
            Store::open(dir.path(), "agent-admission").is_err(),
            "invalid case {kind}"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            bytes,
            "must preserve failed read"
        );
    }
}
