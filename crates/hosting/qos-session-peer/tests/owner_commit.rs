//! Actual SessionOwner barriers, behind the bounded standalone broker handoff.
use qos_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest, OwnerCommitService};
use qos_session_peer::{SessionOwner, owner_commit::OwnerCommitMailbox};
use ui::automerge::{AutoCommit, ROOT, transaction::Transactable};

fn document(owner: &SessionOwner, sequence: u64) -> OwnerCommitRequest {
    let app = "dev.qos.word-processor";
    let record = "tab:0000000000000001";
    let mut doc = AutoCommit::load(&owner.peer().document(app, record).unwrap()).unwrap();
    doc.put(ROOT, "owner_commit_test", sequence).unwrap();
    doc.commit();
    let key = qos_user_identity::persist::read_device_key(
        &qos_user_identity::persist::IdentityPaths::under(owner.root().join("identity"))
            .device_privkey,
    )
    .unwrap();
    let room = owner.peer().room();
    let mut envelope = collab_types::SignedOp::new_unsigned(
        collab_types::DocumentMutation::Snapshot(doc.save())
            .encode()
            .unwrap(),
        collab_types::document_artifact_id(app, room, record),
        collab_types::Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
        collab_types::document_attestation_id(app, room, record),
        collab_types::OpClass::Splice,
        std::time::SystemTime::now(),
        sequence,
    );
    collab_types::sign_op(&mut envelope, &key).unwrap();
    OwnerCommitRequest {
        room: room.into(),
        operation: CommitOperation::Document {
            app: app.into(),
            record: record.into(),
            envelope: Box::new(envelope),
        },
    }
}

#[tokio::test]
async fn owner_commit_ack_waits_for_actual_aggregate_barrier_and_survives_restart() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("owner");
    let mut owner = SessionOwner::initialize(&root).unwrap();
    let before = owner.status();
    let request = document(&owner, 1000);
    let (service, mut mailbox) = OwnerCommitMailbox::bounded(2);
    let mut acknowledgement = tokio::spawn(service.commit(request.clone()));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(25), &mut acknowledgement)
            .await
            .is_err(),
        "enqueue must not count as owner acknowledgement"
    );
    assert_eq!(owner.status(), before);
    mailbox.recv().await.unwrap().apply(&mut owner).unwrap();
    let receipt = acknowledgement.await.unwrap().unwrap();
    receipt.validate_for(&request).unwrap();
    assert!(receipt.generation > before.generation);
    let after = owner.status();
    drop(owner);
    let restored = SessionOwner::open(&root).unwrap();
    assert_eq!(restored.status(), after);
    assert_eq!(
        receipt.aggregate_hash,
        *blake3::hash(&std::fs::read(root.join("session.bin")).unwrap()).as_bytes()
    );
    let mut wrong = request.clone();
    wrong.room = uuid::Uuid::new_v4().to_string();
    assert!(receipt.validate_for(&wrong).is_err());
    if let CommitOperation::Document { record, .. } = &mut wrong.operation {
        record.push_str("-foreign");
    }
    assert!(receipt.validate_for(&wrong).is_err());
    let mut changed = receipt.clone();
    changed.binding.basis[0] ^= 1;
    assert!(
        changed.validate_for(&request).is_err(),
        "basis is load-bearing"
    );
}

#[tokio::test]
async fn owner_commit_failed_barrier_is_observable_and_does_not_acknowledge() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("owner");
    let mut owner = SessionOwner::initialize(&root).unwrap();
    let before = owner.status();
    let request = document(&owner, 1001);
    let (service, mut mailbox) = OwnerCommitMailbox::bounded(1);
    let acknowledgement = tokio::spawn(service.commit(request));
    let pending = mailbox.recv().await.unwrap();
    std::fs::rename(root.join("session.bin"), root.join("saved.bin")).unwrap();
    std::fs::create_dir(root.join("session.bin")).unwrap();
    assert!(pending.apply(&mut owner).is_err());
    assert!(acknowledgement.await.unwrap().is_err());
    assert!(service.health().is_err());
    assert!(!owner.permits_publication());
    assert_eq!(owner.status(), before);
}

#[tokio::test]
async fn owner_commit_pressure_is_bounded_and_never_silent_success() {
    let directory = tempfile::tempdir().unwrap();
    let owner = SessionOwner::initialize(&directory.path().join("owner")).unwrap();
    let (service, mut mailbox) = OwnerCommitMailbox::bounded(1);
    let first = tokio::spawn(service.commit(document(&owner, 1002)));
    let pending = mailbox.recv().await.unwrap();
    assert!(
        service.commit(document(&owner, 1003)).await.is_err(),
        "draining the queue must retain the total-inflight permit"
    );
    assert!(
        service.health().is_err(),
        "failure must withdraw standalone capability"
    );
    drop(pending);
    assert!(
        first.await.unwrap().is_err(),
        "dropping a pending handoff is uncertainty"
    );
}

#[tokio::test]
async fn owner_commit_effect_receipt_is_canonical_immutable_and_restart_durable() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("owner");
    let mut owner = SessionOwner::initialize(&root).unwrap();
    let invocation = uuid::Uuid::new_v4();
    let receipt = qos_desktop::WorkspaceEffectReceipt {
        invocation: serde_json::from_value(serde_json::json!(invocation)).unwrap(),
        actor: hex::encode(owner.peer().authority().user_public_key),
        context: Default::default(),
        status: serde_json::from_value(serde_json::json!({"status":"succeeded"})).unwrap(),
        completed_at: 42,
    };
    let request = OwnerCommitRequest {
        room: owner.peer().room().into(),
        operation: CommitOperation::EffectReceipt {
            invocation,
            redacted: serde_json::to_vec(&receipt).unwrap(),
        },
    };
    let (service, mut mailbox) = OwnerCommitMailbox::bounded(2);
    let acknowledgement = tokio::spawn(service.commit(request.clone()));
    mailbox.recv().await.unwrap().apply(&mut owner).unwrap();
    acknowledgement
        .await
        .unwrap()
        .unwrap()
        .validate_for(&request)
        .unwrap();
    let before = owner.status();
    drop(owner);
    let owner = SessionOwner::open(&root).unwrap();
    assert_eq!(owner.status(), before);
    let values = qos_peer_state::room::receipt_values(owner.peer().admitted_room().unwrap());
    assert_eq!(
        values.get(&invocation.to_string()),
        Some(&serde_json::to_value(receipt).unwrap())
    );
}

#[tokio::test]
async fn owner_commit_local_sdk_snapshot_does_not_weaken_network_checkpoint_rules() {
    let directory = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&directory.path().join("owner")).unwrap();
    let request = document(&owner, 1004);
    let CommitOperation::Document {
        app,
        record,
        envelope,
    } = &request.operation
    else {
        unreachable!()
    };
    let source = *envelope.author.as_bytes();
    let mut remote = owner.peer().clone();
    let network = qos_peer_state::NeighbourMsg::AppDocument {
        peer: qos_peer_state::peer_handle(&source),
        app_id: app.clone(),
        cluster_id: request.room.clone(),
        record_id: record.clone(),
        envelope: envelope.clone(),
    };
    let outcome = remote
        .receive_from(
            qos_peer_state::TransportOrigin::RelayDelta(source),
            network,
            web_time::SystemTime::now(),
        )
        .unwrap();
    assert!(
        outcome.deferred && !outcome.changed,
        "uncoupled network snapshot remains unadmitted"
    );
    let (service, mut mailbox) = OwnerCommitMailbox::bounded(1);
    let acknowledgement = tokio::spawn(service.commit(request.clone()));
    mailbox.recv().await.unwrap().apply(&mut owner).unwrap();
    acknowledgement
        .await
        .unwrap()
        .unwrap()
        .validate_for(&request)
        .unwrap();
    for bytes in owner.outgoing() {
        let message = postcard::from_bytes(bytes).unwrap();
        remote
            .receive_from(
                qos_peer_state::TransportOrigin::RelayDelta(source),
                message,
                web_time::SystemTime::now(),
            )
            .unwrap();
    }
    assert_eq!(
        remote.rows(),
        owner.peer().rows(),
        "only the committed governor-covered checkpoint admits remote state"
    );
}
