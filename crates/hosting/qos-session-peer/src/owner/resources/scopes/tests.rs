use super::*;

fn fixture() -> (
    tempfile::TempDir,
    SessionOwner,
    HostBytes,
    CommittedAuthority,
) {
    let temp = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
    let (ingress, _receiver) = crate::https::ingress_channel();
    let pool = ingress.host_bytes();
    owner.attach_data_resources(&pool).unwrap();
    let current = CommittedAuthority::new(&owner);
    (temp, owner, pool, current)
}

// Catches attachment stranding a real checkpoint despite a reserved outer turn.
#[test]
fn owner_scope_checkpoint_commits_and_publishes_actual_current() {
    let (_temp, mut owner, pool, current) = fixture();
    let before = owner.committed_generation();
    let original = std::fs::read(owner.root().join("session.bin")).unwrap();
    assert!(
        owner.refresh_checkpoint().is_err(),
        "attached unscoped checkpoint must refuse"
    );
    assert_eq!(
        std::fs::read(owner.root().join("session.bin")).unwrap(),
        original
    );
    let mut scope = owner
        .prepare_owner_scope(&pool, OwnerOperation::Checkpoint)
        .unwrap();
    let receipt = owner
        .with_owner_scope(&mut scope, &current, |owner| {
            assert!(owner.serialized_commit_cap().is_ok());
            owner.refresh_checkpoint()
        })
        .unwrap()
        .unwrap();
    assert_eq!(receipt.generation, before + 1);
    assert_eq!(current.read().unwrap().generation(), before + 1);
    assert_ne!(
        std::fs::read(owner.root().join("session.bin")).unwrap(),
        original
    );
    assert!(
        owner.serialized_commit_cap().is_err(),
        "outer reservation must reset after publish"
    );
}

// Catches flush bypassing attachment and encoding/writing without a real turn.
#[test]
fn owner_scope_attached_flush_refuses_without_changing_disk_or_live_state() {
    let (_temp, mut owner, _pool, _current) = fixture();
    let original = std::fs::read(owner.root().join("session.bin")).unwrap();
    let generation = owner.committed_generation();
    assert!(
        owner.flush().is_err(),
        "attached unscoped flush must refuse before serialization"
    );
    assert_eq!(owner.committed_generation(), generation);
    assert_eq!(
        std::fs::read(owner.root().join("session.bin")).unwrap(),
        original
    );
    assert!(!owner.store.is_poisoned());
}

fn deferred_input(owner: &SessionOwner) -> ([u8; 32], NeighbourMsg, Vec<u8>) {
    use ui::automerge::{ROOT, transaction::Transactable};
    let mut author = owner.peer.clone();
    let mut document =
        ui::automerge::AutoCommit::load(&author.document("dev.qos.chat", "").unwrap()).unwrap();
    let heads = document.get_heads();
    document
        .put(ROOT, "scope-deferred", "original authenticated input")
        .unwrap();
    document.commit();
    let mut message = author
        .publish_incremental(
            "dev.qos.chat",
            "",
            document.save_after(&heads),
            &owner.identity.native,
            SystemTime::now(),
        )
        .unwrap();
    let key = ed25519_dalek::SigningKey::from_bytes(&[119; 32]);
    let device = key.verifying_key().to_bytes();
    let NeighbourMsg::AppDocument { peer, envelope, .. } = &mut message else {
        panic!("document fixture")
    };
    *peer = qos_peer_state::peer_handle(&device);
    envelope.author = collab_types::Ed25519PublicKey::from_bytes(device);
    collab_types::sign_op(envelope, &key).unwrap();
    let bytes = postcard::to_stdvec(&message).unwrap();
    (device, message, bytes)
}

// Catches native receive retaining a valid deferred input before the late commit gate.
#[test]
fn owner_scope_unscoped_native_deferral_cannot_change_live_pending_custody() {
    let (_temp, mut owner, _pool, _current) = fixture();
    let (device, message, _) = deferred_input(&owner);
    let before = owner.peer.pending_count();
    assert!(
        owner
            .receive(TransportOrigin::RelayDelta(device), message)
            .is_err(),
        "unscoped deferral must refuse at entry"
    );
    assert_eq!(owner.peer.pending_count(), before);
}

// Catches the equivalent direct delivery entry decoding/cloning/queuing outside a turn.
#[test]
fn owner_scope_unscoped_delivery_deferral_cannot_change_live_pending_custody() {
    let temp = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
    owner.activate_delivery().unwrap();
    let (device, _, bytes) = deferred_input(&owner);
    let (ingress, _receiver) = crate::https::ingress_channel();
    owner.attach_data_resources(&ingress.host_bytes()).unwrap();
    let before = owner.peer.pending_count();
    assert!(
        owner
            .receive_delivery(
                TransportOrigin::RelayDelta(device),
                super::super::super::delivery::CompleteStateItem {
                    delivery_id: [201; 16],
                    canonical: &bytes
                },
                SystemTime::now()
            )
            .is_err(),
        "direct unscoped delivery must refuse at entry"
    );
    assert_eq!(owner.peer.pending_count(), before);
}

// Refusal occurs before caller context or mutation; tokens cannot cross owners/generations.
#[test]
fn owner_scope_wrong_pool_owner_reuse_stale_and_nesting_never_run_callback() {
    let (_temp, mut owner, pool, current) = fixture();
    let (_other_temp, mut other, other_pool, other_current) = fixture();
    assert!(
        owner
            .prepare_owner_scope(&other_pool, OwnerOperation::Checkpoint)
            .is_err()
    );
    let mut first = owner
        .prepare_owner_scope(&pool, OwnerOperation::Checkpoint)
        .unwrap();
    let mut stale = owner
        .prepare_owner_scope(&pool, OwnerOperation::Checkpoint)
        .unwrap();
    assert!(
        other
            .with_owner_scope(&mut first, &other_current, |_| panic!(
                "wrong owner callback"
            ))
            .is_err()
    );
    owner
        .with_owner_scope(&mut first, &current, |owner| {
            assert!(
                owner
                    .with_owner_scope(&mut stale, &current, |_| panic!("nested callback"))
                    .is_err()
            );
            owner.refresh_checkpoint()
        })
        .unwrap()
        .unwrap();
    assert!(
        owner
            .with_owner_scope(&mut first, &current, |_| panic!("reused callback"))
            .is_err()
    );
    assert!(
        owner
            .with_owner_scope(&mut stale, &current, |_| panic!("stale callback"))
            .is_err()
    );
    assert_eq!(
        current.read().unwrap().generation(),
        owner.committed_generation()
    );
}

#[test]
fn owner_scope_pressure_and_disk_refuse_before_any_turn() {
    let (_temp, owner, pool, current) = fixture();
    let before = std::fs::read(owner.root().join("session.bin")).unwrap();
    let pressure = pool
        .try_acquire_bulk(pool.available_bytes() - crate::host_bytes::CONTROL_RESERVE_BYTES)
        .unwrap();
    let error = match owner.prepare_owner_scope(&pool, OwnerOperation::Checkpoint) {
        Err(error) => error,
        Ok(_) => panic!("bulk pressure accepted"),
    };
    assert_eq!(error.kind(), Failure::Capacity(Capacity::Bytes));
    drop(pressure);
    let root = owner.root().to_owned();
    let error = crate::store::test_space::with_space(
        &root,
        crate::store::test_space::Statistics {
            available: 0,
            total: 2 * 1024 * 1024 * 1024,
            unit: 4096,
        },
        || match owner.prepare_owner_scope(&pool, OwnerOperation::Checkpoint) {
            Err(error) => error,
            Ok(_) => panic!("low disk accepted"),
        },
    );
    assert_eq!(error.kind(), Failure::Capacity(Capacity::Disk));
    assert_eq!(
        std::fs::read(owner.root().join("session.bin")).unwrap(),
        before
    );
    assert!(current.read().is_ok());
}

#[test]
fn owner_scope_barrier_failure_denies_current_and_releases_staged_credit() {
    let (_temp, mut owner, pool, current) = fixture();
    let available = pool.available_bytes();
    let mut scope = owner
        .prepare_owner_scope(&pool, OwnerOperation::Checkpoint)
        .unwrap();
    let root = owner.root().to_owned();
    let result = crate::store::test_fault::with_fault(
        &root,
        crate::store::test_fault::Point::BeforeWrite,
        || owner.with_owner_scope(&mut scope, &current, |owner| owner.refresh_checkpoint()),
    );
    assert_eq!(result.unwrap_err().kind(), Failure::DurabilityUncertain);
    assert!(current.read().is_err());
    assert!(owner.serialized_commit_cap().is_err());
    drop(scope);
    // Current denial can release its old capture, but failed candidates cannot retain credits.
    assert!(pool.available_bytes() >= available);
}

#[test]
fn owner_scope_rejection_preserves_current_and_panic_clears_active_scope() {
    let (_temp, mut owner, pool, current) = fixture();
    let mut rejected = owner
        .prepare_owner_scope(&pool, OwnerOperation::Descriptor)
        .unwrap();
    let result = owner
        .with_owner_view_scope(&mut rejected, &current, |view, owner| {
            assert_eq!(view.generation(), owner.committed_generation());
            Err::<(), _>("actual typed rejection")
        })
        .unwrap();
    assert_eq!(result, Err("actual typed rejection"));
    assert!(current.read().is_ok());
    let mut cancelled = owner
        .prepare_owner_scope(&pool, OwnerOperation::Checkpoint)
        .unwrap();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = owner.with_owner_scope(&mut cancelled, &current, |_| {
            panic!("cancel owner callback")
        });
    }));
    assert!(panic.is_err());
    assert!(owner.serialized_commit_cap().is_err());
    assert!(
        current.read().is_err(),
        "abandoned Current remains terminally denied"
    );
}

#[test]
fn owner_scope_media_preparation_refuses_before_cloning_attached_peer() {
    use qos_action_broker::collaboration_admin::{
        CollaborationAdminOperation, MediaCreateInput, MediaDirectionsInput,
    };
    use ui::automerge::{ObjType, ROOT, ReadDoc, transaction::Transactable};
    let temp = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
    owner.activate_delivery().unwrap();
    let mut peer = owner.peer.clone();
    let mut document =
        ui::automerge::AutoCommit::load(&peer.document("dev.qos.chat", "").unwrap()).unwrap();
    let channels = document.get(ROOT, "channels").unwrap().unwrap().1;
    let channel = document
        .put_object(&channels, "scope-voice", ObjType::Map)
        .unwrap();
    document.put(&channel, "kind", "voice").unwrap();
    document.put(&channel, "workspace", "home").unwrap();
    peer.publish_document(
        "dev.qos.chat",
        "",
        document.save(),
        &owner.identity.native,
        SystemTime::now(),
    )
    .unwrap();
    owner
        .commit_mutation_candidate(peer, owner.directory().to_vec())
        .unwrap();
    let (ingress, _rx) = crate::https::ingress_channel();
    let pool = ingress.host_bytes();
    owner.attach_data_resources(&pool).unwrap();
    let operation = CollaborationAdminOperation::MediaCreate(MediaCreateInput {
        app_id: "dev.qos.chat".into(),
        record_id: String::new(),
        channel_id: "scope-voice".into(),
        expected_authorization_revision: None,
        allowed: MediaDirectionsInput {
            publish: true,
            subscribe: true,
        },
    });
    assert!(
        owner
            .prepare_media_transition(&operation, SystemTime::now())
            .is_err(),
        "direct attached media preparation must require early owner scope"
    );
    let current = CommittedAuthority::new(&owner);
    let mut scope = owner
        .prepare_owner_scope(
            &pool,
            OwnerOperation::MediaAdministration { input_len: 128 },
        )
        .unwrap();
    assert!(
        owner
            .with_owner_scope(&mut scope, &current, |o| o
                .prepare_media_transition(&operation, SystemTime::now()))
            .unwrap()
            .is_ok()
    );
}

#[test]
fn owner_scope_deferred_input_keeps_serialized_credit_after_scope_ends() {
    let (_temp, mut owner, pool, current) = fixture();
    let (device, message, bytes) = deferred_input(&owner);
    let before = pool.available_bytes();
    let generation = owner.committed_generation();
    let original = std::fs::read(owner.root().join("session.bin")).unwrap();
    let mut scope = owner
        .prepare_owner_scope(
            &pool,
            OwnerOperation::LegacyReceive {
                input_len: bytes.len(),
            },
        )
        .unwrap();
    owner
        .with_owner_scope(&mut scope, &current, |o| {
            o.receive(TransportOrigin::RelayDelta(device), message)
        })
        .unwrap()
        .unwrap();
    drop(scope);
    assert_eq!(owner.peer.pending_count(), 1);
    assert_eq!(owner.committed_generation(), generation);
    assert_eq!(
        std::fs::read(owner.root().join("session.bin")).unwrap(),
        original
    );
    assert!(
        pool.available_bytes() <= before - bytes.len(),
        "volatile deferred payload needs retained serialized custody after its outer scope"
    );
}

#[test]
fn owner_scope_staging_binds_real_store_workspace_and_original_inventory() {
    let temp = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
    let (ingress, _rx) = crate::https::ingress_channel();
    let pool = ingress.host_bytes();
    owner.attach_data_resources(&pool).unwrap();
    let (foreign, _rx2) = crate::https::ingress_channel();
    assert!(owner.staging_directory(&foreign.host_work()).is_err());
    let directory = owner.staging_directory(&ingress.host_work()).unwrap();
    let identity = crate::store::StageIdentity {
        workspace: owner.workspace_id,
        device: [1; 32],
        delivery: [1; 16],
        digest: [1; 32],
        total: 1,
    };
    let lease = directory.reserve(identity).unwrap();
    assert!(lease.same_pool(&pool));
    let mut wrong = identity;
    wrong.workspace = wrong.workspace.wrapping_add(1);
    assert!(directory.reserve(wrong).is_err());
    assert!(owner.root().join("staging-v1").is_dir());
}
