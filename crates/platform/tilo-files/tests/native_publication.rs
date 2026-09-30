#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

#[cfg(unix)]
mod unix_tests {
    use std::ffi::OsStr;
    use std::fs::{self, File};
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::process::{Child, Command};
    #[cfg(feature = "test-support")]
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    use tilo_files::native::atomic::{
        FileIdentity, Privacy, PublicationState, PublishMode, StorageDirectory,
    };
    #[cfg(feature = "test-support")]
    use tilo_files::native::atomic::{
        PublicationError, publication_state,
        test_support::{Checkpoint, with_failure, with_observer},
    };

    const UMASK_CHILD: &str = "TILO_ATOMIC_FILE_UMASK_CHILD";
    const UMASK_PATH: &str = "TILO_ATOMIC_FILE_UMASK_PATH";
    const FIFO_CHILD: &str = "TILO_ATOMIC_FILE_FIFO_CHILD";
    const FIFO_ROOT: &str = "TILO_ATOMIC_FILE_FIFO_ROOT";

    #[cfg(target_os = "linux")]
    #[test]
    fn held_directory_privacy_rejects_unproven_permission_profile() {
        // Native procfs is deliberately outside the journal permission profiles.
        // A missing proc mount is a failed prerequisite, never a skipped proof.
        let directory = StorageDirectory::open(Path::new("/proc")).unwrap();
        assert_eq!(
            directory
                .validate_privacy(Privacy::OwnerOnly)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::Unsupported
        );
        directory.validate_privacy(Privacy::Inherit).unwrap();
    }

    #[test]
    fn held_directory_privacy_admission_uses_directory_policy_and_held_object() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("journal");
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(path.join("child")).unwrap();
        let directory = StorageDirectory::open(&path).unwrap();
        directory.validate_privacy(Privacy::OwnerOnly).unwrap();
        let moved = root.path().join("moved");
        fs::rename(&path, &moved).unwrap();
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o777)).unwrap();
        directory.validate_privacy(Privacy::OwnerOnly).unwrap();
        fs::set_permissions(&moved, fs::Permissions::from_mode(0o770)).unwrap();
        assert_eq!(
            directory
                .validate_privacy(Privacy::OwnerOnly)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::PermissionDenied
        );
        directory.validate_privacy(Privacy::Inherit).unwrap();
    }

    #[test]
    fn held_directory_survives_ambient_path_replacement() {
        let root = tempfile::tempdir().unwrap();
        let admitted = root.path().join("admitted");
        let moved = root.path().join("moved");
        fs::create_dir(&admitted).unwrap();
        let directory = StorageDirectory::open(&admitted).unwrap();

        fs::rename(&admitted, &moved).unwrap();
        fs::create_dir(&admitted).unwrap();
        directory
            .publish(
                OsStr::new("state"),
                b"held",
                PublishMode::Replace,
                Privacy::Inherit,
            )
            .unwrap();

        assert_eq!(fs::read(moved.join("state")).unwrap(), b"held");
        assert!(!admitted.join("state").exists());
    }

    #[test]
    fn identity_uses_the_open_file_object_and_full_unix_pair() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state");
        let link = root.path().join("link");
        fs::write(&path, b"old").unwrap();
        fs::hard_link(&path, &link).unwrap();
        let held = File::open(&path).unwrap();
        let original = FileIdentity::of(&held).unwrap();

        assert_eq!(
            original,
            FileIdentity::of(&File::open(&link).unwrap()).unwrap()
        );
        tilo_files::native::atomic::replace(&path, b"new", Privacy::Inherit).unwrap();
        let replacement = FileIdentity::of(&File::open(&path).unwrap()).unwrap();
        assert_ne!(original, replacement);
        assert_eq!(original, FileIdentity::of(&held).unwrap());
        assert_eq!(original.volume(), replacement.volume());
    }

    #[test]
    fn no_replace_collision_preserves_the_winner() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state");
        fs::write(&path, b"winner").unwrap();

        let error =
            tilo_files::native::atomic::publish_if_absent(&path, b"loser", Privacy::Inherit)
                .unwrap_err();

        assert_eq!(error.state(), PublicationState::NotPublished);
        assert_eq!(error.io_error().kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&path).unwrap(), b"winner");
        assert_no_stages(root.path());
    }

    #[test]
    fn private_read_rejects_symlinks_and_non_private_files() {
        let root = tempfile::tempdir().unwrap();
        let directory = StorageDirectory::open(root.path()).unwrap();
        fs::write(root.path().join("broad"), b"secret").unwrap();
        fs::set_permissions(root.path().join("broad"), fs::Permissions::from_mode(0o644)).unwrap();
        std::os::unix::fs::symlink("broad", root.path().join("link")).unwrap();

        assert!(
            directory
                .open_read(OsStr::new("broad"), Privacy::OwnerOnly)
                .is_err()
        );
        assert!(
            directory
                .open_read(OsStr::new("link"), Privacy::Inherit)
                .is_err()
        );

        fs::set_permissions(root.path().join("broad"), fs::Permissions::from_mode(0o600)).unwrap();
        fs::hard_link(root.path().join("broad"), root.path().join("hard-link")).unwrap();
        assert!(
            directory
                .open_read(OsStr::new("broad"), Privacy::OwnerOnly)
                .is_err()
        );
    }

    #[test]
    fn child_names_are_single_components() {
        let root = tempfile::tempdir().unwrap();
        let directory = StorageDirectory::open(root.path()).unwrap();

        for invalid in ["", ".", "..", "nested/state", "/absolute"] {
            assert!(
                directory
                    .publish(
                        OsStr::new(invalid),
                        b"data",
                        PublishMode::Replace,
                        Privacy::Inherit,
                    )
                    .is_err(),
                "accepted invalid component {invalid:?}"
            );
        }
    }

    #[test]
    fn fifo_reads_are_rejected_without_blocking_for_both_privacy_modes() {
        if std::env::var_os(FIFO_CHILD).as_deref() == Some(OsStr::new("1")) {
            let root = std::path::PathBuf::from(std::env::var_os(FIFO_ROOT).unwrap());
            let directory = StorageDirectory::open(&root).unwrap();
            for privacy in [Privacy::Inherit, Privacy::OwnerOnly] {
                assert!(directory.open_read(OsStr::new("fifo"), privacy).is_err());
            }
            return;
        }

        let root = tempfile::tempdir().unwrap();
        let fifo = root.path().join("fifo");
        let status = Command::new("/usr/bin/mkfifo").arg(&fifo).status().unwrap();
        assert!(status.success(), "could not create FIFO fixture: {status}");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "unix_tests::fifo_reads_are_rejected_without_blocking_for_both_privacy_modes",
                "--nocapture",
            ])
            .env(FIFO_CHILD, "1")
            .env(FIFO_ROOT, root.path())
            .spawn()
            .unwrap();

        let status = wait_for_child(&mut child).unwrap_or_else(|| {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!(
                "FIFO validation blocked past the {:?} hang bound",
                tilo_test_env::hang_bound()
            );
        });
        assert!(status.success(), "FIFO validation child failed: {status}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn path_only_directory_handle_is_rejected_before_target_mutation() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("state");
        fs::write(&target, b"old").unwrap();
        let descriptor = rustix::fs::open(
            root.path(),
            rustix::fs::OFlags::PATH | rustix::fs::OFlags::DIRECTORY | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .unwrap();

        assert!(StorageDirectory::from_file(File::from(descriptor)).is_err());
        assert_eq!(fs::read(target).unwrap(), b"old");
    }

    #[test]
    fn legacy_inherit_honors_restrictive_umask() {
        if std::env::var_os(UMASK_CHILD).as_deref() == Some(OsStr::new("1")) {
            let path = std::path::PathBuf::from(std::env::var_os(UMASK_PATH).unwrap());
            rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o077));
            tilo_files::native::atomic::write(&path, b"state").unwrap();
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            return;
        }

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state");
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "unix_tests::legacy_inherit_honors_restrictive_umask",
                "--nocapture",
            ])
            .env(UMASK_CHILD, "1")
            .env(UMASK_PATH, &path)
            .status()
            .unwrap();
        assert!(status.success(), "umask child failed: {status}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn owner_only_rejects_an_inherited_extended_acl_before_writing() {
        let root = tempfile::tempdir().unwrap();
        let status = Command::new("/bin/chmod")
            .arg("+a")
            .arg("everyone allow read,file_inherit")
            .arg(root.path())
            .status()
            .unwrap();
        assert!(
            status.success(),
            "could not configure ACL fixture: {status}"
        );
        let path = root.path().join("secret");

        let error =
            tilo_files::native::atomic::replace(&path, b"secret", Privacy::OwnerOnly).unwrap_err();

        assert_eq!(error.state(), PublicationState::NotPublished);
        assert!(!path.exists());
        assert_no_stages(root.path());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn owner_only_read_rejects_an_inherited_extended_acl() {
        let root = tempfile::tempdir().unwrap();
        let status = Command::new("/bin/chmod")
            .arg("+a")
            .arg("everyone allow read,file_inherit")
            .arg(root.path())
            .status()
            .unwrap();
        assert!(
            status.success(),
            "could not configure ACL fixture: {status}"
        );
        let path = root.path().join("secret");
        fs::write(&path, b"").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let directory = StorageDirectory::open(root.path()).unwrap();

        assert!(
            directory
                .open_read(OsStr::new("secret"), Privacy::OwnerOnly)
                .is_err()
        );
    }

    #[cfg(feature = "test-support")]
    #[test]
    fn preflush_failure_preserves_old_target_and_removes_stage() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state");
        fs::write(&path, b"old").unwrap();

        let error = with_failure(Checkpoint::BeforeDataFlush, || {
            tilo_files::native::atomic::replace(&path, b"new", Privacy::Inherit)
        })
        .unwrap_err();

        assert_eq!(error.state(), PublicationState::NotPublished);
        assert_eq!(fs::read(&path).unwrap(), b"old");
        assert_no_stages(root.path());
    }

    #[cfg(feature = "test-support")]
    #[test]
    fn stage_is_owner_only_before_secret_bytes_are_written() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("secret");
        let mode = Arc::new(Mutex::new(None));
        let observed_mode = Arc::clone(&mode);

        with_observer(
            move |event| {
                if event.checkpoint == Checkpoint::StageCreated {
                    let metadata = &event.stage.as_ref().unwrap().metadata;
                    *observed_mode.lock().unwrap() = Some(metadata.permissions().mode() & 0o777);
                }
            },
            || tilo_files::native::atomic::replace(&path, b"secret", Privacy::OwnerOnly).unwrap(),
        );

        assert_eq!(*mode.lock().unwrap(), Some(0o600));
    }

    #[cfg(feature = "test-support")]
    #[test]
    fn legacy_io_error_preserves_publication_state_and_typed_source() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state");
        fs::write(&path, b"old").unwrap();

        let error = with_failure(Checkpoint::BeforeCommit, || {
            tilo_files::native::atomic::write(&path, b"new")
        })
        .unwrap_err();

        assert_eq!(
            publication_state(&error),
            Some(PublicationState::PublishedDurabilityUnknown)
        );
        assert!(
            error
                .get_ref()
                .and_then(|source| source.downcast_ref::<PublicationError>())
                .is_some()
        );
    }

    #[cfg(feature = "test-support")]
    #[test]
    fn dropping_post_publication_error_does_not_delete_new_target() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state");
        fs::write(&path, b"old").unwrap();

        let error = with_failure(Checkpoint::BeforeCommit, || {
            tilo_files::native::atomic::replace(&path, b"new", Privacy::Inherit)
        })
        .unwrap_err();
        drop(error);

        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert_no_stages(root.path());
    }

    #[cfg(feature = "test-support")]
    #[test]
    fn submitted_and_completed_publish_faults_are_ambiguous() {
        for checkpoint in [Checkpoint::PublishSubmitted, Checkpoint::AfterPublish] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("state");
            fs::write(&path, b"old").unwrap();

            let error = with_failure(checkpoint, || {
                tilo_files::native::atomic::replace(&path, b"new", Privacy::Inherit)
            })
            .unwrap_err();

            assert_eq!(error.state(), PublicationState::PublicationUnknown);
            assert!(error.retry_commit().is_err());
            if checkpoint == Checkpoint::AfterPublish {
                assert_eq!(fs::read(&path).unwrap(), b"new");
            } else {
                assert_eq!(fs::read(&path).unwrap(), b"old");
            }
        }
    }

    fn assert_no_stages(directory: &Path) {
        let stages = fs::read_dir(directory)
            .unwrap()
            .map(Result::unwrap)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(stages, 0, "publication left a staging file behind");
    }

    fn wait_for_child(child: &mut Child) -> Option<std::process::ExitStatus> {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return Some(status);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[cfg(any(unix, windows))]
mod identity_tests {
    use std::fs::{self, File, FileTimes};
    use tilo_files::native::atomic::{
        FileIdentity,
        lease::{FileLease, LeaseMode},
    };

    #[test]
    fn identity_tracks_objects_across_clones_links_renames_and_replacement() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("first");
        let first = File::create(&path).unwrap();
        let clone = first.try_clone().unwrap();
        let second = File::create(root.path().join("second")).unwrap();
        let times = FileTimes::new().set_modified(std::time::UNIX_EPOCH);
        first.set_times(times).unwrap();
        second.set_times(times).unwrap();
        let original = FileIdentity::of(&first).unwrap();
        assert_eq!(original, FileIdentity::of(&clone).unwrap());
        assert_ne!(original, FileIdentity::of(&second).unwrap());
        assert_eq!(
            original.volume(),
            FileIdentity::of(&second).unwrap().volume()
        );
        fs::hard_link(&path, root.path().join("alias")).unwrap();
        assert_eq!(
            original,
            FileIdentity::of(&File::open(root.path().join("alias")).unwrap()).unwrap()
        );
        fs::rename(&path, root.path().join("renamed")).unwrap();
        assert_eq!(original, FileIdentity::of(&first).unwrap());
        let replacement = File::create(&path).unwrap();
        assert_ne!(original, FileIdentity::of(&replacement).unwrap());
    }

    #[test]
    fn lease_identity_retains_the_leased_object_after_name_replacement() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("lease");
        let file = File::create(&path).unwrap();
        let identity = FileIdentity::of(&file).unwrap();
        let lease = FileLease::acquire(file, LeaseMode::Exclusive).unwrap();
        fs::rename(&path, root.path().join("old")).unwrap();
        let replacement = File::create(path).unwrap();
        assert_eq!(lease.identity().unwrap(), identity);
        assert_ne!(
            lease.identity().unwrap(),
            FileIdentity::of(&replacement).unwrap()
        );
    }
}

#[cfg(target_os = "macos")]
mod macos_acl_tests {
    use std::{ffi::OsStr, fs, os::unix::fs::PermissionsExt, process::Command};
    use tilo_files::native::atomic::{Privacy, StorageDirectory};

    #[test]
    fn held_directory_privacy_checks_native_acl() {
        let user = String::from_utf8(
            Command::new("/usr/bin/id")
                .arg("-un")
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        for (rule, accepted) in [
            (
                format!("user:{} allow read,write,file_inherit", user.trim()),
                true,
            ),
            (
                "everyone deny execute,file_inherit,only_inherit".into(),
                true,
            ),
            (
                "everyone allow read,file_inherit,only_inherit".into(),
                false,
            ),
            ("group:everyone allow read".into(), false),
        ] {
            let root = tempfile::tempdir().unwrap();
            fs::create_dir(root.path().join("child")).unwrap();
            assert!(
                Command::new("/bin/chmod")
                    .args(["+a", &rule])
                    .arg(root.path())
                    .status()
                    .unwrap()
                    .success()
            );
            fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
            let directory = StorageDirectory::open(root.path()).unwrap();
            let result = directory.validate_privacy(Privacy::OwnerOnly);
            if accepted {
                result.unwrap();
            } else {
                assert_eq!(
                    result.unwrap_err().kind(),
                    std::io::ErrorKind::PermissionDenied
                );
            }
        }
    }

    #[test]
    fn owner_only_accepts_benign_acl_for_private_creation_and_reads() {
        for rule in [
            format!(
                "user:{} allow read,write,file_inherit",
                String::from_utf8(
                    Command::new("/usr/bin/id")
                        .arg("-un")
                        .output()
                        .unwrap()
                        .stdout
                )
                .unwrap()
                .trim()
            ),
            "everyone deny execute,file_inherit,only_inherit".to_owned(),
        ] {
            let root = tempfile::tempdir().unwrap();
            assert!(
                Command::new("/bin/chmod")
                    .args(["+a", &rule])
                    .arg(root.path())
                    .status()
                    .unwrap()
                    .success()
            );
            let path = root.path().join("secret");
            tilo_files::native::atomic::replace(&path, b"secret", Privacy::OwnerOnly).unwrap();
            let directory = StorageDirectory::open(root.path()).unwrap();
            assert!(
                directory
                    .open_read(OsStr::new("secret"), Privacy::OwnerOnly)
                    .is_ok()
            );
            fs::write(root.path().join("existing"), b"existing").unwrap();
            fs::set_permissions(
                root.path().join("existing"),
                fs::Permissions::from_mode(0o600),
            )
            .unwrap();
            assert!(
                directory
                    .open_read(OsStr::new("existing"), Privacy::OwnerOnly)
                    .is_ok()
            );
        }
    }
}

#[cfg(windows)]
mod windows_publication_tests {
    #[cfg(feature = "test-support")]
    use std::sync::Mutex;
    use std::{
        ffi::OsStr,
        fs::{self, File},
        io::{self, Read},
        os::windows::{ffi::OsStringExt, fs::OpenOptionsExt},
        sync::{Arc, Barrier},
    };
    #[cfg(feature = "test-support")]
    use tilo_files::native::atomic::test_support::{Checkpoint, with_failure, with_observer};
    use tilo_files::native::atomic::{
        FileIdentity, Privacy, PublicationState, PublishMode, StorageDirectory,
    };

    #[test]
    fn replace_keeps_old_readers_and_publishes_complete_new_identity() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state");
        tilo_files::native::atomic::write(&path, b"old").unwrap();
        let mut old = File::open(&path).unwrap();
        let identity = FileIdentity::of(&old).unwrap();
        tilo_files::native::atomic::replace(&path, b"new", Privacy::Inherit).unwrap();
        let mut content = Vec::new();
        old.read_to_end(&mut content).unwrap();
        assert_eq!(content, b"old");
        assert_eq!(FileIdentity::of(&old).unwrap(), identity);
        assert_ne!(
            FileIdentity::of(&File::open(&path).unwrap()).unwrap(),
            identity
        );
        assert_eq!(fs::read(&path).unwrap(), b"new");
        tilo_files::native::atomic::replace(&path, b"", Privacy::Inherit).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn no_replace_has_one_complete_winner_and_losers_cannot_overwrite_it() {
        let root = tempfile::tempdir().unwrap();
        let barrier = Arc::new(Barrier::new(8));
        let mut writers = Vec::new();
        for byte in 0u8..8 {
            let path = root.path().join("state");
            let barrier = barrier.clone();
            writers.push(std::thread::spawn(move || {
                barrier.wait();
                match tilo_files::native::atomic::publish_if_absent(
                    &path,
                    &vec![byte; 4096],
                    Privacy::Inherit,
                ) {
                    Ok(()) => Some(byte),
                    Err(error) => {
                        assert_eq!(error.state(), PublicationState::NotPublished);
                        assert_eq!(error.io_error().kind(), io::ErrorKind::AlreadyExists);
                        assert!(error.retry_commit().is_err());
                        None
                    }
                }
            }));
        }
        let winners: Vec<_> = writers
            .into_iter()
            .filter_map(|w| w.join().unwrap())
            .collect();
        assert_eq!(winners.len(), 1);
        assert_eq!(
            fs::read(root.path().join("state")).unwrap(),
            vec![winners[0]; 4096]
        );
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn held_parent_and_raw_utf16_target_are_preserved_through_publication() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("parent");
        fs::create_dir(&parent).unwrap();
        let directory = StorageDirectory::open(&parent).unwrap();
        let id = directory.identity().unwrap();
        fs::rename(&parent, root.path().join("moved")).unwrap();
        fs::create_dir(&parent).unwrap();
        let name = std::ffi::OsString::from_wide(&[0x78, 0xd800]);
        directory
            .publish(&name, b"held", PublishMode::Replace, Privacy::OwnerOnly)
            .unwrap();
        assert_eq!(directory.identity().unwrap(), id);
        let mut content = Vec::new();
        directory
            .open_read(&name, Privacy::OwnerOnly)
            .unwrap()
            .read_to_end(&mut content)
            .unwrap();
        assert_eq!(content, b"held");
        assert_eq!(fs::read_dir(parent).unwrap().count(), 0);
    }

    #[test]
    fn legacy_modes_map_only_0600_to_private_publication_before_mutation() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("secret");
        tilo_files::native::atomic::write_with_mode(&path, b"secret", 0o600).unwrap();
        let directory = StorageDirectory::open(root.path()).unwrap();
        directory
            .open_read(OsStr::new("secret"), Privacy::OwnerOnly)
            .unwrap();
        let error = tilo_files::native::atomic::write_with_mode(&path, b"lost", 0o640).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert_eq!(
            tilo_files::native::atomic::publication_state(&error),
            Some(PublicationState::NotPublished)
        );
        assert_eq!(fs::read(&path).unwrap(), b"secret");
        let missing = root.path().join("missing/state");
        assert!(
            tilo_files::native::atomic::write_creating_parent_with_mode(&missing, b"lost", 0o400)
                .is_err()
        );
        assert!(!root.path().join("missing").exists());
    }

    /// Requires a writable nonlocal or non-NTFS fixture. Missing evidence fails
    /// qualification; an NTFS-only machine does not prove unsupported refusal.
    #[test]
    fn unsupported_native_volume_is_refused_before_target_or_stage_effects() {
        let fixture = std::env::var_os("TILO_ATOMIC_UNSUPPORTED_VOLUME_ROOT")
            .expect("qualification requires a writable remote or non-NTFS volume root");
        let root = tempfile::tempdir_in(fixture).unwrap();
        let path = root.path().join("state");
        fs::write(&path, b"old").unwrap();
        let error =
            tilo_files::native::atomic::replace(&path, b"new", Privacy::Inherit).unwrap_err();
        assert_eq!(error.state(), PublicationState::NotPublished);
        assert_eq!(error.io_error().kind(), io::ErrorKind::Unsupported);
        assert_eq!(fs::read(&path).unwrap(), b"old");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn native_read_sharing_conflict_keeps_its_os_error() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state");
        fs::write(&path, b"old").unwrap();
        let _held = File::options()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        let directory = StorageDirectory::open(root.path()).unwrap();
        let error = directory
            .open_read(OsStr::new("state"), Privacy::Inherit)
            .unwrap_err();
        assert_eq!(
            error.raw_os_error(),
            Some(windows_sys::Win32::Foundation::ERROR_SHARING_VIOLATION as i32)
        );
    }

    #[test]
    fn readonly_target_refusal_preserves_target_and_native_error() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state");
        fs::write(&path, b"old").unwrap();
        let permissions = fs::metadata(&path).unwrap().permissions();
        let mut readonly = permissions.clone();
        readonly.set_readonly(true);
        fs::set_permissions(&path, readonly).unwrap();
        let error =
            tilo_files::native::atomic::replace(&path, b"new", Privacy::Inherit).unwrap_err();
        fs::set_permissions(&path, permissions).unwrap();
        assert_eq!(error.state(), PublicationState::PublicationUnknown);
        assert!(error.io_error().raw_os_error().is_some());
        assert!(error.retry_commit().is_err());
        assert_eq!(fs::read(path).unwrap(), b"old");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 2);
    }

    #[cfg(feature = "test-support")]
    #[test]
    fn checkpoints_prove_file_flush_rename_flush_order_and_same_object() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state");
        let events = Arc::new(Mutex::new(Vec::new()));
        let seen = events.clone();
        with_observer(
            move |event| {
                let stage = event.stage.as_ref().unwrap();
                if event.checkpoint == Checkpoint::StageCreated {
                    assert_eq!(stage.metadata.len(), 0);
                }
                seen.lock()
                    .unwrap()
                    .push((event.checkpoint, stage.identity.clone()));
            },
            || tilo_files::native::atomic::write_with_mode(&path, b"secret", 0o600).unwrap(),
        );
        let events = events.lock().unwrap();
        assert_eq!(
            events.iter().map(|e| e.0).collect::<Vec<_>>(),
            vec![
                Checkpoint::StageCreated,
                Checkpoint::BeforeDataFlush,
                Checkpoint::DataFlushed,
                Checkpoint::BeforePublish,
                Checkpoint::PublishSubmitted,
                Checkpoint::AfterPublish,
                Checkpoint::BeforeCommit,
                Checkpoint::Committed,
            ]
        );
        let published = FileIdentity::of(&File::open(&path).unwrap()).unwrap();
        assert!(events.iter().all(|e| e.1 == published));
    }

    #[cfg(feature = "test-support")]
    #[test]
    fn pre_submission_failures_remove_only_the_owned_stage() {
        for checkpoint in [
            Checkpoint::StageCreated,
            Checkpoint::BeforeDataFlush,
            Checkpoint::BeforePublish,
        ] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("state");
            fs::write(&path, b"old").unwrap();
            let error = with_failure(checkpoint, || {
                tilo_files::native::atomic::replace(&path, b"new", Privacy::Inherit)
            })
            .unwrap_err();
            assert_eq!(error.state(), PublicationState::NotPublished);
            assert_eq!(fs::read(&path).unwrap(), b"old");
            assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
        }
    }

    #[cfg(feature = "test-support")]
    #[test]
    fn ambiguous_submission_never_deletes_or_retries_the_object() {
        for checkpoint in [Checkpoint::PublishSubmitted, Checkpoint::AfterPublish] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("state");
            fs::write(&path, b"old").unwrap();
            let error = with_failure(checkpoint, || {
                tilo_files::native::atomic::replace(&path, b"new", Privacy::Inherit)
            })
            .unwrap_err();
            assert_eq!(error.state(), PublicationState::PublicationUnknown);
            drop(error.retry_commit().unwrap_err());
            if checkpoint == Checkpoint::AfterPublish {
                assert_eq!(fs::read(&path).unwrap(), b"new");
                assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
            } else {
                assert_eq!(fs::read(&path).unwrap(), b"old");
                assert_eq!(fs::read_dir(root.path()).unwrap().count(), 2);
            }
        }
    }

    #[cfg(feature = "test-support")]
    #[test]
    fn retry_flushes_retained_published_object_without_reissuing_rename() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state");
        let error = with_failure(Checkpoint::BeforeCommit, || {
            tilo_files::native::atomic::publish_if_absent(&path, b"first", Privacy::Inherit)
        })
        .unwrap_err();
        assert_eq!(error.state(), PublicationState::PublishedDurabilityUnknown);
        let first = File::open(&path).unwrap();
        tilo_files::native::atomic::replace(&path, b"later", Privacy::Inherit).unwrap();
        error.retry_commit().unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"later");
        assert_ne!(
            FileIdentity::of(&first).unwrap(),
            FileIdentity::of(&File::open(path).unwrap()).unwrap()
        );
    }

    #[cfg(feature = "test-support")]
    #[test]
    fn legacy_postpublication_failure_preserves_phase_and_drop_preserves_target() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state");
        let error = with_failure(Checkpoint::BeforeCommit, || {
            tilo_files::native::atomic::write(&path, b"published")
        })
        .unwrap_err();
        assert_eq!(
            tilo_files::native::atomic::publication_state(&error),
            Some(PublicationState::PublishedDurabilityUnknown)
        );
        drop(error);
        assert_eq!(fs::read(path).unwrap(), b"published");
    }
    #[test]
    fn publication_rejects_invalid_components_and_accepts_a_full_length_name() {
        let root = tempfile::tempdir().unwrap();
        let directory = StorageDirectory::open(root.path()).unwrap();
        for invalid in ["", ".", "..", "a/b", "a\\b", "a:b", "a\0b"] {
            let error = directory
                .publish(
                    OsStr::new(invalid),
                    b"never",
                    PublishMode::Replace,
                    Privacy::Inherit,
                )
                .unwrap_err();
            assert_eq!(error.state(), PublicationState::NotPublished);
            assert_eq!(error.io_error().kind(), io::ErrorKind::InvalidInput);
        }
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
        let full = "x".repeat(255);
        directory
            .publish(
                OsStr::new(&full),
                b"full",
                PublishMode::Replace,
                Privacy::Inherit,
            )
            .unwrap();
        let mut contents = Vec::new();
        directory
            .open_read(OsStr::new(&full), Privacy::Inherit)
            .unwrap()
            .read_to_end(&mut contents)
            .unwrap();
        assert_eq!(contents, b"full");
    }

    #[cfg(feature = "test-support")]
    #[test]
    fn prepublication_cleanup_refuses_to_delete_a_rebound_stage_name() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state");
        fs::write(&path, b"old").unwrap();
        let observed = root.path().to_owned();
        let original_id = Arc::new(Mutex::new(None));
        let id = original_id.clone();
        let error = with_observer(
            move |event| {
                if event.checkpoint == Checkpoint::StageCreated {
                    *id.lock().unwrap() = Some(event.stage.as_ref().unwrap().identity.clone());
                    let stage = fs::read_dir(&observed)
                        .unwrap()
                        .map(Result::unwrap)
                        .find(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
                        .unwrap();
                    fs::rename(stage.path(), observed.join("displaced")).unwrap();
                    fs::write(stage.path(), b"other owner").unwrap();
                }
            },
            || {
                with_failure(Checkpoint::BeforeDataFlush, || {
                    tilo_files::native::atomic::replace(&path, b"new", Privacy::Inherit)
                })
            },
        )
        .unwrap_err();
        assert_eq!(error.state(), PublicationState::NotPublished);
        assert_eq!(fs::read(&path).unwrap(), b"old");
        assert_eq!(fs::read(root.path().join("displaced")).unwrap(), b"new");
        assert_eq!(
            FileIdentity::of(&File::open(root.path().join("displaced")).unwrap()).unwrap(),
            original_id.lock().unwrap().clone().unwrap()
        );
        let rebound = fs::read_dir(root.path())
            .unwrap()
            .map(Result::unwrap)
            .find(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .unwrap();
        assert_eq!(fs::read(rebound.path()).unwrap(), b"other owner");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 3);
    }

    #[cfg(feature = "test-support")]
    #[test]
    fn repeated_commit_failures_retain_retry_authority_without_namespace_replay() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state");
        let error = with_failure(Checkpoint::BeforeCommit, || {
            tilo_files::native::atomic::replace(&path, b"new", Privacy::OwnerOnly)
        })
        .unwrap_err();
        let identity = FileIdentity::of(&File::open(&path).unwrap()).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let events = seen.clone();
        let error = with_observer(
            move |e| events.lock().unwrap().push(e.checkpoint),
            || with_failure(Checkpoint::BeforeCommit, || error.retry_commit()),
        )
        .unwrap_err();
        assert_eq!(error.state(), PublicationState::PublishedDurabilityUnknown);
        assert_eq!(*seen.lock().unwrap(), vec![Checkpoint::BeforeCommit]);
        error.retry_commit().unwrap();
        assert_eq!(
            FileIdentity::of(&File::open(&path).unwrap()).unwrap(),
            identity
        );
        assert_eq!(fs::read(path).unwrap(), b"new");
    }

    #[test]
    fn independent_readers_never_observe_absence_or_partial_replacement() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state");
        tilo_files::native::atomic::write(&path, &vec![1; 16384]).unwrap();
        let done = Arc::new(AtomicBool::new(false));
        let stop = done.clone();
        let started = Arc::new(Barrier::new(2));
        let reader_started = started.clone();
        let reader_path = path.clone();
        let reader = std::thread::spawn(move || {
            let mut observations = 0;
            loop {
                let bytes = fs::read(&reader_path).unwrap();
                assert_eq!(bytes.len(), 16384);
                assert!(bytes.iter().all(|byte| *byte == bytes[0]));
                assert!(matches!(bytes[0], 1 | 2));
                observations += 1;
                if observations == 1 {
                    reader_started.wait();
                }
                if stop.load(Ordering::Acquire) {
                    return observations;
                }
            }
        });
        started.wait();
        let result = (0..32)
            .try_for_each(|i| tilo_files::native::atomic::write(&path, &vec![1 + i % 2; 16384]));
        done.store(true, Ordering::Release);
        let observations = reader.join().unwrap();
        result.unwrap();
        assert!(observations > 1);
    }
}

#[cfg(all(unix, feature = "test-support"))]
#[test]
fn retained_publication_retry_observes_real_commit_failure() {
    use tilo_files::native::atomic::test_support::{Checkpoint, with_failure};
    use tilo_files::native::atomic::{Privacy, PublicationState, replace};
    let root = tempfile::tempdir().unwrap();
    with_failure(Checkpoint::BeforeCommit, || {
        let error = replace(&root.path().join("retry"), b"value", Privacy::Inherit).unwrap_err();
        let error = error
            .retry_commit()
            .expect_err("persistent native commit fault must also affect retry");
        assert_eq!(error.state(), PublicationState::PublishedDurabilityUnknown);
    });
}
