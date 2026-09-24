use super::*;
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, transaction::Transactable};
use qos_action_broker::collaboration_admin::{self as admin, CollaborationAdmin};

pub(crate) fn voice_store() -> (tempfile::TempDir, Store, qos_user_identity::DeviceCert) {
    let (dir, mut store, device) = crate::agent_owner_tests::enrolled();
    let now = qos_relay::clock::now_ns();
    let (before, mut peer) = store.peer_transition_at(now).unwrap();
    let mut chat = AutoCommit::load(&peer.document("dev.qos.chat", "").unwrap()).unwrap();
    let channels = chat.get(ROOT, "channels").unwrap().unwrap().1;
    let channel = chat
        .put_object(&channels, "cloud_voice", ObjType::Map)
        .unwrap();
    chat.put(&channel, "kind", "voice").unwrap();
    chat.put(&channel, "workspace", "cloud").unwrap();
    peer.publish_document("dev.qos.chat", "", chat.save(), &store.key(), now_time(now))
        .unwrap();
    let mut state = store.state.clone();
    state.outbox.extend(
        durable_mutation_publications(
            &mut peer,
            &before,
            &store.key(),
            now_time(now),
            store.next_generation().unwrap(),
        )
        .unwrap(),
    );
    state.bound.as_mut().unwrap().peer = peer.durable().unwrap();
    store.commit(state).unwrap();
    store.activate_delivery_storage(now).unwrap();
    let caller = store
        .peer_at(now)
        .unwrap()
        .certificates()
        .into_iter()
        .find(|cert| cert.device_public_key == device)
        .unwrap();
    (dir, store, caller)
}

fn create() -> admin::CollaborationAdminOperation {
    admin::CollaborationAdminOperation::MediaCreate(admin::MediaCreateInput {
        app_id: "dev.qos.chat".into(),
        record_id: "".into(),
        channel_id: "cloud_voice".into(),
        expected_authorization_revision: None,
        allowed: admin::MediaDirectionsInput {
            publish: true,
            subscribe: true,
        },
    })
}

#[tokio::test]
async fn cloud_media_governance_uses_actual_caller_receipt_and_reopen_barrier() {
    let (dir, store, caller) = voice_store();
    let room = store.state.room.clone();
    assert_ne!(
        caller,
        *store.peer().unwrap().authority(),
        "Cloud caller is its certified browser, not the host governor"
    );
    let service = crate::broker::CloudCollaborationAdmin::new(dir.path(), "agent-admission", &room);
    let invocation = qos_actions::InvocationId(uuid::Uuid::new_v4());
    let request = admin::CollaborationAdminRequest {
        room,
        invocation,
        caller,
        operation: create(),
    };
    drop(store);
    let first = service
        .execute(request.clone())
        .await
        .expect("real current Cloud RoomAdmin must create the governed call");
    assert!(
        service.supports_media_governance(),
        "selected actual Store writer is available to its existing broker"
    );
    let store = Store::open(dir.path(), "agent-admission").unwrap();
    let generation = store.state.generation;
    let peer = store.peer().unwrap();
    let policy = qos_peer_state::room::media::parse(peer.admitted_room().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(policy.calls.len(), 1);
    assert_eq!(store.state.receipts.len(), 1);
    assert!(store.state.outbox.is_empty());
    assert!(
        store
            .delivery_relay_batch()
            .unwrap()
            .messages()
            .next()
            .is_some()
    );
    drop(store);
    assert_eq!(service.execute(request.clone()).await.unwrap(), first);
    let store = Store::open(dir.path(), "agent-admission").unwrap();
    assert_eq!(
        store.state.generation, generation,
        "same invocation replays its original committed receipt"
    );
    drop(store);
    let mut changed = request;
    let admin::CollaborationAdminOperation::MediaCreate(input) = &mut changed.operation else {
        unreachable!()
    };
    input.allowed.subscribe = false;
    assert!(
        service.execute(changed).await.is_err(),
        "same invocation cannot substitute media intent"
    );
}

#[tokio::test]
async fn cloud_media_governance_all_operations_preserve_counters_and_semantic_errors() {
    use qos_peer_state::room::media::{CallPhaseV1, MediaCallResultV1};
    let (dir, store, caller) = voice_store();
    let room = store.state.room.clone();
    let service = crate::broker::CloudCollaborationAdmin::new(dir.path(), "agent-admission", &room);
    drop(store);
    let run = |operation| {
        service.execute(admin::CollaborationAdminRequest {
            room: room.clone(),
            invocation: qos_actions::InvocationId(uuid::Uuid::new_v4()),
            caller: caller.clone(),
            operation,
        })
    };
    let mut result: MediaCallResultV1 =
        serde_json::from_value(run(create()).await.unwrap()).unwrap();
    let change =
        admin::CollaborationAdminOperation::MediaSetDirections(admin::MediaDirectionsChangeInput {
            call_id: hex::encode(result.call_id),
            expected_authorization_revision: result.authorization_revision,
            expected_epoch: result.epoch,
            allowed: admin::MediaDirectionsInput {
                publish: true,
                subscribe: false,
            },
        });
    result = serde_json::from_value(run(change.clone()).await.unwrap()).unwrap();
    assert_eq!(result.epoch, 2);
    assert!(matches!(
        run(change).await,
        Err(admin::CollaborationAdminError::Semantic {
            code: admin::MediaRejectionCode::Stale,
            ..
        })
    ));
    result = serde_json::from_value(
        run(
            admin::CollaborationAdminOperation::MediaSetParticipantDenial(
                admin::MediaDenialInput {
                    call_id: hex::encode(result.call_id),
                    expected_authorization_revision: result.authorization_revision,
                    expected_epoch: result.epoch,
                    person_root: hex::encode(caller.user_public_key),
                    denied: admin::MediaDirectionsInput {
                        publish: false,
                        subscribe: true,
                    },
                },
            ),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(result.epoch, 3);
    result = serde_json::from_value(
        run(admin::CollaborationAdminOperation::MediaEnd(
            admin::MediaCallInput {
                call_id: hex::encode(result.call_id),
                expected_authorization_revision: result.authorization_revision,
                expected_epoch: result.epoch,
            },
        ))
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(result.phase, CallPhaseV1::Ended);
    result = serde_json::from_value(
        run(admin::CollaborationAdminOperation::MediaRestart(
            admin::MediaCallInput {
                call_id: hex::encode(result.call_id),
                expected_authorization_revision: result.authorization_revision,
                expected_epoch: result.epoch,
            },
        ))
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(result.phase, CallPhaseV1::Active);
    assert_eq!(result.epoch, 5);
    let store = Store::open(dir.path(), "agent-admission").unwrap();
    assert_eq!(store.state.receipts.len(), 5);
}

#[test]
fn cloud_media_governance_failed_barrier_never_returns_success_or_replays_effect() {
    use atomic_file::test_support::Checkpoint;
    let (dir, mut store, caller) = voice_store();
    let admin::CollaborationAdminOperation::MediaCreate(input) = create() else {
        unreachable!()
    };
    let request = ActionRequest {
        version: 1,
        room: store.state.room.clone(),
        invocation_id: "lost-media-reply".into(),
        operation: NativeOperation::MediaCreate(input),
    };
    let result = crate::storage::test_support::with_save_failure(
        dir.path(),
        Checkpoint::BeforeCommit,
        || {
            store.action(
                caller.device_public_key,
                &request,
                qos_relay::clock::now_ns(),
            )
        },
    );
    assert!(result.as_ref().is_err_and(crate::is_store_failure));
    assert!(
        store
            .action(
                caller.device_public_key,
                &request,
                qos_relay::clock::now_ns()
            )
            .is_err()
    );
    drop(store);
    let mut reopened = Store::open(dir.path(), "agent-admission").unwrap();
    let generation = reopened.state.generation;
    let receipt = reopened
        .action(
            caller.device_public_key,
            &request,
            qos_relay::clock::now_ns(),
        )
        .unwrap();
    assert_eq!(receipt.status, "completed");
    assert_eq!(reopened.state.generation, generation);
    let peer = reopened.peer().unwrap();
    assert_eq!(
        qos_peer_state::room::media::parse(peer.admitted_room().unwrap())
            .unwrap()
            .unwrap()
            .calls
            .len(),
        1
    );
}

#[test]
fn cloud_media_governance_publication_reconciles_verified_revocation_before_signing() {
    use qos_user_identity::{RevocationEntry, RevocationReason, sign_revocation_entry};
    let (_dir, mut store, caller) = voice_store();
    let admin::CollaborationAdminOperation::MediaCreate(input) = create() else {
        unreachable!()
    };
    let now = qos_relay::clock::now_ns();
    store
        .action(
            caller.device_public_key,
            &ActionRequest {
                version: 1,
                room: store.state.room.clone(),
                invocation_id: "prepare-policy".into(),
                operation: NativeOperation::MediaCreate(input),
            },
            now,
        )
        .unwrap();
    let (before, mut candidate) = store.peer_transition_at(now).unwrap();
    let mut revoked = RevocationEntry {
        user_public_key: caller.user_public_key,
        revoked_device_public_key: caller.device_public_key,
        revoked_at_ns: now,
        reason: RevocationReason::Lost,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut revoked, &SigningKey::from_bytes(&[7; 32])).unwrap();
    candidate
        .receive_from(
            TransportOrigin::RelayDelta(before.authority().device_public_key),
            NeighbourMsg::IdentityHello {
                cert: Box::new(before.authority().clone()),
                revocations: vec![revoked],
            },
            now_time(now + 1),
        )
        .unwrap();
    assert!(
        candidate
            .validate_media_governance_for_publication()
            .is_err()
    );
    let publications = durable_mutation_publications(
        &mut candidate,
        &before,
        &store.key(),
        now_time(now + 1),
        store.next_generation().unwrap(),
    )
    .unwrap();
    candidate
        .validate_media_governance_for_publication()
        .expect(
            "every actual Store publication helper must reconcile before preparing signed messages",
        );
    assert!(!publications.is_empty());
    let mut next = store.state.clone();
    next.outbox.extend(publications);
    next.bound.as_mut().unwrap().peer = candidate.durable().unwrap();
    store.commit(next).unwrap();
}
