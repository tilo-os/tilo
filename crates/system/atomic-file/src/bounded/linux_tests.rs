//! Actual-filesystem qualification of the bounded owner directory on Linux.
//!
//! Every case needs a task-private XFS root that the coordinator supplies:
//! `TILO_PRIVATE_ACTIONS_XFS_FIXTURES` names a `0700` directory owned by the
//! test principal on XFS, and `setfacl`/`getfacl` are installed. The general
//! test job has neither (its container root is overlayfs), so the cases are
//! ignored there and run explicitly:
//!
//! ```sh
//! TILO_PRIVATE_ACTIONS_XFS_FIXTURES=/mnt/xfs/private \
//!     cargo test -p tilo-atomic-file --lib bounded::linux_tests -- --ignored
//! ```
#![expect(
    clippy::result_large_err,
    reason = "the tests return the production custody-carrying errors from closures"
)]
use super::*;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::process::Command;
#[path = "tests/allocations.rs"]
mod allocations;
#[path = "tests/fresh_admin.rs"]
mod fresh_admin;

fn private_root() -> tempfile::TempDir {
    let base = std::env::var_os("TILO_PRIVATE_ACTIONS_XFS_FIXTURES")
        .expect("qualification requires an explicit task-private XFS fixture root");
    let held = File::open(&base).unwrap();
    let metadata = held.metadata().unwrap();
    assert!(metadata.is_dir());
    assert_eq!(metadata.uid(), rustix::process::geteuid().as_raw());
    assert_eq!(metadata.mode() & 0o777, 0o700);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "statfs magic numbers are 32-bit; the width and sign of `f_type` vary by target"
    )]
    let magic = rustix::fs::fstatfs(&held).unwrap().f_type as u32;
    assert_eq!(magic, 0x5846_5342);
    let temp = tempfile::tempdir_in(base).unwrap();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    temp
}

fn acl(path: &std::path::Path, entries: &str) {
    assert!(
        Command::new("setfacl")
            .args(["-m", entries])
            .arg(path)
            .status()
            .expect("setfacl is a required qualification tool")
            .success()
    );
}

#[test]
#[ignore = "native Linux XFS qualification; the coordinator supplies TILO_PRIVATE_ACTIONS_XFS_FIXTURES and the acl tools"]
fn bounded_linux_actual_xfs_admission_and_masked_named_acl() {
    use allocations::measure;
    let root = private_root();
    let foreign = if rustix::process::geteuid().as_raw() == 65534 {
        65533
    } else {
        65534
    };
    acl(root.path(), &format!("u:{foreign}:rwx,m::---"));
    assert_eq!(fs::metadata(root.path()).unwrap().mode() & 0o077, 0);
    let input = StorageDirectory::open(root.path()).unwrap();
    let held = measure(0, || BoundedOwnerDirectory::admit(input))
        .unwrap_or_else(|e| panic!("actual XFS private root refused: {:?}", e.io_error()));
    acl(root.path(), "m::r--");
    assert_eq!(fs::metadata(root.path()).unwrap().mode() & 0o070, 0o040);
    assert!(measure(0, || held.validate()).is_err());
    acl(root.path(), "m::---");
    measure(0, || held.validate()).unwrap();
}

#[test]
#[ignore = "native Linux XFS qualification; the coordinator supplies TILO_PRIVATE_ACTIONS_XFS_FIXTURES and the acl tools"]
fn bounded_linux_inherited_default_acl_keeps_actual_stage_private() {
    use allocations::measure;
    let root = private_root();
    let foreign = if rustix::process::geteuid().as_raw() == 65534 {
        65533
    } else {
        65534
    };
    acl(
        root.path(),
        &format!("d:u::rwx,d:u:{foreign}:rwx,d:g::rwx,d:m::rwx,d:o::rwx"),
    );
    let input = StorageDirectory::open(root.path()).unwrap();
    let held = measure(0, || BoundedOwnerDirectory::admit(input))
        .unwrap_or_else(|e| panic!("{:?}", e.io_error()));
    let name = OsStr::new("state");
    measure(
        BoundedOwnerDirectory::publication_allocation_bytes(name).unwrap(),
        || {
            held.publish(
                name,
                b"actual persisted bytes",
                crate::PublishMode::IfAbsent,
            )
        },
    )
    .unwrap();
    let file = measure(0, || held.open_read(name)).unwrap();
    assert_eq!(file.metadata().unwrap().mode() & 0o077, 0);
    let identity = FileIdentity::of(&file).unwrap();
    measure(
        BoundedOwnerDirectory::existing_commit_allocation_bytes(name).unwrap(),
        || held.commit_existing(name, &identity),
    )
    .unwrap();
    assert_eq!(
        fs::read(root.path().join(name)).unwrap(),
        b"actual persisted bytes"
    );
}

#[test]
#[ignore = "native Linux XFS qualification; the coordinator supplies TILO_PRIVATE_ACTIONS_XFS_FIXTURES and the acl tools"]
fn bounded_linux_current_objects_and_names_are_rechecked() {
    use allocations::measure;
    let root = private_root();
    let path = root.path().join("data");
    fs::write(&path, b"secret").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let held = BoundedOwnerDirectory::admit(StorageDirectory::open(root.path()).unwrap()).unwrap();
    let input = measure(0, || held.open_read(OsStr::new("data"))).unwrap();
    let identity = FileIdentity::of(&input).unwrap();
    let foreign = if rustix::process::geteuid().as_raw() == 65534 {
        65533
    } else {
        65534
    };
    acl(&path, &format!("u:{foreign}:r--,m::r--"));
    assert!(measure(0, || held.open_read(OsStr::new("data"))).is_err());
    assert!(measure(4, || held.commit_existing(OsStr::new("data"), &identity)).is_err());
    acl(&path, "m::---");
    measure(0, || held.open_read(OsStr::new("data"))).unwrap();
    fs::hard_link(&path, root.path().join("alias")).unwrap();
    assert!(measure(0, || held.open_read(OsStr::new("data"))).is_err());
    fs::remove_file(root.path().join("alias")).unwrap();
    std::os::unix::fs::symlink(&path, root.path().join("link")).unwrap();
    assert!(measure(0, || held.open_read(OsStr::new("link"))).is_err());
    fs::create_dir(root.path().join("directory")).unwrap();
    assert!(measure(0, || held.open_read(OsStr::new("directory"))).is_err());
    rustix::fs::mknodat(
        rustix::fs::CWD,
        root.path().join("fifo"),
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_raw_mode(0o600),
        0,
    )
    .unwrap();
    assert!(measure(0, || held.open_read(OsStr::new("fifo"))).is_err());
    for name in ["", ".", "..", "a/b", "a\0b"] {
        assert!(measure(0, || held.open_read(OsStr::new(name))).is_err());
    }
    let long = "a".repeat(201);
    assert!(measure(0, || held.open_read(OsStr::new(&long))).is_err());
}

#[test]
#[ignore = "native Linux XFS qualification; the coordinator supplies TILO_PRIVATE_ACTIONS_XFS_FIXTURES and the acl tools"]
fn bounded_linux_child_uses_retained_root_and_current_identity() {
    use allocations::measure;
    let root = private_root();
    let parent = root.path().join("parent");
    fs::create_dir(&parent).unwrap();
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
    fs::create_dir(parent.join("child")).unwrap();
    fs::set_permissions(parent.join("child"), fs::Permissions::from_mode(0o700)).unwrap();
    let held = BoundedOwnerDirectory::admit(StorageDirectory::open(&parent).unwrap()).unwrap();
    let child = measure(0, || held.open_directory_child(OsStr::new("child"))).unwrap();
    let original = child.identity().unwrap();
    let moved = root.path().join("moved");
    fs::rename(&parent, &moved).unwrap();
    fs::create_dir(&parent).unwrap();
    assert_eq!(
        measure(0, || held.open_directory_child(OsStr::new("child")))
            .unwrap()
            .identity()
            .unwrap(),
        original
    );
    child
        .publish(
            OsStr::new("data"),
            b"retained",
            crate::PublishMode::IfAbsent,
        )
        .unwrap();
    assert_eq!(fs::read(moved.join("child/data")).unwrap(), b"retained");
    fs::rename(moved.join("child"), moved.join("old")).unwrap();
    fs::create_dir(moved.join("child")).unwrap();
    fs::set_permissions(moved.join("child"), fs::Permissions::from_mode(0o700)).unwrap();
    assert_ne!(
        measure(0, || held.open_directory_child(OsStr::new("child")))
            .unwrap()
            .identity()
            .unwrap(),
        original
    );
    fs::set_permissions(&moved, fs::Permissions::from_mode(0o750)).unwrap();
    assert!(measure(0, || held.open_directory_child(OsStr::new("child"))).is_err());
}

#[cfg(feature = "test-support")]
#[test]
#[ignore = "native Linux XFS qualification; the coordinator supplies TILO_PRIVATE_ACTIONS_XFS_FIXTURES and the acl tools"]
fn bounded_linux_retry_keeps_original_errors_and_rechecks_parent_and_file() {
    use crate::test_support::{Checkpoint, ExistingCheckpoint, with_existing_hooks, with_failure};
    let root = private_root();
    let held = BoundedOwnerDirectory::admit(StorageDirectory::open(root.path()).unwrap()).unwrap();
    let error = with_failure(Checkpoint::BeforeCommit, || {
        held.publish(OsStr::new("data"), b"original", crate::PublishMode::Replace)
    })
    .unwrap_err();
    assert_eq!(
        error.state(),
        crate::PublicationState::PublishedDurabilityUnknown
    );
    error.retry_commit().unwrap();
    let file = held.open_read(OsStr::new("data")).unwrap();
    let identity = FileIdentity::of(&file).unwrap();
    let error = with_existing_hooks(
        Some(ExistingCheckpoint::BeforeNamespaceFlush),
        |_| {},
        || held.commit_existing(OsStr::new("data"), &identity),
    )
    .unwrap_err();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o750)).unwrap();
    let error = error.retry_commit().unwrap_err();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(root.path().join("data"), fs::Permissions::from_mode(0o640)).unwrap();
    let error = error.retry_commit().unwrap_err();
    fs::set_permissions(root.path().join("data"), fs::Permissions::from_mode(0o600)).unwrap();
    error.retry_commit().unwrap();
    let error = with_existing_hooks(
        Some(ExistingCheckpoint::BeforeNamespaceFlush),
        |_| {},
        || held.commit_existing(OsStr::new("data"), &identity),
    )
    .unwrap_err();
    fs::rename(root.path().join("data"), root.path().join("old")).unwrap();
    fs::write(root.path().join("data"), b"replacement").unwrap();
    fs::set_permissions(root.path().join("data"), fs::Permissions::from_mode(0o600)).unwrap();
    assert!(error.retry_commit().unwrap_err().source_changed());
    assert_eq!(fs::read(root.path().join("data")).unwrap(), b"replacement");
}

#[test]
#[ignore = "native Linux XFS qualification; the coordinator supplies TILO_PRIVATE_ACTIONS_XFS_FIXTURES and the acl tools"]
fn bounded_linux_descriptor_and_filesystem_refusals_do_not_allocate() {
    use allocations::measure;
    let proc = StorageDirectory::open(std::path::Path::new("/proc")).unwrap();
    let Err(error) = measure(0, || BoundedOwnerDirectory::admit(proc)) else {
        panic!("procfs admitted")
    };
    assert_eq!(error.io_error().kind(), io::ErrorKind::Unsupported);
    let root = private_root();
    let descriptor = rustix::fs::open(
        root.path(),
        rustix::fs::OFlags::PATH | rustix::fs::OFlags::DIRECTORY | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .unwrap();
    assert!(StorageDirectory::from_file(File::from(descriptor)).is_err());
}

#[test]
#[ignore = "native Linux XFS qualification; the coordinator supplies TILO_PRIVATE_ACTIONS_XFS_FIXTURES and the acl tools"]
fn bounded_linux_existing_sentinel_has_real_exclusive_lease() {
    use crate::lease::{AcquireError, FileLease, LeaseMode};
    use allocations::measure;
    let root = private_root();
    let path = root.path().join("writer.lock");
    fs::write(&path, b"").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let held = BoundedOwnerDirectory::admit(StorageDirectory::open(root.path()).unwrap()).unwrap();
    let file = measure(0, || held.open_lease_existing(OsStr::new("writer.lock"))).unwrap();
    let lease = measure(0, || FileLease::try_acquire(file, LeaseMode::Exclusive)).unwrap();
    let competing = measure(0, || held.open_lease_existing(OsStr::new("writer.lock"))).unwrap();
    assert!(matches!(
        measure(0, || FileLease::try_acquire(
            competing,
            LeaseMode::Exclusive
        )),
        Err(AcquireError::Busy)
    ));
    drop(lease);
    let fresh = measure(0, || held.open_lease_existing(OsStr::new("writer.lock"))).unwrap();
    measure(0, || FileLease::try_acquire(fresh, LeaseMode::Exclusive)).unwrap();
    assert!(measure(0, || held.open_lease_existing(OsStr::new("absent"))).is_err());
    assert!(!root.path().join("absent").exists());
}

#[test]
#[ignore = "native Linux XFS qualification; the coordinator supplies TILO_PRIVATE_ACTIONS_XFS_FIXTURES and the acl tools"]
fn bounded_fresh_admin_linux_inherited_acl_is_preserved_and_masked() {
    use allocations::measure;
    let temp = private_root();
    let foreign = if rustix::process::geteuid().as_raw() == 65534 {
        65533
    } else {
        65534
    };
    acl(
        temp.path(),
        &format!("d:u::rwx,d:u:{foreign}:rwx,d:g::rwx,d:m::rwx,d:o::rwx"),
    );
    let parent_before = Command::new("getfacl")
        .arg("-cpn")
        .arg(temp.path())
        .output()
        .unwrap();
    assert!(parent_before.status.success());
    let root = BoundedOwnerDirectory::admit(StorageDirectory::open(temp.path()).unwrap()).unwrap();
    let fresh = measure(0, || root.create_fresh_child(OsStr::new("sender"))).unwrap();
    measure(0, || fresh.normalize_and_commit()).unwrap();
    let parent_after = Command::new("getfacl")
        .arg("-cpn")
        .arg(temp.path())
        .output()
        .unwrap();
    assert!(parent_after.status.success());
    assert_eq!(parent_after.stdout, parent_before.stdout);
    let child = Command::new("getfacl")
        .arg("-cpn")
        .arg(temp.path().join("sender"))
        .output()
        .unwrap();
    assert!(child.status.success());
    let child = String::from_utf8(child.stdout).unwrap();
    assert!(
        child
            .lines()
            .any(|line| line.starts_with(&format!("user:{foreign}:rwx")))
    );
    assert!(child.lines().any(|line| line == "mask::---"));
    assert_eq!(
        fs::metadata(temp.path().join("sender"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

#[test]
#[ignore = "native Linux XFS qualification; the coordinator supplies TILO_PRIVATE_ACTIONS_XFS_FIXTURES and the acl tools"]
fn bounded_fresh_admin_linux_widened_child_refuses_before_chmod() {
    use allocations::measure;
    let temp = private_root();
    let root = BoundedOwnerDirectory::admit(StorageDirectory::open(temp.path()).unwrap()).unwrap();
    let fresh = root.create_fresh_child(OsStr::new("sender")).unwrap();
    fs::set_permissions(
        temp.path().join("sender"),
        fs::Permissions::from_mode(0o750),
    )
    .unwrap();
    let Err(error) = measure(0, || fresh.normalize_and_commit()) else {
        panic!("normalized widened child")
    };
    assert_eq!(
        error.io_error().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    assert_eq!(
        fs::metadata(temp.path().join("sender"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o750
    );
}
