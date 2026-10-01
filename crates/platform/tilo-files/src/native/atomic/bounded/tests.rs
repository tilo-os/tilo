#![expect(
    clippy::result_large_err,
    reason = "the tests return the production custody-carrying errors from closures"
)]
use super::*;
use std::{fs, os::unix::fs::PermissionsExt, process::Command};
fn private_root() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    root
}
fn acl(path: &std::path::Path, rule: &str) {
    assert!(
        Command::new("/bin/chmod")
            .args(["+a", rule])
            .arg(path)
            .status()
            .unwrap()
            .success()
    );
}
#[test]
fn bounded_privacy_local_no_acl_opens_actual_private_file() {
    let root = private_root();
    let path = root.path().join("data");
    fs::write(&path, b"private").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let held = BoundedOwnerDirectory::admit(StorageDirectory::open(root.path()).unwrap()).unwrap();
    held.validate().unwrap();
    let mut f = held.open_read(OsStr::new("data")).unwrap();
    let mut b = String::new();
    std::io::Read::read_to_string(&mut f, &mut b).unwrap();
    assert_eq!(b, "private");
    held.open_lease_existing(OsStr::new("data")).unwrap();
    assert!(held.open_lease_existing(OsStr::new("missing")).is_err());
    assert!(!root.path().join("missing").exists());
}
#[test]
fn bounded_privacy_deny_only_inherited_acl_is_supported() {
    let root = private_root();
    acl(
        root.path(),
        "everyone deny execute,file_inherit,only_inherit",
    );
    StorageDirectory::open(root.path())
        .unwrap()
        .validate_privacy(crate::native::atomic::Privacy::OwnerOnly)
        .unwrap();
    BoundedOwnerDirectory::admit(StorageDirectory::open(root.path()).unwrap()).unwrap();
}
#[test]
fn bounded_privacy_owner_allow_does_not_change_general_owner_only() {
    let root = private_root();
    let user = String::from_utf8(
        Command::new("/usr/bin/id")
            .arg("-un")
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    acl(
        root.path(),
        &format!("user:{} allow read,write,file_inherit", user.trim()),
    );
    let ordinary = StorageDirectory::open(root.path()).unwrap();
    ordinary
        .validate_privacy(crate::native::atomic::Privacy::OwnerOnly)
        .unwrap();
    assert!(BoundedOwnerDirectory::admit(ordinary).is_err());
}
#[test]
fn bounded_privacy_publication_and_existing_commit_use_same_original_directory() {
    let temp = private_root();
    let before = temp.path().join("parent");
    fs::create_dir(&before).unwrap();
    fs::set_permissions(&before, fs::Permissions::from_mode(0o700)).unwrap();
    let held = BoundedOwnerDirectory::admit(StorageDirectory::open(&before).unwrap()).unwrap();
    let moved = temp.path().join("moved");
    fs::rename(&before, &moved).unwrap();
    fs::create_dir(&before).unwrap();
    held.publish(
        OsStr::new("data"),
        b"committed",
        crate::native::atomic::PublishMode::Replace,
    )
    .unwrap();
    let f = held.open_read(OsStr::new("data")).unwrap();
    held.commit_existing(OsStr::new("data"), &FileIdentity::of(&f).unwrap())
        .unwrap();
    assert_eq!(fs::read(moved.join("data")).unwrap(), b"committed");
    assert!(!before.join("data").exists());
}
#[test]
fn bounded_privacy_fresh_child_normalizes_inherited_owner_allow_only_on_new_object() {
    let temp = private_root();
    let user = String::from_utf8(
        Command::new("/usr/bin/id")
            .arg("-un")
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    acl(
        temp.path(),
        &format!(
            "user:{} allow read,write,file_inherit,directory_inherit",
            user.trim()
        ),
    );
    let parent =
        OwnerOnlyProvisioning::admit(StorageDirectory::open(temp.path()).unwrap()).unwrap();
    let fresh = parent.create_child(OsStr::new("sender")).unwrap();
    let child = fresh.normalize_and_commit().unwrap();
    child
        .publish(
            OsStr::new("outbox"),
            b"retained",
            crate::native::atomic::PublishMode::Replace,
        )
        .unwrap();
    // Original root still has its allow: normalization must never target it.
    assert!(BoundedOwnerDirectory::admit(StorageDirectory::open(temp.path()).unwrap()).is_err());
    assert_eq!(
        fs::read(temp.path().join("sender/outbox")).unwrap(),
        b"retained"
    );
    let Err(err) = parent.create_child(OsStr::new("sender")) else {
        panic!("adopted existing child")
    };
    assert_eq!(err.state(), FreshChildState::NotCreated);
    assert_eq!(
        fs::read(temp.path().join("sender/outbox")).unwrap(),
        b"retained"
    );
}
#[test]
fn bounded_privacy_fresh_child_refuses_nonempty_and_replaced_name_without_normalizing() {
    let temp = private_root();
    let parent =
        OwnerOnlyProvisioning::admit(StorageDirectory::open(temp.path()).unwrap()).unwrap();
    let fresh = parent.create_child(OsStr::new("sender")).unwrap();
    fs::write(temp.path().join("sender/external"), b"do not touch").unwrap();
    assert!(fresh.normalize_and_commit().is_err());
    let fresh = parent.create_child(OsStr::new("other")).unwrap();
    fs::rename(temp.path().join("other"), temp.path().join("moved")).unwrap();
    fs::create_dir(temp.path().join("other")).unwrap();
    assert!(fresh.normalize_and_commit().is_err());
}
#[test]
fn bounded_privacy_current_child_acl_mode_and_link_checks_cannot_be_cached() {
    let temp = private_root();
    let path = temp.path().join("data");
    fs::write(&path, b"secret").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let held = BoundedOwnerDirectory::admit(StorageDirectory::open(temp.path()).unwrap()).unwrap();
    let f = held.open_read(OsStr::new("data")).unwrap();
    let identity = FileIdentity::of(&f).unwrap();
    acl(&path, "everyone allow read");
    assert!(held.open_read(OsStr::new("data")).is_err());
    assert!(held.commit_existing(OsStr::new("data"), &identity).is_err());
    assert!(
        Command::new("/bin/chmod")
            .arg("-N")
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    fs::hard_link(&path, temp.path().join("alias")).unwrap();
    assert!(held.open_read(OsStr::new("data")).is_err());
    fs::remove_file(temp.path().join("alias")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(held.open_read(OsStr::new("data")).is_err());
    std::os::unix::fs::symlink(&path, temp.path().join("link")).unwrap();
    assert!(held.open_read(OsStr::new("link")).is_err());
    fs::create_dir(temp.path().join("directory")).unwrap();
    assert!(held.open_read(OsStr::new("directory")).is_err());
    for name in ["", "..", ".", "a/b", "a\0b"] {
        assert!(held.open_read(OsStr::new(name)).is_err());
    }
    assert!(held.open_read(OsStr::new(&"a".repeat(201))).is_err());
}
#[test]
fn bounded_privacy_actual_128_unknown_principal_denies_require_no_membership() {
    use std::{ffi::c_void, os::fd::AsRawFd};
    unsafe extern "C" {
        fn acl_init(n: i32) -> *mut c_void;
        fn acl_free(p: *mut c_void) -> i32;
        fn acl_create_entry(acl: *mut *mut c_void, entry: *mut *mut c_void) -> i32;
        fn acl_set_tag_type(entry: *mut c_void, tag: i32) -> i32;
        fn acl_set_qualifier(entry: *mut c_void, guid: *const u8) -> i32;
        fn acl_set_fd_np(fd: i32, acl: *mut c_void, kind: i32) -> i32;
    }
    let temp = private_root();
    let dir = File::open(temp.path()).unwrap();
    // Test fixture only; the production bounded engine never invokes these APIs.
    unsafe {
        let mut acl = acl_init(128);
        assert!(!acl.is_null());
        for i in 0u8..128 {
            let mut e = std::ptr::null_mut();
            assert_eq!(acl_create_entry(&raw mut acl, &raw mut e), 0);
            assert_eq!(acl_set_tag_type(e, 2), 0);
            let mut uuid = [0x73u8; 16];
            uuid[15] = i;
            assert_eq!(acl_set_qualifier(e, uuid.as_ptr()), 0);
        }
        assert_eq!(acl_set_fd_np(dir.as_raw_fd(), acl, 0x100), 0);
        assert_eq!(acl_free(acl), 0);
    }
    BoundedOwnerDirectory::admit(StorageDirectory::from_file(dir).unwrap())
        .unwrap()
        .validate()
        .unwrap();
}
#[cfg(feature = "test-support")]
#[test]
fn bounded_privacy_failure_guards_retry_original_commit_without_republication() {
    use crate::native::atomic::test_support::{
        Checkpoint, ExistingCheckpoint, with_existing_hooks, with_failure,
    };
    let temp = private_root();
    let held = BoundedOwnerDirectory::admit(StorageDirectory::open(temp.path()).unwrap()).unwrap();
    let error = with_failure(Checkpoint::BeforeCommit, || {
        held.publish(
            OsStr::new("data"),
            b"original",
            crate::native::atomic::PublishMode::Replace,
        )
    })
    .unwrap_err();
    assert_eq!(
        error.state(),
        crate::native::atomic::PublicationState::PublishedDurabilityUnknown
    );
    assert_eq!(fs::read(temp.path().join("data")).unwrap(), b"original");
    error.retry_commit().unwrap();
    let f = held.open_read(OsStr::new("data")).unwrap();
    let identity = FileIdentity::of(&f).unwrap();
    let error = with_existing_hooks(
        Some(ExistingCheckpoint::BeforeNamespaceFlush),
        |_| {},
        || held.commit_existing(OsStr::new("data"), &identity),
    )
    .unwrap_err();
    fs::rename(temp.path().join("data"), temp.path().join("old")).unwrap();
    fs::write(temp.path().join("data"), b"replacement").unwrap();
    fs::set_permissions(temp.path().join("data"), fs::Permissions::from_mode(0o600)).unwrap();
    assert!(error.retry_commit().unwrap_err().source_changed());
    assert_eq!(fs::read(temp.path().join("data")).unwrap(), b"replacement");
}
#[test]
fn bounded_privacy_fresh_barrier_failure_keeps_exact_custody_and_retry() {
    for at in [
        super::fresh::FreshCheckpoint::Child,
        super::fresh::FreshCheckpoint::Parent,
    ] {
        let temp = private_root();
        let parent =
            OwnerOnlyProvisioning::admit(StorageDirectory::open(temp.path()).unwrap()).unwrap();
        let fresh = parent.create_child(OsStr::new("sender")).unwrap();
        let Err(error) = super::fresh::with_failure(at, || fresh.normalize_and_commit()) else {
            panic!("missing injected barrier")
        };
        assert_eq!(error.state(), FreshChildState::CreatedUncommitted);
        let held = error.retry_normalize_and_commit().unwrap();
        held.validate().unwrap();
    }
}
mod allocations;
mod fresh_admin;
#[test]
fn bounded_privacy_requested_heap_obeys_explicit_operation_costs() {
    use allocations::measure;
    let temp = private_root();
    let directory = StorageDirectory::open(temp.path()).unwrap();
    let held = measure(0, || BoundedOwnerDirectory::admit(directory)).unwrap();
    measure(0, || held.validate()).unwrap();
    let name = OsStr::new("data");
    let count = BoundedOwnerDirectory::publication_allocation_bytes(name).unwrap();
    measure(count, || {
        held.publish(
            name,
            b"actual bytes",
            crate::native::atomic::PublishMode::Replace,
        )
    })
    .unwrap();
    let f = measure(0, || held.open_read(name)).unwrap();
    let id = FileIdentity::of(&f).unwrap();
    measure(
        BoundedOwnerDirectory::existing_commit_allocation_bytes(name).unwrap(),
        || held.commit_existing(name, &id),
    )
    .unwrap();
    measure(0, || held.open_lease_existing(name)).unwrap();
    measure(0, || held.open_read(OsStr::new("absent"))).unwrap_err();
    // Parent admission is deliberately outside bounded measurement: ordinary
    // OwnerOnly may invoke native membership. The fresh child uses no resolver.
    let parent =
        OwnerOnlyProvisioning::admit(StorageDirectory::open(temp.path()).unwrap()).unwrap();
    let fresh = measure(0, || parent.create_child(OsStr::new("sender"))).unwrap();
    measure(0, || fresh.normalize_and_commit()).unwrap();
    let root_handle = StorageDirectory::open(temp.path()).unwrap();
    let child = measure(0, || {
        BoundedOwnerDirectory::open_child(&root_handle, OsStr::new("sender"))
    });
    assert!(child.is_ok());
}

#[test]
fn bounded_privacy_submitted_mkdir_error_preserves_unknown_effect_without_adoption() {
    let temp = private_root();
    let parent =
        OwnerOnlyProvisioning::admit(StorageDirectory::open(temp.path()).unwrap()).unwrap();
    let Err(error) = super::fresh::with_failure(super::fresh::FreshCheckpoint::AfterMkdir, || {
        parent.create_child(OsStr::new("sender"))
    }) else {
        panic!("missing submitted error")
    };
    assert!(temp.path().join("sender").is_dir());
    assert_eq!(error.state(), FreshChildState::CreationUnknown);
    let Err(error) = error.retry_normalize_and_commit() else {
        panic!("adopted unknown child")
    };
    assert_eq!(error.state(), FreshChildState::CreationUnknown);
    assert!(parent.create_child(OsStr::new("sender")).is_err());
}

#[test]
fn bounded_privacy_directory_child_validates_current_parent_before_open() {
    use allocations::measure;
    let temp = private_root();
    fs::create_dir(temp.path().join("sender")).unwrap();
    fs::set_permissions(
        temp.path().join("sender"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let held = BoundedOwnerDirectory::admit(StorageDirectory::open(temp.path()).unwrap()).unwrap();
    measure(0, || held.open_directory_child(OsStr::new("sender"))).unwrap();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o750)).unwrap();
    // The child itself remains private; skipping the parent recheck would pass.
    assert!(measure(0, || held.open_directory_child(OsStr::new("sender"))).is_err());
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    measure(0, || held.open_directory_child(OsStr::new("sender"))).unwrap();
}
#[test]
fn bounded_privacy_directory_child_rejects_symlink_without_following() {
    use allocations::measure;
    let temp = private_root();
    fs::create_dir(temp.path().join("target")).unwrap();
    fs::set_permissions(
        temp.path().join("target"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    std::os::unix::fs::symlink("target", temp.path().join("sender")).unwrap();
    let held = BoundedOwnerDirectory::admit(StorageDirectory::open(temp.path()).unwrap()).unwrap();
    assert!(measure(0, || held.open_directory_child(OsStr::new("sender"))).is_err());
}
#[test]
fn bounded_privacy_directory_child_observes_current_name_under_original_root() {
    use allocations::measure;
    let temp = private_root();
    let parent = temp.path().join("parent");
    fs::create_dir(&parent).unwrap();
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
    fs::create_dir(parent.join("sender")).unwrap();
    fs::set_permissions(parent.join("sender"), fs::Permissions::from_mode(0o700)).unwrap();
    let held = BoundedOwnerDirectory::admit(StorageDirectory::open(&parent).unwrap()).unwrap();
    let first = measure(0, || held.open_directory_child(OsStr::new("sender"))).unwrap();
    let original = first.identity().unwrap();
    let moved = temp.path().join("moved");
    fs::rename(&parent, &moved).unwrap();
    fs::create_dir(&parent).unwrap();
    // An ambient path reopen would fail: replacement parent contains no child.
    assert_eq!(
        measure(0, || held.open_directory_child(OsStr::new("sender")))
            .unwrap()
            .identity()
            .unwrap(),
        original
    );
    fs::rename(moved.join("sender"), moved.join("old")).unwrap();
    fs::create_dir(moved.join("sender")).unwrap();
    fs::set_permissions(moved.join("sender"), fs::Permissions::from_mode(0o700)).unwrap();
    let current = measure(0, || held.open_directory_child(OsStr::new("sender"))).unwrap();
    assert_ne!(current.identity().unwrap(), original);
    assert_eq!(first.identity().unwrap(), original);
    assert_eq!(
        current.identity().unwrap(),
        FileIdentity::of(&File::open(moved.join("sender")).unwrap()).unwrap()
    );
}
