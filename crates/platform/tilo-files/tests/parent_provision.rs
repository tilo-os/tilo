#![cfg(all(unix, feature = "test-support"))]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::exit,
    reason = "in a test these are the assertion, not a hazard; the restart child exits on purpose"
)]
use std::{fs, path::Path};
#[cfg(target_os = "linux")]
#[path = "support/posix_acl.rs"]
mod posix_acl;

#[test]
fn unresolved_parent_components_fail_before_creation() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("missing/../other/state");
    let error = tilo_files::native::atomic::write_creating_parent(&path, b"x").unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert!(!root.path().join("missing").exists());
    assert!(!root.path().join("other").exists());
}

#[test]
fn invalid_final_component_fails_before_parent_creation() {
    for exact_mode in [None, Some(0o600)] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("missing").join("bad\0name");
        assert_filename_rejected_without_effects(root.path(), &path, exact_mode);
    }
}

#[test]
fn missing_final_component_fails_before_parent_creation() {
    for exact_mode in [None, Some(0o600)] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("missing/..");
        assert!(path.file_name().is_none());
        assert_filename_rejected_without_effects(root.path(), &path, exact_mode);
    }
}

fn assert_filename_rejected_without_effects(root: &Path, path: &Path, exact_mode: Option<u32>) {
    let error = with_provision_observer(
        |_| panic!("invalid filename must not reach parent provisioning"),
        || {
            with_observer(
                |_| panic!("invalid filename must not reach file staging"),
                || match exact_mode {
                    None => tilo_files::native::atomic::write_creating_parent(path, b"x"),
                    Some(mode) => tilo_files::native::atomic::write_creating_parent_with_mode(
                        path, b"x", mode,
                    ),
                },
            )
        },
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(
        tilo_files::native::atomic::publication_state(&error),
        Some(tilo_files::native::atomic::PublicationState::NotPublished)
    );
    assert_eq!(directory_state(&error), None);
    let typed = error
        .into_inner()
        .unwrap()
        .downcast::<tilo_files::native::atomic::PublicationError>()
        .unwrap();
    assert_eq!(
        typed.state(),
        tilo_files::native::atomic::PublicationState::NotPublished
    );
    assert_eq!(typed.io_error().kind(), io::ErrorKind::InvalidInput);
    let source = std::error::Error::source(typed.as_ref())
        .unwrap()
        .downcast_ref::<io::Error>()
        .unwrap();
    assert_eq!(source.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(source.to_string(), typed.io_error().to_string());
    assert_eq!(fs::read_dir(root).unwrap().count(), 0);
    assert!(!root.join("missing").exists());
}

use std::{cell::RefCell, path::PathBuf, rc::Rc};
use tilo_files::native::atomic::test_support::{
    Checkpoint, ProvisionCheckpoint as P, with_observer, with_provision_failure,
    with_provision_observer,
};

fn physical(root: &Path) -> PathBuf {
    fs::canonicalize(root).unwrap()
}
fn chain(leaf: &Path) -> Vec<PathBuf> {
    leaf.ancestors().map(Path::to_owned).collect()
}

#[test]
fn wrappers_flush_entire_complete_tree_before_staging_on_every_call() {
    let root = tempfile::tempdir().unwrap();
    let leaf = physical(root.path()).join("profile/data");
    for explicit_mode in [None, Some(0o644)] {
        let visits = Rc::new(RefCell::new(Vec::new()));
        let observed = visits.clone();
        let complete = leaf.clone();
        let staged = visits.clone();
        let expected = chain(&leaf);
        let expected_stage = expected.clone();
        with_provision_observer(
            move |event| {
                if matches!(event.checkpoint, P::BeforeSync | P::Synced) {
                    assert!(
                        complete.is_dir(),
                        "whole suffix must precede the first real fsync"
                    );
                    assert!(event.identity.is_some());
                    observed
                        .borrow_mut()
                        .push((event.checkpoint, event.path.clone()));
                }
            },
            || {
                with_observer(
                    move |event| {
                        if event.checkpoint == Checkpoint::StageCreated {
                            assert_eq!(
                                *staged.borrow(),
                                expected_stage
                                    .iter()
                                    .flat_map(|p| [
                                        (P::BeforeSync, p.clone()),
                                        (P::Synced, p.clone())
                                    ])
                                    .collect::<Vec<_>>()
                            );
                        }
                    },
                    || match explicit_mode {
                        None => tilo_files::native::atomic::write_creating_parent(
                            &leaf.join("state"),
                            b"payload",
                        ),
                        Some(mode) => tilo_files::native::atomic::write_creating_parent_with_mode(
                            &leaf.join("state"),
                            b"payload",
                            mode,
                        ),
                    },
                )
            },
        )
        .unwrap();
        assert_eq!(
            visits
                .borrow()
                .iter()
                .filter(|(p, _)| *p == P::Synced)
                .map(|(_, path)| path.clone())
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(fs::read(leaf.join("state")).unwrap(), b"payload");
    }
}

#[test]
fn early_ancestor_failure_prevents_staging_and_fresh_call_replays() {
    let root = tempfile::tempdir().unwrap();
    let profile = physical(root.path()).join("profile");
    let path = profile.join("data/state");
    let error = with_provision_failure(P::BeforeSync, &profile, || {
        with_observer(
            |event| {
                assert_ne!(event.checkpoint, Checkpoint::StageCreated);
            },
            || tilo_files::native::atomic::write_creating_parent(&path, b"payload"),
        )
    })
    .unwrap_err();
    assert!(profile.join("data").is_dir());
    assert!(!path.exists());
    drop(error);
    let seen = Rc::new(RefCell::new(Vec::new()));
    let observed = seen.clone();
    with_provision_observer(
        move |e| {
            if e.checkpoint == P::Synced {
                observed.borrow_mut().push(e.path.clone());
            }
        },
        || tilo_files::native::atomic::write_creating_parent(&path, b"payload"),
    )
    .unwrap();
    assert_eq!(*seen.borrow(), chain(path.parent().unwrap()));
}

use std::{
    io,
    os::unix::fs::{PermissionsExt, symlink},
    process::Command,
};
use tilo_files::native::atomic::test_support::{MkdirFault, with_mkdir_result};
use tilo_files::native::atomic::{
    DirectoryState as D, Privacy, ProvisionError, StorageDirectory, directory_state,
    provision_directory,
};
fn error(result: Result<StorageDirectory, ProvisionError>) -> ProvisionError {
    result.err().expect("provision must fail")
}

#[test]
fn complete_retry_replays_every_barrier_and_returns_requested_leaf() {
    let root = tempfile::tempdir().unwrap();
    let profile = physical(root.path()).join("profile");
    let leaf = profile.join("data");
    let first = error(with_provision_failure(P::BeforeSync, &profile, || {
        provision_directory(&leaf, Privacy::Inherit)
    }));
    assert_eq!(first.state(), D::CreatedDurabilityUnknown);
    let second = error(with_provision_failure(P::BeforeSync, &profile, || {
        first.retry_commit()
    }));
    assert_eq!(second.state(), D::CreatedDurabilityUnknown);
    let visits = Rc::new(RefCell::new(Vec::new()));
    let observed = visits.clone();
    let held = with_provision_observer(
        move |e| {
            if e.checkpoint == P::Synced {
                observed.borrow_mut().push(e.path.clone());
            }
        },
        || second.retry_commit(),
    )
    .unwrap();
    assert_eq!(*visits.borrow(), chain(&leaf));
    assert_eq!(
        held.identity().unwrap(),
        StorageDirectory::open(&leaf).unwrap().identity().unwrap()
    );
    let existing = error(with_provision_failure(P::BeforeSync, &profile, || {
        provision_directory(&leaf, Privacy::Inherit)
    }));
    assert_eq!(existing.state(), D::Unchanged);
    assert!(existing.retry_commit().is_ok());
}

#[test]
fn partial_retry_cannot_create_remainder_or_return_prefix() {
    for checkpoint in [P::BeforeCreate, P::Created] {
        let root = tempfile::tempdir().unwrap();
        let profile = physical(root.path()).join("profile");
        let leaf = profile.join("data");
        let failure = error(with_provision_failure(checkpoint, &profile, || {
            provision_directory(&leaf, Privacy::Inherit)
        }));
        assert_eq!(
            failure.state(),
            if checkpoint == P::Created {
                D::CreatedDurabilityUnknown
            } else {
                D::Unchanged
            }
        );
        assert!(!leaf.exists());
        let state = failure.state();
        let same = with_provision_observer(
            |_| panic!("partial retry must perform no provisioning operation"),
            || error(failure.retry_commit()),
        );
        assert_eq!(same.state(), state);
        assert!(!leaf.exists());
        drop(same);
        provision_directory(&leaf, Privacy::Inherit).unwrap();
        assert!(leaf.is_dir());
    }
}

#[test]
fn presubmission_failure_preserves_earlier_known_creation() {
    let root = tempfile::tempdir().unwrap();
    let leaf = physical(root.path()).join("profile/data");
    let failure = error(with_provision_failure(P::BeforeCreate, &leaf, || {
        provision_directory(&leaf, Privacy::Inherit)
    }));
    assert_eq!(failure.state(), D::CreatedDurabilityUnknown);
    assert!(leaf.parent().unwrap().is_dir());
    assert!(!leaf.exists());
    assert!(failure.retry_commit().is_err());
    assert!(!leaf.exists());
}

#[test]
fn submitted_mkdir_failure_retains_unknown_effects_and_never_retries_or_cleans() {
    for after in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let uncertain = physical(root.path()).join("profile/uncertain");
        let leaf = uncertain.join("leaf");
        let fault = if after {
            MkdirFault::AfterSuccess(5)
        } else {
            MkdirFault::BeforeNative(5)
        };
        let failure = error(with_mkdir_result(&uncertain, fault, || {
            provision_directory(&leaf, Privacy::Inherit)
        }));
        assert_eq!(failure.state(), D::NamespaceUnknown);
        assert_eq!(failure.io_error().raw_os_error(), Some(5));
        assert_eq!(uncertain.exists(), after);
        assert!(!leaf.exists());
        let wrapped = failure.into_io_error();
        assert_eq!(directory_state(&wrapped), Some(D::NamespaceUnknown));
        let failure = *wrapped
            .into_inner()
            .unwrap()
            .downcast::<ProvisionError>()
            .unwrap();
        let failure = with_provision_observer(
            |_| panic!("unknown partial must not replay"),
            || error(failure.retry_commit()),
        );
        assert_eq!(failure.io_error().raw_os_error(), Some(5));
        drop(failure);
        assert_eq!(
            uncertain.exists(),
            after,
            "drop must not clean up unknown effects"
        );
        provision_directory(&leaf, Privacy::Inherit).unwrap();
        assert!(leaf.is_dir());
    }
}

#[test]
fn eexist_winner_is_admitted_and_flushed_but_never_assumed_private() {
    for private in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let leaf = physical(root.path()).join("winner");
        let winner = leaf.clone();
        let result = with_provision_observer(
            move |e| {
                if e.checkpoint == P::BeforeCreate && e.path == winner {
                    fs::create_dir(&winner).unwrap();
                    fs::set_permissions(&winner, fs::Permissions::from_mode(0o755)).unwrap();
                }
            },
            || {
                provision_directory(
                    &leaf,
                    if private {
                        Privacy::OwnerOnly
                    } else {
                        Privacy::Inherit
                    },
                )
            },
        );
        if private {
            let failure = error(result);
            assert_eq!(failure.state(), D::Unchanged);
            assert_eq!(failure.io_error().kind(), io::ErrorKind::PermissionDenied);
        } else {
            result.unwrap();
        }
        assert_eq!(
            fs::metadata(&leaf).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }
}

#[test]
fn replaced_ancestor_fails_without_publishing_or_recreating_on_retry() {
    let root = tempfile::tempdir().unwrap();
    let profile = physical(root.path()).join("profile");
    let leaf = profile.join("data");
    let moved = physical(root.path()).join("old-profile");
    let source = profile.clone();
    let destination = moved.clone();
    let observed_leaf = leaf.clone();
    let failure = error(with_provision_observer(
        move |e| {
            if e.checkpoint == P::BeforeSync && e.path == observed_leaf {
                fs::rename(&source, &destination).unwrap();
                fs::create_dir(&source).unwrap();
            }
        },
        || provision_directory(&leaf, Privacy::Inherit),
    ));
    assert_eq!(failure.state(), D::CreatedDurabilityUnknown);
    assert!(!leaf.exists());
    assert!(moved.join("data").is_dir());
    assert!(failure.retry_commit().is_err());
    assert!(!leaf.exists());
    assert_eq!(fs::read_dir(&profile).unwrap().count(), 0);
}

#[test]
fn wrapper_refuses_ancestor_replacement_before_any_file_stage() {
    let root = tempfile::tempdir().unwrap();
    let leaf = physical(root.path()).join("profile/data");
    let observed_leaf = leaf.clone();
    let profile = leaf.parent().unwrap().to_owned();
    let moved = physical(root.path()).join("moved");
    let result = with_provision_observer(
        move |e| {
            if e.checkpoint == P::BeforeSync && e.path == observed_leaf {
                fs::rename(&profile, &moved).unwrap();
                fs::create_dir(&profile).unwrap();
            }
        },
        || {
            with_observer(
                |e| assert_ne!(e.checkpoint, Checkpoint::StageCreated),
                || tilo_files::native::atomic::write_creating_parent(&leaf.join("state"), b"x"),
            )
        },
    );
    assert!(result.is_err());
    assert!(!leaf.exists());
}

#[test]
fn admitted_alias_succeeds_but_retargeting_and_new_suffix_symlinks_fail() {
    let root = tempfile::tempdir().unwrap();
    let base = physical(root.path());
    let alias = base.join("alias");
    fs::create_dir(base.join("target")).unwrap();
    fs::create_dir(base.join("other")).unwrap();
    symlink(base.join("target"), &alias).unwrap();
    let leaf = alias.join("nested");
    provision_directory(&leaf, Privacy::Inherit).unwrap();
    let changed_alias = alias;
    let other = base.join("other");
    let physical_leaf = base.join("target/nested");
    let failure = error(with_provision_observer(
        move |e| {
            if e.checkpoint == P::BeforeSync && e.path == physical_leaf {
                fs::remove_file(&changed_alias).unwrap();
                symlink(&other, &changed_alias).unwrap();
            }
        },
        || provision_directory(&leaf, Privacy::Inherit),
    ));
    assert_eq!(failure.state(), D::Unchanged);
    assert!(failure.retry_commit().is_err());
    assert!(!base.join("other/nested").exists());
    let injected = base.join("injected");
    let selected = injected.clone();
    let target = base.join("other");
    let failure = error(with_provision_observer(
        move |e| {
            if e.checkpoint == P::BeforeCreate && e.path == selected {
                symlink(&target, &selected).unwrap();
            }
        },
        || provision_directory(&injected.join("data"), Privacy::Inherit),
    ));
    assert_eq!(failure.state(), D::Unchanged);
    assert!(!base.join("other/data").exists());
    assert!(
        fs::symlink_metadata(&injected)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn dangling_alias_and_nondirectory_prefix_fail_without_effects() {
    let root = tempfile::tempdir().unwrap();
    symlink(root.path().join("absent"), root.path().join("dangling")).unwrap();
    let failure = error(provision_directory(
        &root.path().join("dangling/child"),
        Privacy::Inherit,
    ));
    assert_eq!(failure.state(), D::Unchanged);
    assert!(!root.path().join("absent").exists());
    fs::write(root.path().join("file"), b"x").unwrap();
    let failure = error(provision_directory(
        &root.path().join("file/child"),
        Privacy::Inherit,
    ));
    assert_eq!(failure.state(), D::Unchanged);
    assert_eq!(failure.io_error().kind(), io::ErrorKind::NotADirectory);
}

#[test]
fn owner_only_creation_and_existing_leaf_do_not_restrict_traversal_ancestors() {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).unwrap();
    let leaf = root.path().join("private/leaf");
    provision_directory(&leaf, Privacy::OwnerOnly).unwrap();
    for path in [root.path().join("private"), leaf.clone()] {
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    provision_directory(&leaf, Privacy::OwnerOnly).unwrap();
    assert_eq!(
        fs::metadata(root.path()).unwrap().permissions().mode() & 0o777,
        0o755
    );
    let failure = error(provision_directory(root.path(), Privacy::OwnerOnly));
    assert_eq!(failure.state(), D::Unchanged);
    assert_eq!(failure.io_error().kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(
        fs::metadata(root.path()).unwrap().permissions().mode() & 0o777,
        0o755
    );
}

#[test]
fn retry_revalidates_created_directory_privacy() {
    let root = tempfile::tempdir().unwrap();
    let parent = physical(root.path()).join("private");
    let leaf = parent.join("leaf");
    let failure = error(with_provision_failure(P::BeforeSync, &leaf, || {
        provision_directory(&leaf, Privacy::OwnerOnly)
    }));
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
    let failure = error(failure.retry_commit());
    assert_eq!(failure.io_error().kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(
        fs::metadata(&parent).unwrap().permissions().mode() & 0o777,
        0o755
    );
}

#[test]
fn legacy_explicit_file_modes_preserve_inherited_directory_search_bits() {
    let root = tempfile::tempdir().unwrap();
    // Compare ordinary mkdir under the current umask; never mutate process umask.
    fs::create_dir(root.path().join("ordinary")).unwrap();
    let directory_mode = fs::metadata(root.path().join("ordinary"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    for mode in [0o600, 0o644] {
        let parent = root.path().join(format!("mode-{mode:o}"));
        let file = parent.join("state");
        tilo_files::native::atomic::write_creating_parent_with_mode(&file, b"x", mode).unwrap();
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            mode
        );
        assert_eq!(
            fs::metadata(&parent).unwrap().permissions().mode() & 0o777,
            directory_mode
        );
    }
}

#[test]
fn empty_root_and_existing_parent_traversal_are_admitted_correctly() {
    assert_eq!(
        error(provision_directory(Path::new(""), Privacy::Inherit))
            .io_error()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    provision_directory(Path::new("/"), Privacy::Inherit).unwrap();
    provision_directory(Path::new("."), Privacy::Inherit).unwrap();
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("existing")).unwrap();
    provision_directory(&root.path().join("existing/../new"), Privacy::Inherit).unwrap();
    assert!(root.path().join("new").is_dir());
}

#[test]
fn restart_child() {
    let Some(root) = std::env::var_os("ATOMIC_PROVISION_RESTART_FIXTURE") else {
        return;
    };
    let profile = PathBuf::from(root).join("profile");
    let file = profile.join("data/state");
    if std::env::var_os("ATOMIC_PROVISION_RESTART_PHASE").unwrap() == "fail" {
        let failure = with_provision_failure(P::BeforeSync, &profile, || {
            tilo_files::native::atomic::write_creating_parent(&file, b"payload")
        })
        .unwrap_err();
        assert_eq!(directory_state(&failure), Some(D::CreatedDurabilityUnknown));
        assert!(file.parent().unwrap().is_dir());
        assert!(!file.exists());
        // Exit without Rust destructors: the parent process will start a fresh
        // test process, with no retained error/handles. This is not power loss.
        std::process::exit(0);
    }
    let visits = Rc::new(RefCell::new(Vec::new()));
    let observed = visits.clone();
    with_provision_observer(
        move |e| {
            if e.checkpoint == P::Synced {
                observed.borrow_mut().push(e.path.clone());
            }
        },
        || tilo_files::native::atomic::write_creating_parent(&file, b"payload"),
    )
    .unwrap();
    assert_eq!(*visits.borrow(), chain(file.parent().unwrap()));
    assert_eq!(fs::read(file).unwrap(), b"payload");
}

#[test]
fn process_restart_replays_existing_ancestors_without_prior_authority() {
    let root = tempfile::tempdir().unwrap();
    for phase in ["fail", "retry"] {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "restart_child", "--nocapture"])
            .env("ATOMIC_PROVISION_RESTART_FIXTURE", physical(root.path()))
            .env("ATOMIC_PROVISION_RESTART_PHASE", phase)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(
        fs::read(root.path().join("profile/data/state")).unwrap(),
        b"payload"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn macos_creation_preflight_checks_inheritable_acl_before_mkdir() {
    let user = Command::new("/usr/bin/id").arg("-un").output().unwrap();
    assert!(user.status.success());
    let user = String::from_utf8(user.stdout).unwrap();
    for (rule, accepted) in [
        (
            "everyone allow read,directory_inherit,only_inherit".to_owned(),
            false,
        ),
        (
            format!("user:{} allow read,write,directory_inherit", user.trim()),
            true,
        ),
        (
            "everyone deny delete,directory_inherit,only_inherit".to_owned(),
            true,
        ),
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
        let leaf = root.path().join("private/leaf");
        let result = provision_directory(&leaf, Privacy::OwnerOnly);
        if accepted {
            result.unwrap();
            StorageDirectory::open(&leaf)
                .unwrap()
                .validate_privacy(Privacy::OwnerOnly)
                .unwrap();
        } else {
            let failure = error(result);
            assert_eq!(failure.state(), D::Unchanged);
            assert_eq!(failure.io_error().kind(), io::ErrorKind::PermissionDenied);
            assert!(
                !root.path().join("private").exists(),
                "inherited allow must be rejected before mkdir"
            );
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn linux_default_acl_creation_remains_mode_bounded() {
    let root = tempfile::tempdir().unwrap();
    posix_acl::modify(root.path(), "d:u::rwx,d:g::rwx,d:o::rwx");
    let leaf = root.path().join("private/leaf");
    provision_directory(&leaf, Privacy::OwnerOnly).unwrap();
    for path in [leaf.parent().unwrap(), leaf.as_path()] {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        StorageDirectory::open(path)
            .unwrap()
            .validate_privacy(Privacy::OwnerOnly)
            .unwrap();
    }
}

#[cfg(target_os = "macos")]
#[test]
fn native_tmp_alias_is_traversed_forward_and_fully_flushed() {
    assert!(
        fs::symlink_metadata("/tmp")
            .unwrap()
            .file_type()
            .is_symlink(),
        "macOS alias fixture must be observable"
    );
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let leaf = root.path().join("profile/data");
    let held = provision_directory(&leaf, Privacy::Inherit).unwrap();
    assert_eq!(
        held.identity().unwrap(),
        StorageDirectory::open(&physical(&leaf))
            .unwrap()
            .identity()
            .unwrap()
    );
}

#[test]
fn provision_hooks_restore_outer_scope_after_unwind() {
    let root = tempfile::tempdir().unwrap();
    let leaf = physical(root.path()).join("leaf");
    fs::create_dir(&leaf).unwrap();
    let visits = Rc::new(RefCell::new(0));
    let observed = visits.clone();
    with_provision_observer(
        move |_| *observed.borrow_mut() += 1,
        || {
            with_provision_failure(P::BeforeSync, &leaf, || {
                let nested = std::panic::catch_unwind(|| {
                    with_provision_observer(
                        |_| panic!("nested observer"),
                        || {
                            with_provision_failure(P::BeforeCreate, &leaf, || {
                                let _ = provision_directory(&leaf, Privacy::Inherit);
                            });
                        },
                    );
                });
                assert!(nested.is_err());
                let restored = error(provision_directory(&leaf, Privacy::Inherit));
                assert_eq!(restored.state(), D::Unchanged);
                assert_eq!(
                    *visits.borrow(),
                    1,
                    "outer observer runs before restored failure"
                );
            });
            provision_directory(&leaf, Privacy::Inherit).unwrap();
        },
    );
    let final_visits = *visits.borrow();
    provision_directory(&leaf, Privacy::Inherit).unwrap();
    assert_eq!(
        *visits.borrow(),
        final_visits,
        "outer observer restored on return"
    );
}
