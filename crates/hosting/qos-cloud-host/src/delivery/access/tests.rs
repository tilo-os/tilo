use super::*;
use atomic_file::test_support::{ExistingCheckpoint, with_existing_hooks};

fn selected() -> (tempfile::TempDir, Store) {
    let (dir, mut store, _) = crate::agent_owner_tests::enrolled();
    store
        .activate_delivery_storage(qos_relay::clock::now_ns())
        .unwrap();
    (dir, store)
}

#[test]
fn delivery_access_actual_lock_busy_then_exact_current_without_rewrite() {
    let (dir, store) = selected();
    let bytes = std::fs::read(dir.path().join("state.json")).unwrap();
    let generation = store.state.generation;
    let host = store.state.host_secret;
    assert!(matches!(
        try_open(dir.path(), "agent-admission"),
        Err(AccessError::Busy)
    ));
    drop(store);
    let current = try_open(dir.path(), "agent-admission").unwrap();
    assert_eq!(current.state.generation, generation);
    assert_eq!(current.state.host_secret, host);
    assert_eq!(std::fs::read(dir.path().join("state.json")).unwrap(), bytes);
    assert!(matches!(
        try_open(dir.path(), "agent-admission"),
        Err(AccessError::Busy)
    ));
}

#[test]
fn delivery_access_missing_and_legacy_are_denied_without_provisioning() {
    let parent = tempfile::tempdir().unwrap();
    let absent = parent.path().join("absent");
    assert!(matches!(
        try_open(&absent, "agent-admission"),
        Err(AccessError::Denied(_))
    ));
    assert!(!absent.exists());
    assert!(matches!(
        try_open(parent.path(), "agent-admission"),
        Err(AccessError::Denied(_))
    ));
    assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 0);
    let (dir, store, _) = crate::agent_owner_tests::enrolled();
    drop(store);
    let bytes = std::fs::read(dir.path().join("state.json")).unwrap();
    assert!(matches!(
        try_open(dir.path(), "agent-admission"),
        Err(AccessError::Denied(_))
    ));
    assert_eq!(std::fs::read(dir.path().join("state.json")).unwrap(), bytes);
}

#[test]
fn delivery_access_reconciliation_or_corruption_is_denied_never_busy() {
    let (dir, store) = selected();
    drop(store);
    let path = dir.path().join("state.json");
    let bytes = std::fs::read(&path).unwrap();
    let result = with_existing_hooks(
        Some(ExistingCheckpoint::BeforeNamespaceFlush),
        |_| {},
        || try_open(dir.path(), "agent-admission"),
    );
    assert!(matches!(result, Err(AccessError::Denied(_))));
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert!(try_open(dir.path(), "agent-admission").is_ok());
    std::fs::write(&path, b"{broken").unwrap();
    assert!(matches!(
        try_open(dir.path(), "agent-admission"),
        Err(AccessError::Denied(_))
    ));
    assert_eq!(std::fs::read(&path).unwrap(), b"{broken");
}

#[test]
fn delivery_access_retains_exact_admitted_directory_handle() {
    let (dir, store) = selected();
    drop(store);
    let current = try_open(dir.path(), "agent-admission").unwrap();
    let expected =
        atomic_file::FileIdentity::of(&std::fs::File::open(dir.path()).unwrap()).unwrap();
    assert_eq!(
        atomic_file::FileIdentity::of(&current.directory).unwrap(),
        expected
    );
    let moved = dir.path().with_extension("moved");
    std::fs::rename(dir.path(), &moved).unwrap();
    assert_eq!(
        atomic_file::FileIdentity::of(&current.directory).unwrap(),
        expected
    );
    // Restore the test fixture name for TempDir cleanup; production has not reopened it.
    std::fs::rename(&moved, dir.path()).unwrap();
}

#[test]
fn delivery_access_publication_uses_original_locked_directory_after_rename() {
    use std::os::unix::fs::PermissionsExt;
    let (dir, mut store) = selected();
    let original = std::fs::read(dir.path().join("state.json")).unwrap();
    let holder = tempfile::tempdir().unwrap();
    let moved = holder.path().join("original");
    std::fs::rename(dir.path(), &moved).unwrap();
    std::fs::create_dir(dir.path()).unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(dir.path().join("state.json"), b"unrelated replacement").unwrap();
    let outcome = store.commit(store.state.clone());
    let retained = std::fs::read(moved.join("state.json")).unwrap();
    let replacement = std::fs::read(dir.path().join("state.json")).unwrap();
    drop(store);
    std::fs::remove_dir_all(dir.path()).unwrap();
    std::fs::rename(&moved, dir.path()).unwrap();
    outcome.unwrap();
    assert!(
        replacement == b"unrelated replacement",
        "the original lock cannot authorize a replacement namespace"
    );
    assert!(
        retained != original,
        "the original directory receives the actual committed generation"
    );
}
