use super::*;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::process::Command;
#[path = "tests/allocations.rs"]
mod allocations;

fn xfs_fixture() -> tempfile::TempDir {
    let base = std::env::var_os("TILO_PRIVATE_ACTIONS_XFS_FIXTURES")
        .expect("qualification requires an explicit task-private XFS fixture root");
    let held = File::open(&base).unwrap();
    let metadata = held.metadata().unwrap();
    assert!(metadata.is_dir());
    assert_eq!(metadata.uid(), rustix::process::geteuid().as_raw());
    assert_eq!(metadata.mode() & 0o777, 0o700);
    assert_eq!(
        rustix::fs::fstatfs(&held).unwrap().f_type as u32,
        0x58465342
    );
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
fn bounded_linux_actual_xfs_admission_and_masked_named_acl() {
    use allocations::measure;
    let root = xfs_fixture();
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
fn bounded_linux_inherited_default_acl_keeps_actual_stage_private() {
    use allocations::measure;
    let root = xfs_fixture();
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
fn bounded_linux_current_objects_and_names_are_rechecked() {
    use allocations::measure;
    let root = xfs_fixture();
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
fn bounded_linux_child_uses_retained_root_and_current_identity() {
    use allocations::measure;
    let root = xfs_fixture();
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
fn bounded_linux_retry_keeps_original_errors_and_rechecks_parent_and_file() {
    use crate::test_support::{Checkpoint, ExistingCheckpoint, with_existing_hooks, with_failure};
    let root = xfs_fixture();
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
fn bounded_linux_descriptor_and_filesystem_refusals_do_not_allocate() {
    use allocations::measure;
    let proc = StorageDirectory::open(std::path::Path::new("/proc")).unwrap();
    let error = match measure(0, || BoundedOwnerDirectory::admit(proc)) {
        Ok(_) => panic!("procfs admitted"),
        Err(error) => error,
    };
    assert_eq!(error.io_error().kind(), io::ErrorKind::Unsupported);
    let root = xfs_fixture();
    let descriptor = rustix::fs::open(
        root.path(),
        rustix::fs::OFlags::PATH | rustix::fs::OFlags::DIRECTORY | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .unwrap();
    assert!(StorageDirectory::from_file(File::from(descriptor)).is_err());
}

#[test]
fn bounded_linux_existing_sentinel_has_real_exclusive_lease() {
    use crate::lease::{AcquireError, FileLease, LeaseMode};
    use allocations::measure;
    let root = xfs_fixture();
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
fn bounded_fresh_admin_creates_from_actual_retained_root() {
    use allocations::measure;
    let temp = xfs_fixture();
    let original = StorageDirectory::open(temp.path()).unwrap();
    let root_identity = original.identity().unwrap();
    let root = measure(0, || BoundedOwnerDirectory::admit(original)).unwrap();
    let fresh = measure(0, || root.create_fresh_child(OsStr::new("sender")))
        .unwrap_or_else(|error| panic!("actual bounded root must create fresh child: {error:?}"));
    let child = measure(0, || fresh.normalize_and_commit()).unwrap();
    assert_eq!(root.identity().unwrap(), root_identity);
    assert_eq!(
        fs::metadata(temp.path().join("sender"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert!(
        fs::read_dir(temp.path().join("sender"))
            .unwrap()
            .next()
            .is_none()
    );
    assert_eq!(
        child.identity().unwrap(),
        root.open_directory_child(OsStr::new("sender"))
            .unwrap()
            .identity()
            .unwrap()
    );
}

#[test]
fn bounded_fresh_admin_existing_names_are_never_adopted() {
    use allocations::measure;
    let temp = xfs_fixture();
    fs::create_dir(temp.path().join("directory")).unwrap();
    fs::set_permissions(
        temp.path().join("directory"),
        fs::Permissions::from_mode(0o750),
    )
    .unwrap();
    fs::write(temp.path().join("file"), b"preserve").unwrap();
    std::os::unix::fs::symlink("directory", temp.path().join("link")).unwrap();
    let root = BoundedOwnerDirectory::admit(StorageDirectory::open(temp.path()).unwrap()).unwrap();
    for name in ["directory", "file", "link"] {
        let error = match measure(0, || root.create_fresh_child(OsStr::new(name))) {
            Ok(_) => panic!("adopted existing name"),
            Err(error) => error,
        };
        assert_eq!(error.state(), FreshChildState::NotCreated);
        assert_eq!(error.io_error().kind(), std::io::ErrorKind::AlreadyExists);
        assert!(error.retry_normalize_and_commit().is_err());
    }
    assert_eq!(
        fs::metadata(temp.path().join("directory"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o750
    );
    assert_eq!(fs::read(temp.path().join("file")).unwrap(), b"preserve");
    assert!(
        fs::symlink_metadata(temp.path().join("link"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn bounded_fresh_admin_parent_change_prevents_submission() {
    use allocations::measure;
    let temp = xfs_fixture();
    let root = BoundedOwnerDirectory::admit(StorageDirectory::open(temp.path()).unwrap()).unwrap();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o750)).unwrap();
    let error = match measure(0, || root.create_fresh_child(OsStr::new("absent"))) {
        Ok(_) => panic!("created under widened parent"),
        Err(error) => error,
    };
    assert_eq!(error.state(), FreshChildState::NotCreated);
    assert_eq!(
        error.io_error().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    assert!(!temp.path().join("absent").exists());
}

#[test]
fn bounded_fresh_admin_renamed_root_keeps_original_parent_barrier() {
    use allocations::measure;
    let temp = xfs_fixture();
    let original = temp.path().join("original");
    let moved = temp.path().join("moved");
    fs::create_dir(&original).unwrap();
    fs::set_permissions(&original, fs::Permissions::from_mode(0o700)).unwrap();
    let root = BoundedOwnerDirectory::admit(StorageDirectory::open(&original).unwrap()).unwrap();
    fs::rename(&original, &moved).unwrap();
    fs::create_dir(&original).unwrap();
    let fresh = measure(0, || root.create_fresh_child(OsStr::new("sender"))).unwrap();
    measure(0, || fresh.normalize_and_commit()).unwrap();
    assert!(moved.join("sender").is_dir());
    assert!(!original.join("sender").exists());
}

#[test]
fn bounded_fresh_admin_parent_and_nonempty_retry_keep_original_custody() {
    use allocations::measure;
    let temp = xfs_fixture();
    let root = BoundedOwnerDirectory::admit(StorageDirectory::open(temp.path()).unwrap()).unwrap();
    let fresh = measure(0, || root.create_fresh_child(OsStr::new("sender"))).unwrap();
    let error = match super::fresh::with_failure(super::fresh::FreshCheckpoint::Child, || {
        fresh.normalize_and_commit()
    }) {
        Ok(_) => panic!("missing child barrier failure"),
        Err(error) => error,
    };
    assert_eq!(error.state(), FreshChildState::CreatedUncommitted);
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o750)).unwrap();
    let error = match measure(0, || error.retry_normalize_and_commit()) {
        Ok(_) => panic!("retry ignored changed parent"),
        Err(error) => error,
    };
    assert_eq!(
        error.io_error().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(temp.path().join("sender/.new"), b"preserve").unwrap();
    let error = match measure(0, || error.retry_normalize_and_commit()) {
        Ok(_) => panic!("retry reused EOF or erased new entry"),
        Err(error) => error,
    };
    assert_eq!(
        error.io_error().kind(),
        std::io::ErrorKind::DirectoryNotEmpty
    );
    assert_eq!(
        fs::read(temp.path().join("sender/.new")).unwrap(),
        b"preserve"
    );
    fs::remove_file(temp.path().join("sender/.new")).unwrap();
    let child = measure(0, || error.retry_normalize_and_commit()).unwrap();
    assert_eq!(
        child.identity().unwrap(),
        root.open_directory_child(OsStr::new("sender"))
            .unwrap()
            .identity()
            .unwrap()
    );
}

#[test]
fn bounded_fresh_admin_replaced_child_is_not_normalized() {
    use allocations::measure;
    let temp = xfs_fixture();
    let root = BoundedOwnerDirectory::admit(StorageDirectory::open(temp.path()).unwrap()).unwrap();
    let fresh = root.create_fresh_child(OsStr::new("sender")).unwrap();
    fs::rename(temp.path().join("sender"), temp.path().join("old")).unwrap();
    fs::create_dir(temp.path().join("sender")).unwrap();
    fs::set_permissions(
        temp.path().join("sender"),
        fs::Permissions::from_mode(0o750),
    )
    .unwrap();
    let error = match measure(0, || fresh.normalize_and_commit()) {
        Ok(_) => panic!("adopted replacement child"),
        Err(error) => error,
    };
    assert_eq!(error.state(), FreshChildState::CreatedUncommitted);
    assert_eq!(
        fs::metadata(temp.path().join("sender"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o750
    );
    assert!(measure(0, || error.retry_normalize_and_commit()).is_err());
}

#[test]
fn bounded_fresh_admin_ambiguous_mkdir_never_recovers_by_name() {
    use allocations::measure;
    let temp = xfs_fixture();
    let root = BoundedOwnerDirectory::admit(StorageDirectory::open(temp.path()).unwrap()).unwrap();
    let error = match super::fresh::with_failure(super::fresh::FreshCheckpoint::AfterMkdir, || {
        root.create_fresh_child(OsStr::new("sender"))
    }) {
        Ok(_) => panic!("missing mkdir uncertainty"),
        Err(error) => error,
    };
    assert!(temp.path().join("sender").is_dir());
    assert_eq!(error.state(), FreshChildState::CreationUnknown);
    let error = match measure(0, || error.retry_normalize_and_commit()) {
        Ok(_) => panic!("adopted unknown child"),
        Err(error) => error,
    };
    assert_eq!(error.state(), FreshChildState::CreationUnknown);
    assert!(measure(0, || root.create_fresh_child(OsStr::new("sender"))).is_err());
}

#[test]
fn bounded_fresh_admin_each_barrier_failure_retries_original_child() {
    use allocations::measure;
    for at in [
        super::fresh::FreshCheckpoint::Child,
        super::fresh::FreshCheckpoint::Parent,
    ] {
        let temp = xfs_fixture();
        let root =
            BoundedOwnerDirectory::admit(StorageDirectory::open(temp.path()).unwrap()).unwrap();
        let fresh = root.create_fresh_child(OsStr::new("sender")).unwrap();
        let before = root
            .open_directory_child(OsStr::new("sender"))
            .unwrap()
            .identity()
            .unwrap();
        let error = match super::fresh::with_failure(at, || fresh.normalize_and_commit()) {
            Ok(_) => panic!("missing barrier refusal"),
            Err(error) => error,
        };
        assert_eq!(error.state(), FreshChildState::CreatedUncommitted);
        let child = measure(0, || error.retry_normalize_and_commit()).unwrap();
        assert_eq!(child.identity().unwrap(), before);
    }
}

#[test]
fn bounded_fresh_admin_nonempty_child_preserves_each_entry() {
    use allocations::measure;
    for (name, directory) in [
        ("ordinary".to_owned(), false),
        (".hidden".to_owned(), false),
        ("subdirectory".to_owned(), true),
        ("x".repeat(255), false),
    ] {
        let temp = xfs_fixture();
        let root =
            BoundedOwnerDirectory::admit(StorageDirectory::open(temp.path()).unwrap()).unwrap();
        let fresh = root.create_fresh_child(OsStr::new("sender")).unwrap();
        let entry = temp.path().join("sender").join(name);
        if directory {
            fs::create_dir(&entry).unwrap();
        } else {
            fs::write(&entry, b"untouched").unwrap();
        }
        let error = match measure(0, || fresh.normalize_and_commit()) {
            Ok(_) => panic!("normalized nonempty child"),
            Err(error) => error,
        };
        assert_eq!(
            error.io_error().kind(),
            std::io::ErrorKind::DirectoryNotEmpty
        );
        drop(error);
        if directory {
            assert!(entry.is_dir());
        } else {
            assert_eq!(fs::read(entry).unwrap(), b"untouched");
        }
    }
}

#[test]
fn bounded_fresh_admin_linux_inherited_acl_is_preserved_and_masked() {
    use allocations::measure;
    let temp = xfs_fixture();
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
fn bounded_fresh_admin_linux_widened_child_refuses_before_chmod() {
    use allocations::measure;
    let temp = xfs_fixture();
    let root = BoundedOwnerDirectory::admit(StorageDirectory::open(temp.path()).unwrap()).unwrap();
    let fresh = root.create_fresh_child(OsStr::new("sender")).unwrap();
    fs::set_permissions(
        temp.path().join("sender"),
        fs::Permissions::from_mode(0o750),
    )
    .unwrap();
    let error = match measure(0, || fresh.normalize_and_commit()) {
        Ok(_) => panic!("normalized widened child"),
        Err(error) => error,
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
