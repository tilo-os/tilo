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
    let key = tilo_user_identity::persist::read_device_key(
        &tilo_user_identity::persist::IdentityPaths::under(
            owner.root().canonicalize().unwrap().join("identity"),
        )
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
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
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
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
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
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
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
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
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
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
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
    for bytes in &outgoing_bytes(&owner) {
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

#[tokio::test]
async fn small_commit_retains_baseline_and_publishes_only_touched_route() {
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let mut owner = SessionOwner::initialize(&directory.path().join("owner")).unwrap();
    let baseline = outgoing_bytes(&owner).to_vec();
    let untouched = owner
        .peer()
        .rows()
        .into_iter()
        .find(|row| row.app_id != "dev.qos.word-processor")
        .unwrap();
    let prior_receipt = owner
        .peer()
        .durable_coverage(&untouched.app_id, &untouched.record_id)
        .unwrap()
        .unwrap();
    let prior_floors = owner
        .peer()
        .replay_floors()
        .into_iter()
        .filter(|(app, record, _, _)| app == &untouched.app_id && record == &untouched.record_id)
        .collect::<Vec<_>>();
    let (service, mut mailbox) = OwnerCommitMailbox::bounded(1);
    let acknowledgement = tokio::spawn(service.commit(document(&owner, 1005)));
    mailbox.recv().await.unwrap().apply(&mut owner).unwrap();
    acknowledgement.await.unwrap().unwrap();
    assert!(
        outgoing_bytes(&owner).starts_with(&baseline),
        "ordinary commit must persist its recovery baseline"
    );
    let tail = &outgoing_bytes(&owner)[baseline.len()..];
    assert_eq!(
        tail.len(),
        2,
        "one room delta plus exactly the touched snapshot"
    );
    assert_eq!(
        &outgoing_kinds(&owner)[baseline.len()..],
        &[
            qos_peer_state::transport::Retention::Delta,
            qos_peer_state::transport::Retention::Checkpoint
        ]
    );
    assert!(matches!(
        postcard::from_bytes::<qos_peer_state::NeighbourMsg>(&tail[0]).unwrap(),
        qos_peer_state::NeighbourMsg::SyncResp { .. }
    ));
    assert!(
        matches!(postcard::from_bytes::<qos_peer_state::NeighbourMsg>(&tail[1]).unwrap(), qos_peer_state::NeighbourMsg::AppDocument { app_id, record_id, .. } if app_id == "dev.qos.word-processor" && record_id == "tab:0000000000000001")
    );
    assert_eq!(
        owner
            .peer()
            .durable_coverage(&untouched.app_id, &untouched.record_id)
            .unwrap()
            .unwrap(),
        prior_receipt
    );
    assert_eq!(owner.peer().replay_floors().into_iter().filter(|(app, record, _, _)| app == &untouched.app_id && record == &untouched.record_id).collect::<Vec<_>>(), prior_floors);
}

#[tokio::test]
async fn room_effect_receipt_publishes_no_application_snapshot() {
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let mut owner = SessionOwner::initialize(&directory.path().join("owner")).unwrap();
    let baseline = outgoing_bytes(&owner).to_vec();
    let floors = owner.peer().replay_floors();
    let receipts =
        qos_peer_state::durability::entries(owner.peer().admitted_room().unwrap()).unwrap();
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
    let (service, mut mailbox) = OwnerCommitMailbox::bounded(1);
    let acknowledgement = tokio::spawn(service.commit(request));
    mailbox.recv().await.unwrap().apply(&mut owner).unwrap();
    acknowledgement.await.unwrap().unwrap();
    assert!(
        outgoing_bytes(&owner).starts_with(&baseline),
        "room-only commit must retain baseline"
    );
    let tail = &outgoing_bytes(&owner)[baseline.len()..];
    assert_eq!(
        tail.len(),
        1,
        "room-only effects have no application publication"
    );
    assert!(matches!(
        postcard::from_bytes::<qos_peer_state::NeighbourMsg>(&tail[0]).unwrap(),
        qos_peer_state::NeighbourMsg::SyncResp { .. }
    ));
    assert_eq!(owner.peer().replay_floors(), floors);
    assert_eq!(
        qos_peer_state::durability::entries(owner.peer().admitted_room().unwrap()).unwrap(),
        receipts
    );
}

#[tokio::test]
async fn committed_publication_compacts_to_bounded_recoverable_history() {
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let root = directory.path().join("owner");
    let mut owner = SessionOwner::initialize(&root).unwrap();
    let baseline_len = outgoing_bytes(&owner).len();
    let (service, mut mailbox) = OwnerCommitMailbox::bounded(1);
    for sequence in 1100..1140 {
        let acknowledgement = tokio::spawn(service.commit(document(&owner, sequence * 2)));
        mailbox.recv().await.unwrap().apply(&mut owner).unwrap();
        acknowledgement.await.unwrap().unwrap();
        assert!(
            outgoing_bytes(&owner).len() <= baseline_len + 64,
            "obsolete publication tail must be bounded"
        );
    }
    let bytes = outgoing_bytes(&owner).to_vec();
    let rows = owner.peer().rows();
    drop(owner);
    let owner = SessionOwner::open(&root).unwrap();
    assert_eq!(outgoing_bytes(&owner), bytes);
    let mut fresh = qos_peer_state::StatePeer::new(
        owner.peer().room(),
        owner.peer().authority().clone(),
        owner.peer().certificates(),
        tilo_user_identity::RevocationList::new(),
        web_time::SystemTime::now(),
    )
    .unwrap();
    for bytes in &outgoing_bytes(&owner) {
        fresh
            .receive_from(
                qos_peer_state::TransportOrigin::RelayDelta(
                    owner.peer().authority().device_public_key,
                ),
                postcard::from_bytes(bytes).unwrap(),
                web_time::SystemTime::now(),
            )
            .unwrap();
    }
    assert_eq!(fresh.rows(), rows);
    for row in rows {
        assert!(
            fresh
                .durable_covers_current(&row.app_id, &row.record_id)
                .unwrap()
        );
    }
}

#[tokio::test]
async fn failed_compaction_barrier_keeps_exact_recovery_tail_and_receipts() {
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let root = directory.path().join("owner");
    let mut owner = SessionOwner::initialize(&root).unwrap();
    let (service, mut mailbox) = OwnerCommitMailbox::bounded(1);
    // Stay just below the existing >=64-frame compaction threshold.
    // The next two-frame commit must compact under the injected failed barrier.
    for sequence in 1200..1231 {
        let acknowledgement = tokio::spawn(service.commit(document(&owner, sequence * 2)));
        mailbox.recv().await.unwrap().apply(&mut owner).unwrap();
        acknowledgement.await.unwrap().unwrap();
    }
    assert_eq!(
        outgoing_bytes(&owner).len() - owner.publication_baseline().1,
        62
    );
    let before = owner.status();
    let bytes = outgoing_bytes(&owner).to_vec();
    let kinds = outgoing_kinds(&owner).to_vec();
    let receipt = owner.peer().durable_coverage("dev.qos.chat", "").unwrap();
    let request = document(&owner, 3000);
    let CommitOperation::Document {
        app,
        record,
        envelope,
    } = &request.operation
    else {
        unreachable!()
    };
    let mut candidate = owner.peer().clone();
    let now = web_time::SystemTime::now();
    candidate
        .admit_local_app_document(app, record, *envelope.clone(), now)
        .unwrap();
    let key = tilo_user_identity::persist::read_device_key(
        &tilo_user_identity::persist::IdentityPaths::under(
            root.canonicalize().unwrap().join("identity"),
        )
        .device_privkey,
    )
    .unwrap();
    let next_frames = candidate
        .durable_mutation_messages(owner.peer(), &key, now, owner.status().generation + 1)
        .unwrap()
        .len();
    assert_eq!(
        next_frames, 2,
        "faulted commit must cross the compaction threshold"
    );
    assert_eq!(
        owner.outgoing().len() - owner.publication_baseline().1 + next_frames,
        64
    );
    let acknowledgement = tokio::spawn(service.commit(request));
    let pending = mailbox.recv().await.unwrap();
    std::fs::rename(root.join("session.bin"), root.join("saved.bin")).unwrap();
    std::fs::create_dir(root.join("session.bin")).unwrap();
    assert!(pending.apply(&mut owner).is_err());
    assert!(acknowledgement.await.unwrap().is_err());
    assert!(!owner.permits_publication());
    assert_eq!(owner.status(), before);
    assert_eq!(outgoing_bytes(&owner), bytes);
    assert_eq!(outgoing_kinds(&owner), kinds);
    assert_eq!(
        owner.peer().durable_coverage("dev.qos.chat", "").unwrap(),
        receipt
    );
    drop(owner);
    std::fs::remove_dir(root.join("session.bin")).unwrap();
    std::fs::rename(root.join("saved.bin"), root.join("session.bin")).unwrap();
    let restored = SessionOwner::open(&root).unwrap();
    assert_eq!(restored.status(), before);
    assert_eq!(outgoing_bytes(&restored), bytes);
}

fn outgoing_bytes(owner: &qos_session_peer::SessionOwner) -> Vec<Vec<u8>> {
    owner.outgoing().iter().map(|p| p.bytes.to_vec()).collect()
}
fn outgoing_kinds(
    owner: &qos_session_peer::SessionOwner,
) -> Vec<qos_peer_state::transport::Retention> {
    owner.outgoing().iter().map(|p| p.kind).collect()
}

#[tokio::test]
async fn delivery_selected_owner_mailbox_commits_exact_document_and_reopens_same_receipt() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("delivery-owner");
    let mut owner = SessionOwner::initialize(&root).unwrap();
    owner.activate_delivery().unwrap();
    let request = document(&owner, 1000);
    let (service, mut mailbox) = OwnerCommitMailbox::bounded(1);
    let waiting = tokio::spawn(service.commit(request.clone()));
    mailbox.recv().await.unwrap().apply(&mut owner).unwrap();
    let receipt = waiting.await.unwrap().unwrap();
    receipt.validate_for(&request).unwrap();
    let after = owner.status();
    assert!(owner.peer().admission_v5_selected());
    drop(owner);
    let owner = SessionOwner::open(&root).unwrap();
    assert!(owner.peer().admission_v5_selected());
    assert_eq!(owner.status(), after);
    assert_eq!(
        receipt.aggregate_hash,
        *blake3::hash(&std::fs::read(root.join("session.bin")).unwrap()).as_bytes()
    );
}
