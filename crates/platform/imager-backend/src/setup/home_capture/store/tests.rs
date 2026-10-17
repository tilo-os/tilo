use super::*;

fn fixture(root: &Path, operation: &str) -> (Store, Record) {
    let pin = PinnedDirectory::open_absolute(root).unwrap();
    let store = Store::open(pin, operation).unwrap();
    let grant = Grant {
        schema: 1,
        operation: operation.into(),
        caller: rustix::process::geteuid().as_raw(),
        home: NativePath::encode(root),
        inode: std::fs::metadata(root).unwrap().ino(),
        owner: rustix::process::geteuid().as_raw(),
        filesystem: [1; 16],
        subvolume: [2; 16],
        custody: None,
        preparation_namespace: None,
    };
    let source = btrfs::SubvolumeIdentity {
        filesystem: [1; 16],
        id: 256,
        uuid: [2; 16],
        parent_uuid: [0; 16],
        readonly: false,
    };
    let record = Record::intent(grant, &source, Path::new("home/alice")).unwrap();
    (store, record)
}

#[test]
fn durable_intent_roundtrip_serializes_same_operation_and_excludes_other_workers() {
    let root = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let operation = uuid::Uuid::new_v4().to_string();
    let (store, record) = fixture(root.path(), &operation);
    store.save(&record, true).unwrap();
    assert_eq!(store.load().unwrap(), Some(record.clone()));
    assert!(
        Store::open(
            PinnedDirectory::open_absolute(root.path()).unwrap(),
            &operation
        )
        .is_err()
    );
    assert_eq!(
        std::fs::metadata(
            root.path()
                .join("records")
                .join(format!("{operation}.json"))
        )
        .unwrap()
        .mode()
            & 0o777,
        0o600
    );
    let mut foreign = record.clone();
    foreign.grant.operation = uuid::Uuid::new_v4().to_string();
    assert!(store.save(&foreign, false).is_err());
    assert_eq!(store.load().unwrap(), Some(record));
}

#[test]
fn replaced_or_public_records_never_become_capture_cleanup_authority() {
    let root = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let operation = uuid::Uuid::new_v4().to_string();
    let (store, record) = fixture(root.path(), &operation);
    store.save(&record, true).unwrap();
    let path = root
        .path()
        .join("records")
        .join(format!("{operation}.json"));
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(store.load().is_err());
    std::fs::remove_file(&path).unwrap();
    let outside = root.path().join("unowned.json");
    std::fs::write(&outside, serde_json::to_vec(&record).unwrap()).unwrap();
    std::os::unix::fs::symlink(&outside, &path).unwrap();
    assert!(store.load().is_err());
    assert!(outside.exists());
}

#[test]
fn original_output_operation_lease_rejects_replaced_lock_and_foreign_operation() {
    let root = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let operation = uuid::Uuid::new_v4().to_string();
    let store = Store::open(
        PinnedDirectory::open_absolute(root.path()).unwrap(),
        &operation,
    )
    .unwrap();
    store.check_original_output_operation(&operation).unwrap();
    let device = std::fs::metadata(root.path()).unwrap().dev();
    store.check_original_output_filesystem(device).unwrap();
    assert!(
        store
            .check_original_output_filesystem(device.checked_add(1).unwrap())
            .is_err()
    );
    assert!(
        Store::open(
            PinnedDirectory::open_absolute(root.path()).unwrap(),
            &operation
        )
        .is_err()
    );
    assert!(
        store
            .check_original_output_operation(&uuid::Uuid::new_v4().to_string())
            .is_err()
    );
    let lock = root
        .path()
        .join("records")
        .join(format!("{operation}.lock"));
    std::fs::rename(&lock, root.path().join("held-original.lock")).unwrap();
    std::fs::write(&lock, b"").unwrap();
    std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o600)).unwrap();
    let error = store
        .check_original_output_operation(&operation)
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Original output operation lease name changed"
    );
    // The old descriptor/lock remains retained; matching permission, pathname
    // and operation do not turn the replacement into its authority.
    assert!(root.path().join("held-original.lock").exists());
}

#[test]
fn original_output_incomplete_intent_refuses_replay_and_named_replacement() {
    let root = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let operation = uuid::Uuid::new_v4().to_string();
    let store = Store::open(
        PinnedDirectory::open_absolute(root.path()).unwrap(),
        &operation,
    )
    .unwrap();
    let attempt = uuid::Uuid::new_v4().to_string();
    let (held, bytes) = store
        .claim_original_output_intent([9; 32], &attempt)
        .unwrap();
    store.check_original_output_intent(&held, &bytes).unwrap();
    let same = store
        .claim_original_output_intent([9; 32], &attempt)
        .unwrap_err();
    assert_eq!(same.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(
        same.to_string(),
        "Original output same intent requires verified recovery"
    );
    let different = store
        .claim_original_output_intent([8; 32], &uuid::Uuid::new_v4().to_string())
        .unwrap_err();
    assert_eq!(different.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(
        different.to_string(),
        "Original output different intent refuses replacement"
    );
    let path = root
        .path()
        .join("records")
        .join(format!("{operation}.original-output"));
    assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o7777, 0o600);
    std::fs::rename(&path, root.path().join("held-intent")).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let error = store
        .check_original_output_intent(&held, &bytes)
        .unwrap_err();
    assert_eq!(error.to_string(), "Original output intent name changed");
    // Same bytes and operation/attempt do not substitute a new named inode.
    assert_eq!(
        std::fs::read(root.path().join("held-intent")).unwrap(),
        bytes
    );
}

#[test]
fn original_output_prepared_retry_rebinds_fresh_claim_and_refuses_dirty_or_launched_stage() {
    use output_attempt::StageBinding;
    let root = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let operation = uuid::Uuid::new_v4().to_string();
    let parent = PinnedDirectory::open_absolute(root.path()).unwrap();
    let owner = rustix::process::geteuid().as_raw();
    let uid = if owner == 0 { 99 } else { owner };
    let gid = rustix::process::getegid().as_raw();
    let binding = || StageBinding {
        declaration: [1; 32],
        filesystem: [2; 16],
        uid,
        gid,
    };
    let store = Store::open(parent.clone(), &operation).unwrap();
    let (attempt, output) = store
        .prepare_original_output_stage(&parent, binding())
        .unwrap();
    let original_identity = tilo_files::native::atomic::FileIdentity::of(&output).unwrap();
    let (intent, bytes) = store
        .claim_prepared_original_output_intent([3; 32], &attempt, &output)
        .unwrap();
    store.check_original_output_intent(&intent, &bytes).unwrap();
    drop(intent);
    drop(store);
    // Simulate lost acknowledgement before launch: actual operation lease was
    // released and reacquired, durable record and held exact stage survive.
    let store = Store::open(parent.clone(), &operation).unwrap();
    let (resumed, stage) = store
        .prepare_original_output_stage(&parent, binding())
        .unwrap();
    assert_eq!(resumed, attempt);
    assert_eq!(
        tilo_files::native::atomic::FileIdentity::of(&stage).unwrap(),
        original_identity
    );
    let (new_intent, new_bytes) = store
        .claim_prepared_original_output_intent([4; 32], &resumed, &stage)
        .unwrap();
    assert_ne!(new_bytes, bytes);
    store
        .check_original_output_intent(&new_intent, &new_bytes)
        .unwrap();
    let foreign = StageBinding {
        declaration: [5; 32],
        ..binding()
    };
    assert!(
        store
            .prepare_original_output_stage(&parent, foreign)
            .is_err()
    );
    use cap_primitives::fs::OpenOptionsExt;
    let mut options = cap_primitives::fs::OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    cap_primitives::fs::open(&stage, Path::new("partial"), &options).unwrap();
    assert!(
        store
            .prepare_original_output_stage(&parent, binding())
            .is_err()
    );
    assert!(
        store
            .claim_prepared_original_output_intent([6; 32], &resumed, &stage)
            .is_err()
    );
    std::fs::remove_file(root.path().join(format!("stage-{attempt}/partial"))).unwrap();
    store
        .mark_original_output_launching(&resumed, &stage)
        .unwrap();
    drop(store);
    let store = Store::open(parent, &operation).unwrap();
    assert!(
        store
            .prepare_original_output_stage(
                &PinnedDirectory::open_absolute(root.path()).unwrap(),
                binding()
            )
            .is_err()
    );
    assert!(
        store
            .claim_prepared_original_output_intent([7; 32], &resumed, &stage)
            .is_err()
    );
}

#[test]
fn original_output_namespace_checkpoint_refuses_unlaunched_foreign_and_duplicate_intent() {
    use output_attempt::StageBinding;
    let root = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let operation = uuid::Uuid::new_v4().to_string();
    let parent = PinnedDirectory::open_absolute(root.path()).unwrap();
    let owner = rustix::process::geteuid().as_raw();
    let uid = if owner == 0 { 99 } else { owner };
    let binding = || StageBinding {
        declaration: [1; 32],
        filesystem: [2; 16],
        uid,
        gid: rustix::process::getegid().as_raw(),
    };
    let store = Store::open(parent.clone(), &operation).unwrap();
    let (attempt, output) = store
        .prepare_original_output_stage(&parent, binding())
        .unwrap();
    // These are actual process facts, used only to exercise durable Store
    // binding. This storage test does not certify a launched worker namespace.
    let namespace = File::open("/proc/self/ns/pid").unwrap().metadata().unwrap();
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
    let start_ticks = stat
        .rsplit_once(") ")
        .unwrap()
        .1
        .split_whitespace()
        .nth(19)
        .unwrap()
        .parse()
        .unwrap();
    let facts = launcher::owned_service::ServiceCustody {
        init_pid: std::process::id(),
        start_ticks,
        namespace_dev: namespace.dev(),
        namespace_ino: namespace.ino(),
    };
    assert!(
        store
            .record_original_output_namespace(&attempt, &output, &facts)
            .is_err()
    );
    store
        .mark_original_output_launching(&attempt, &output)
        .unwrap();
    assert!(
        store
            .record_original_output_namespace(&uuid::Uuid::new_v4().to_string(), &output, &facts)
            .is_err()
    );
    let invalid = launcher::owned_service::ServiceCustody {
        init_pid: 1,
        ..facts
    };
    assert!(
        store
            .record_original_output_namespace(&attempt, &output, &invalid)
            .is_err()
    );
    store
        .record_original_output_namespace(&attempt, &output, &facts)
        .unwrap();
    drop(store);
    let resumed = Store::open(parent.clone(), &operation).unwrap();
    assert!(
        resumed
            .record_original_output_namespace(&attempt, &output, &facts)
            .is_err()
    );
    assert!(
        resumed
            .prepare_original_output_stage(&parent, binding())
            .is_err()
    );
}

#[test]
fn uncommitted_original_review_preserves_prepared_state_and_refuses_started_or_corrupt_attempts() {
    use crate::setup::{migration_handoff, prepared_setup::OfflineHomePlan};
    use output_attempt::{
        StageBinding, lookup_committed_original_selection, review_uncommitted_original_selection,
    };
    use sha2::Digest;
    let temp = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let home_path = temp.path().join("home");
    std::fs::create_dir(&home_path).unwrap();
    let caller = crate::setup::home_capture::original_caller::fixture_source_owner(&home_path);
    let operation = uuid::Uuid::new_v4().to_string();
    let plan = migration_handoff::capture_fixture(
        &home_path,
        &operation,
        migration_handoff::Categories {
            home: true,
            ..Default::default()
        },
    )
    .unwrap();
    let media = crate::media_layout::MediaPartitionBinding {
        schema: 1,
        target_stable_id: "owned-fixture".into(),
        disk_guid: uuid::Uuid::new_v4().to_string(),
        partition_guid: uuid::Uuid::new_v4().to_string(),
        number: 3,
        first_lba: 8192,
        last_lba: 16383,
        sector_bytes: 512,
        profile_offset: 4096,
    };
    let declaration = OfflineHomePlan::new(
        plan.clone(),
        "owned-fixture-setup".into(),
        [9; 32],
        media.clone(),
        caller.clone(),
        1024 * 1024,
    )
    .unwrap();
    let root = PinnedDirectory::open_absolute(temp.path()).unwrap();
    let private = root.create_child(OsStr::new(".tilo-installer")).unwrap();
    if rustix::process::geteuid().as_raw() != 0 {
        assert_eq!(
            review_uncommitted_original_selection(&private, &declaration)
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            lookup_committed_original_selection(&private, &declaration)
                .err()
                .expect("Nonroot lookup must refuse root custody")
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        return;
    }
    // Controlled protected metadata fixture, not authenticated source/export authority.
    let filesystem = uuid::Uuid::new_v4();
    let pending = crate::setup::media_setup::protected_output::PendingArea {
        schema: 1,
        medium: media.clone(),
        setup: crate::imager_model::PreparedSetupExpectation {
            schema: 1,
            sealed_bytes: 128,
            ciphertext_sha256: "a".repeat(64),
        },
        plans_sha256: sha2::Sha256::digest(
            serde_json::to_vec(&vec![plan.bytes().unwrap()]).unwrap(),
        )
        .into(),
        operation: operation.clone(),
        original_uid: caller.uid(),
        name: format!(".tilo-pending-{operation}"),
        inode: 1,
    };
    let held = storage(&private).unwrap();
    held.publish(OsStr::new("filesystem-owner.json"),&serde_json::to_vec(&serde_json::json!({"schema":1,"binding":media,"filesystem_uuid":filesystem.to_string()})).unwrap(),PublishMode::IfAbsent,Privacy::OwnerOnly).unwrap();
    held.publish(
        OsStr::new("original-pending-area.json"),
        &serde_json::to_vec(&pending).unwrap(),
        PublishMode::IfAbsent,
        Privacy::OwnerOnly,
    )
    .unwrap();
    review_uncommitted_original_selection(&private, &declaration).unwrap();
    assert!(
        lookup_committed_original_selection(&private, &declaration)
            .unwrap()
            .is_none()
    );
    assert!(!temp.path().join(".tilo-installer/original-output").exists());
    let parent = private.create_child(OsStr::new("original-output")).unwrap();
    assert!(review_uncommitted_original_selection(&private, &declaration).is_err()); // Partial Store is not absence.
    let store = Store::open(parent.clone(), &operation).unwrap();
    drop(store);
    review_uncommitted_original_selection(&private, &declaration).unwrap();
    let store = Store::open(parent.clone(), &operation).unwrap();
    let binding = StageBinding {
        declaration: sha2::Sha256::digest(serde_json::to_vec(&declaration).unwrap()).into(),
        filesystem: *filesystem.as_bytes(),
        uid: caller.uid(),
        gid: caller.primary_gid(),
    };
    let (attempt, stage) = store
        .prepare_original_output_stage(&parent, binding)
        .unwrap();
    let (intent, bytes) = store
        .claim_prepared_original_output_intent([7; 32], &attempt, &stage)
        .unwrap();
    drop(intent);
    drop(store);
    let journal = temp
        .path()
        .join(".tilo-installer/original-output/records")
        .join(format!("{operation}.original-output-attempt"));
    let before = std::fs::read(&journal).unwrap();
    review_uncommitted_original_selection(&private, &declaration).unwrap();
    assert!(
        lookup_committed_original_selection(&private, &declaration)
            .unwrap()
            .is_none()
    );
    assert_eq!(std::fs::read(&journal).unwrap(), before);
    assert_eq!(
        std::fs::read(
            journal
                .parent()
                .unwrap()
                .join(format!("{operation}.original-output"))
        )
        .unwrap(),
        bytes
    );
    // A foreign protected source binding never resumes this prepared attempt.
    let mut foreign = serde_json::to_value(&declaration).unwrap();
    foreign["setup_binding"] = "foreign".into();
    let foreign: OfflineHomePlan = serde_json::from_value(foreign).unwrap();
    assert!(review_uncommitted_original_selection(&private, &foreign).is_err());
    let stage_path = temp
        .path()
        .join(".tilo-installer/original-output")
        .join(format!("stage-{attempt}"));
    std::fs::write(stage_path.join("partial"), b"not empty").unwrap();
    assert!(review_uncommitted_original_selection(&private, &declaration).is_err());
    std::fs::remove_file(stage_path.join("partial")).unwrap();
    let store = Store::open(parent, &operation).unwrap();
    store
        .mark_original_output_launching(&attempt, &stage)
        .unwrap();
    drop(store);
    assert!(review_uncommitted_original_selection(&private, &declaration).is_err());
    assert!(lookup_committed_original_selection(&private, &declaration).is_err());
    // Malformed later states are refusal cases, not fabricated successful verifier receipts.
    let started = std::fs::read(&journal).unwrap();
    for phase in ["exported", "verifying", "verified"] {
        let mut corrupt: serde_json::Value = serde_json::from_slice(&started).unwrap();
        corrupt["phase"] = phase.into();
        std::fs::write(&journal, serde_json::to_vec(&corrupt).unwrap()).unwrap();
        assert!(review_uncommitted_original_selection(&private, &declaration).is_err());
        assert!(lookup_committed_original_selection(&private, &declaration).is_err());
    }
    std::fs::write(&journal, b"invalid").unwrap();
    assert!(review_uncommitted_original_selection(&private, &declaration).is_err());
    assert!(lookup_committed_original_selection(&private, &declaration).is_err());
    std::fs::remove_file(&journal).unwrap();
    assert!(review_uncommitted_original_selection(&private, &declaration).is_err()); // An unrecorded stage/intent is not never-started.
}

#[test]
fn original_applications_output_prepared_retry_is_exact_and_cannot_adopt_home_intent() {
    use output_attempt::StageBinding;
    let root = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let parent = PinnedDirectory::open_absolute(root.path()).unwrap();
    let uid = rustix::process::geteuid().as_raw();
    let binding = || StageBinding {
        declaration: [41; 32],
        filesystem: [42; 16],
        uid: if uid == 0 { 99 } else { uid },
        gid: rustix::process::getegid().as_raw(),
    };
    let operation = uuid::Uuid::new_v4().to_string();
    let store = Store::open(parent.clone(), &operation).unwrap();
    let (attempt, output) = store
        .prepare_original_applications_output_stage(&parent, binding())
        .unwrap();
    let actual = tilo_files::native::atomic::FileIdentity::of(&output).unwrap();
    let checkpoint = root
        .path()
        .join("records")
        .join(format!("{operation}.original-output-attempt"));
    let before = std::fs::read(&checkpoint).unwrap();
    let record: serde_json::Value = serde_json::from_slice(&before).unwrap();
    assert_eq!(record["schema"], 2);
    assert!(
        record["expected"].is_null(),
        "applications-only never invents Home authority"
    );
    assert!(
        store
            .prepare_original_output_stage(&parent, binding())
            .is_err(),
        "Home preparation cannot adopt the same bound applications-only intent"
    );
    assert_eq!(std::fs::read(&checkpoint).unwrap(), before);
    let (same, held) = store
        .prepare_original_applications_output_stage(&parent, binding())
        .unwrap();
    assert_eq!(same, attempt);
    assert_eq!(
        tilo_files::native::atomic::FileIdentity::of(&held).unwrap(),
        actual
    );
    store
        .mark_original_output_launching(&attempt, &output)
        .unwrap();
    let launched = std::fs::read(&checkpoint).unwrap();
    assert!(
        store
            .prepare_original_applications_output_stage(&parent, binding())
            .is_err(),
        "lost launch acknowledgement cannot automatically start another writer"
    );
    assert_eq!(std::fs::read(&checkpoint).unwrap(), launched);
}
