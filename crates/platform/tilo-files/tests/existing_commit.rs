#![cfg(all(unix, feature = "test-support"))]
use std::{
    cell::RefCell,
    ffi::OsStr,
    fs::{self, File},
    io::Write,
    rc::Rc,
};
use tilo_files::native::atomic::lease::{AcquireError, LeaseMode};
use tilo_files::native::atomic::test_support::{
    ExistingCheckpoint as E, ProvisionCheckpoint as P, with_existing_hooks, with_provision_failure,
    with_provision_observer,
};
use tilo_files::native::atomic::{FileIdentity, Privacy, StorageDirectory, reconcile_directory};

#[test]
fn existing_commit_retains_nondeleting_retry_and_rejects_replacement() {
    for checkpoint in [E::BeforeDataFlush, E::BeforeNamespaceFlush] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("winner");
        fs::write(&path, b"owner\n").unwrap();
        let parent = StorageDirectory::open(root.path()).unwrap();
        let identity = FileIdentity::of(&File::open(&path).unwrap()).unwrap();
        let error = with_existing_hooks(
            Some(checkpoint),
            |_| {},
            || parent.commit_existing(OsStr::new("winner"), &identity, Privacy::Inherit),
        )
        .unwrap_err();
        assert!(!error.source_changed());
        assert!(tilo_files::native::atomic::publication_state(&error.into_io_error()).is_none());
        let error = with_existing_hooks(
            Some(checkpoint),
            |_| {},
            || parent.commit_existing(OsStr::new("winner"), &identity, Privacy::Inherit),
        )
        .unwrap_err();
        with_existing_hooks(
            Some(checkpoint),
            |_| {},
            || assert!(error.retry_commit().is_err()),
        );
        let error = with_existing_hooks(
            Some(checkpoint),
            |_| {},
            || parent.commit_existing(OsStr::new("winner"), &identity, Privacy::Inherit),
        )
        .unwrap_err();
        error.retry_commit().unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"owner\n");
        let error = with_existing_hooks(
            Some(checkpoint),
            |_| {},
            || parent.commit_existing(OsStr::new("winner"), &identity, Privacy::Inherit),
        )
        .unwrap_err();
        fs::rename(&path, root.path().join("old")).unwrap();
        fs::write(&path, b"owner\n").unwrap();
        assert!(error.retry_commit().unwrap_err().source_changed());
        assert_eq!(fs::read(&path).unwrap(), b"owner\n");
    }
}

#[test]
fn existing_reconciliation_commits_exact_chain_and_never_recreates() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("one/two");
    fs::create_dir_all(&path).unwrap();
    let directory = StorageDirectory::open(&path).unwrap();
    let physical = fs::canonicalize(&path).unwrap();
    let events = Rc::new(RefCell::new(Vec::new()));
    let observed = events.clone();
    let error = with_provision_failure(P::BeforeSync, physical.parent().unwrap(), || {
        reconcile_directory(&path, &directory, Privacy::Inherit)
    })
    .unwrap_err();
    with_provision_observer(
        move |event| {
            assert!(!matches!(event.checkpoint, P::BeforeCreate | P::Created));
            if event.checkpoint == P::BeforeSync {
                observed.borrow_mut().push(event.path.clone());
            }
        },
        || error.retry_commit(),
    )
    .unwrap();
    assert_eq!(
        *events.borrow(),
        physical
            .ancestors()
            .map(std::borrow::ToOwned::to_owned)
            .collect::<Vec<_>>()
    );
    let error = with_provision_failure(P::BeforeSync, &physical, || {
        reconcile_directory(&path, &directory, Privacy::Inherit)
    })
    .unwrap_err();
    fs::rename(&path, root.path().join("old")).unwrap();
    assert!(error.retry_commit().unwrap_err().source_changed());
    assert!(
        reconcile_directory(&path, &directory, Privacy::Inherit)
            .unwrap_err()
            .source_changed()
    );
    assert!(!path.exists());
    fs::create_dir(&path).unwrap();
    assert!(
        reconcile_directory(&path, &directory, Privacy::Inherit)
            .unwrap_err()
            .source_changed()
    );
}

#[test]
fn held_sentinel_and_append_refuse_symlinks_nonregular_and_replaced_parent() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("held");
    fs::create_dir(&path).unwrap();
    let directory = StorageDirectory::open(&path).unwrap();
    fs::write(path.join("data"), b"old").unwrap();
    let identity = FileIdentity::of(&File::open(path.join("data")).unwrap()).unwrap();
    assert!(
        directory
            .commit_existing(OsStr::new("data"), &identity, Privacy::OwnerOnly)
            .is_err()
    );
    fs::set_permissions(path.join("data"), fs::Permissions::from_mode(0o600)).unwrap();
    directory
        .commit_existing(OsStr::new("data"), &identity, Privacy::OwnerOnly)
        .unwrap();
    symlink("data", path.join("link")).unwrap();
    fs::create_dir(path.join("folder")).unwrap();
    for name in ["link", "folder", "../escape"] {
        assert!(
            directory
                .try_acquire_lease(OsStr::new(name), LeaseMode::Exclusive)
                .is_err()
        );
        assert!(
            directory
                .open_append_existing(OsStr::new(name), Privacy::Inherit)
                .is_err()
        );
        assert!(
            directory
                .commit_existing(OsStr::new(name), &identity, Privacy::Inherit)
                .is_err()
        );
    }
    let lease = directory
        .try_acquire_lease(OsStr::new("gate"), LeaseMode::Exclusive)
        .unwrap();
    assert!(matches!(
        directory.try_acquire_lease(OsStr::new("gate"), LeaseMode::Shared),
        Err(AcquireError::Busy)
    ));
    let gate_identity = lease.identity().unwrap();
    assert_eq!(
        gate_identity,
        FileIdentity::of(
            &directory
                .open_read(OsStr::new("gate"), Privacy::Inherit)
                .unwrap()
        )
        .unwrap()
    );
    drop(lease);
    assert!(
        directory
            .try_acquire_lease(OsStr::new("gate"), LeaseMode::Exclusive)
            .is_ok()
    );
    fs::rename(&path, root.path().join("moved")).unwrap();
    fs::create_dir(&path).unwrap();
    let mut file = directory
        .open_append_existing(OsStr::new("data"), Privacy::Inherit)
        .unwrap();
    file.write_all(b"+delta").unwrap();
    file.sync_all().unwrap();
    assert_eq!(
        fs::read(root.path().join("moved/data")).unwrap(),
        b"old+delta"
    );
    assert!(!path.join("data").exists());
    assert!(
        directory
            .open_append_existing(OsStr::new("absent"), Privacy::Inherit)
            .is_err()
    );
    assert!(!root.path().join("moved/absent").exists());
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "a raw legacy flock holder is the interoperability peer under test"
)]
fn legacy_flock_child() {
    use std::io::Read;
    let Some(path) = std::env::var_os("TILO_EXISTING_FLOCK_CHILD") else {
        return;
    };
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    let shared = std::env::var_os("TILO_EXISTING_FLOCK_SHARED").is_some();
    let native = std::env::var_os("TILO_EXISTING_FLOCK_NATIVE").is_some();
    let (_native_file, _lease) = if native {
        rustix::fs::flock(
            &file,
            if shared {
                rustix::fs::FlockOperation::NonBlockingLockShared
            } else {
                rustix::fs::FlockOperation::NonBlockingLockExclusive
            },
        )
        .unwrap();
        (Some(file), None)
    } else {
        (
            None,
            Some(
                tilo_files::native::atomic::lease::FileLease::try_acquire(
                    file,
                    if shared {
                        LeaseMode::Shared
                    } else {
                        LeaseMode::Exclusive
                    },
                )
                .unwrap(),
            ),
        )
    };
    println!("READY");
    std::io::stdout().flush().unwrap();
    std::io::stdin().read_to_end(&mut Vec::new()).unwrap();
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "a raw legacy flock holder is the interoperability peer under test"
)]
fn held_lease_interoperates_with_legacy_flock_in_real_processes() {
    use std::io::BufRead;
    use std::process::{Command, Stdio};
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    for native_child in [false, true] {
        for shared_child in [false, true] {
            for shared_parent in [false, true] {
                let root = tempfile::tempdir().unwrap();
                let path = root.path().join("gate");
                let mut command = Command::new(std::env::current_exe().unwrap());
                command
                    .args(["--exact", "legacy_flock_child", "--nocapture"])
                    .env("TILO_EXISTING_FLOCK_CHILD", &path)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped());
                if native_child {
                    command.env("TILO_EXISTING_FLOCK_NATIVE", "1");
                }
                if shared_child {
                    command.env("TILO_EXISTING_FLOCK_SHARED", "1");
                }
                let mut child = ChildGuard(command.spawn().unwrap());
                let mut reader = std::io::BufReader::new(child.0.stdout.take().unwrap());
                let mut line = String::new();
                loop {
                    line.clear();
                    assert!(
                        reader.read_line(&mut line).unwrap() > 0,
                        "child exited before ready"
                    );
                    if line.trim() == "READY" {
                        break;
                    }
                }
                let directory = StorageDirectory::open(root.path()).unwrap();
                if native_child {
                    let result = directory.try_acquire_lease(
                        OsStr::new("gate"),
                        if shared_parent {
                            LeaseMode::Shared
                        } else {
                            LeaseMode::Exclusive
                        },
                    );
                    assert_eq!(result.is_ok(), shared_parent && shared_child);
                    if !(shared_parent && shared_child) {
                        assert!(matches!(result, Err(AcquireError::Busy)));
                    }
                } else {
                    let file = std::fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open(&path)
                        .unwrap();
                    let result = rustix::fs::flock(
                        &file,
                        if shared_parent {
                            rustix::fs::FlockOperation::NonBlockingLockShared
                        } else {
                            rustix::fs::FlockOperation::NonBlockingLockExclusive
                        },
                    );
                    assert_eq!(result.is_ok(), shared_parent && shared_child);
                }
                drop(child.0.stdin.take());
                assert!(child.0.wait().unwrap().success());
                assert!(
                    directory
                        .try_acquire_lease(OsStr::new("gate"), LeaseMode::Exclusive)
                        .is_ok()
                );
            }
        }
    }
}
