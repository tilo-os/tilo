//! Fresh-child administration cases shared by the macOS suite and the Linux
//! XFS qualification suite; each parent supplies `private_root()`.
use super::*;
#[test]
#[cfg_attr(
    target_os = "linux",
    ignore = "native Linux XFS qualification; the coordinator supplies TILO_PRIVATE_ACTIONS_XFS_FIXTURES and the acl tools"
)]
fn bounded_fresh_admin_creates_from_actual_retained_root() {
    use super::allocations::measure;
    let temp = private_root();
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
#[cfg_attr(
    target_os = "linux",
    ignore = "native Linux XFS qualification; the coordinator supplies TILO_PRIVATE_ACTIONS_XFS_FIXTURES and the acl tools"
)]
fn bounded_fresh_admin_existing_names_are_never_adopted() {
    use super::allocations::measure;
    let temp = private_root();
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
        let Err(error) = measure(0, || root.create_fresh_child(OsStr::new(name))) else {
            panic!("adopted existing name")
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
#[cfg_attr(
    target_os = "linux",
    ignore = "native Linux XFS qualification; the coordinator supplies TILO_PRIVATE_ACTIONS_XFS_FIXTURES and the acl tools"
)]
fn bounded_fresh_admin_parent_change_prevents_submission() {
    use super::allocations::measure;
    let temp = private_root();
    let root = BoundedOwnerDirectory::admit(StorageDirectory::open(temp.path()).unwrap()).unwrap();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o750)).unwrap();
    let Err(error) = measure(0, || root.create_fresh_child(OsStr::new("absent"))) else {
        panic!("created under widened parent")
    };
    assert_eq!(error.state(), FreshChildState::NotCreated);
    assert_eq!(
        error.io_error().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    assert!(!temp.path().join("absent").exists());
}

#[test]
#[cfg_attr(
    target_os = "linux",
    ignore = "native Linux XFS qualification; the coordinator supplies TILO_PRIVATE_ACTIONS_XFS_FIXTURES and the acl tools"
)]
fn bounded_fresh_admin_renamed_root_keeps_original_parent_barrier() {
    use super::allocations::measure;
    let temp = private_root();
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
#[cfg_attr(
    target_os = "linux",
    ignore = "native Linux XFS qualification; the coordinator supplies TILO_PRIVATE_ACTIONS_XFS_FIXTURES and the acl tools"
)]
fn bounded_fresh_admin_parent_and_nonempty_retry_keep_original_custody() {
    use super::allocations::measure;
    let temp = private_root();
    let root = BoundedOwnerDirectory::admit(StorageDirectory::open(temp.path()).unwrap()).unwrap();
    let fresh = measure(0, || root.create_fresh_child(OsStr::new("sender"))).unwrap();
    let Err(error) = crate::native::atomic::bounded::fresh::with_failure(
        crate::native::atomic::bounded::fresh::FreshCheckpoint::Child,
        || fresh.normalize_and_commit(),
    ) else {
        panic!("missing child barrier failure")
    };
    assert_eq!(error.state(), FreshChildState::CreatedUncommitted);
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o750)).unwrap();
    let Err(error) = measure(0, || error.retry_normalize_and_commit()) else {
        panic!("retry ignored changed parent")
    };
    assert_eq!(
        error.io_error().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(temp.path().join("sender/.new"), b"preserve").unwrap();
    let Err(error) = measure(0, || error.retry_normalize_and_commit()) else {
        panic!("retry reused EOF or erased new entry")
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
#[cfg_attr(
    target_os = "linux",
    ignore = "native Linux XFS qualification; the coordinator supplies TILO_PRIVATE_ACTIONS_XFS_FIXTURES and the acl tools"
)]
fn bounded_fresh_admin_replaced_child_is_not_normalized() {
    use super::allocations::measure;
    let temp = private_root();
    let root = BoundedOwnerDirectory::admit(StorageDirectory::open(temp.path()).unwrap()).unwrap();
    let fresh = root.create_fresh_child(OsStr::new("sender")).unwrap();
    fs::rename(temp.path().join("sender"), temp.path().join("old")).unwrap();
    fs::create_dir(temp.path().join("sender")).unwrap();
    fs::set_permissions(
        temp.path().join("sender"),
        fs::Permissions::from_mode(0o750),
    )
    .unwrap();
    let Err(error) = measure(0, || fresh.normalize_and_commit()) else {
        panic!("adopted replacement child")
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
#[cfg_attr(
    target_os = "linux",
    ignore = "native Linux XFS qualification; the coordinator supplies TILO_PRIVATE_ACTIONS_XFS_FIXTURES and the acl tools"
)]
fn bounded_fresh_admin_ambiguous_mkdir_never_recovers_by_name() {
    use super::allocations::measure;
    let temp = private_root();
    let root = BoundedOwnerDirectory::admit(StorageDirectory::open(temp.path()).unwrap()).unwrap();
    let Err(error) = crate::native::atomic::bounded::fresh::with_failure(
        crate::native::atomic::bounded::fresh::FreshCheckpoint::AfterMkdir,
        || root.create_fresh_child(OsStr::new("sender")),
    ) else {
        panic!("missing mkdir uncertainty")
    };
    assert!(temp.path().join("sender").is_dir());
    assert_eq!(error.state(), FreshChildState::CreationUnknown);
    let Err(error) = measure(0, || error.retry_normalize_and_commit()) else {
        panic!("adopted unknown child")
    };
    assert_eq!(error.state(), FreshChildState::CreationUnknown);
    assert!(measure(0, || root.create_fresh_child(OsStr::new("sender"))).is_err());
}

#[test]
#[cfg_attr(
    target_os = "linux",
    ignore = "native Linux XFS qualification; the coordinator supplies TILO_PRIVATE_ACTIONS_XFS_FIXTURES and the acl tools"
)]
fn bounded_fresh_admin_each_barrier_failure_retries_original_child() {
    use super::allocations::measure;
    for at in [
        crate::native::atomic::bounded::fresh::FreshCheckpoint::Child,
        crate::native::atomic::bounded::fresh::FreshCheckpoint::Parent,
    ] {
        let temp = private_root();
        let root =
            BoundedOwnerDirectory::admit(StorageDirectory::open(temp.path()).unwrap()).unwrap();
        let fresh = root.create_fresh_child(OsStr::new("sender")).unwrap();
        let before = root
            .open_directory_child(OsStr::new("sender"))
            .unwrap()
            .identity()
            .unwrap();
        let Err(error) = crate::native::atomic::bounded::fresh::with_failure(at, || {
            fresh.normalize_and_commit()
        }) else {
            panic!("missing barrier refusal")
        };
        assert_eq!(error.state(), FreshChildState::CreatedUncommitted);
        let child = measure(0, || error.retry_normalize_and_commit()).unwrap();
        assert_eq!(child.identity().unwrap(), before);
    }
}

#[test]
#[cfg_attr(
    target_os = "linux",
    ignore = "native Linux XFS qualification; the coordinator supplies TILO_PRIVATE_ACTIONS_XFS_FIXTURES and the acl tools"
)]
fn bounded_fresh_admin_nonempty_child_preserves_each_entry() {
    use super::allocations::measure;
    for (name, directory) in [
        ("ordinary".to_owned(), false),
        (".hidden".to_owned(), false),
        ("subdirectory".to_owned(), true),
        ("x".repeat(255), false),
    ] {
        let temp = private_root();
        let root =
            BoundedOwnerDirectory::admit(StorageDirectory::open(temp.path()).unwrap()).unwrap();
        let fresh = root.create_fresh_child(OsStr::new("sender")).unwrap();
        let entry = temp.path().join("sender").join(name);
        if directory {
            fs::create_dir(&entry).unwrap();
        } else {
            fs::write(&entry, b"untouched").unwrap();
        }
        let Err(error) = measure(0, || fresh.normalize_and_commit()) else {
            panic!("normalized nonempty child")
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
