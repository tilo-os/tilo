use super::publish::{MAX_NUMBER, Names};
use super::*;
use std::fs;
use std::path::Path;

type TestHook<'a> = Box<dyn FnMut(&Path) -> std::io::Result<()> + 'a>;

#[derive(Default)]
struct TestTransferObserver<'a> {
    force_staging: bool,
    before_copy_source: Option<TestHook<'a>>,
    before_publish: Option<TestHook<'a>>,
    before_source_remove: Option<TestHook<'a>>,
    remove_source: Option<TestHook<'a>>,
    after_source_quarantine: Option<TestHook<'a>>,
    staged_source: Option<TestHook<'a>>,
    before_source_restore: Option<TestHook<'a>>,
}

impl TransferObserver for TestTransferObserver<'_> {
    fn before_source_restore(&mut self, source: &Path, _: &Path) -> std::io::Result<()> {
        self.before_source_restore
            .as_mut()
            .map_or(Ok(()), |hook| hook(source))
    }

    fn remove_source(&mut self, source: &Path) -> std::io::Result<()> {
        match &mut self.remove_source {
            Some(hook) => hook(source),
            None => remove_path(source),
        }
    }

    fn staged_source(&mut self, source: &Path) -> std::io::Result<()> {
        self.staged_source
            .as_mut()
            .map_or(Ok(()), |hook| hook(source))
    }

    fn after_source_quarantine(&mut self, source: &Path) -> std::io::Result<()> {
        self.after_source_quarantine
            .as_mut()
            .map_or(Ok(()), |hook| hook(source))
    }

    fn allow_direct_rename(&self) -> bool {
        !self.force_staging
    }

    fn before_copy_source(&mut self, source: &Path) -> std::io::Result<()> {
        self.before_copy_source
            .as_mut()
            .map_or(Ok(()), |hook| hook(source))
    }

    fn before_publish(&mut self, destination: &Path) -> std::io::Result<()> {
        self.before_publish
            .as_mut()
            .map_or(Ok(()), |hook| hook(destination))
    }

    fn before_source_remove(&mut self, source: &Path) -> std::io::Result<()> {
        self.before_source_remove
            .as_mut()
            .map_or(Ok(()), |hook| hook(source))
    }
}

#[test]
fn names_number_never_replacing() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    assert_eq!(
        free_name(root, std::ffi::OsStr::new("a.txt")).unwrap(),
        Some(root.join("a.txt"))
    );
    fs::write(root.join("a.txt"), b"x").unwrap();
    assert_eq!(
        free_name(root, std::ffi::OsStr::new("a.txt")).unwrap(),
        Some(root.join("a 2.txt"))
    );
    fs::write(root.join("a 2.txt"), b"x").unwrap();
    assert_eq!(
        free_name(root, std::ffi::OsStr::new("a.txt")).unwrap(),
        Some(root.join("a 3.txt"))
    );
    assert_eq!(
        Names::new(std::ffi::OsStr::new("report.txt")).for_suffix(2),
        "report 2.txt"
    );
    assert_eq!(
        Names::new(std::ffi::OsStr::new("folder")).for_suffix(2),
        "folder 2"
    );
}

#[cfg(unix)]
#[test]
fn clipboard_recursive_copy_refuses_replaced_destination_descendant() {
    for replaced_relative in ["payload/nested", "payload", ""] {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let destination = root.path().join("destination");
        let outside = root.path().join("outside");
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::write(source.join("nested/report.txt"), b"original").unwrap();
        fs::create_dir(&destination).unwrap();
        fs::create_dir(&outside).unwrap();
        let target = outside.clone();
        let receiving = destination.clone();
        let mut replaced = false;
        let mut observer = TestTransferObserver {
            force_staging: true,
            before_copy_source: Some(Box::new(|path| {
                if path.file_name() == Some(std::ffi::OsStr::new("report.txt")) {
                    let container = fs::read_dir(&receiving)?.next().unwrap()?.path();
                    let replaced_path = if replaced_relative.is_empty() {
                        container
                    } else {
                        container.join(replaced_relative)
                    };
                    fs::remove_dir_all(&replaced_path)?;
                    std::os::unix::fs::symlink(&target, &replaced_path)?;
                    replaced = true;
                }
                Ok(())
            })),
            ..Default::default()
        };
        let result = transfer(&source, &destination, false, &mut observer);
        drop(observer);
        assert!(
            replaced,
            "fault must exercise an existing destination descendant"
        );
        assert!(
            !outside.join("report.txt").exists(),
            "copy must never write through a replaced staging descendant"
        );
        assert!(
            result.is_err(),
            "replaced destination descendant must be refused"
        );
        assert_eq!(
            fs::read(source.join("nested/report.txt")).unwrap(),
            b"original"
        );
    }
}

#[test]
fn clipboard_cut_lost_restoration_authority_retains_complete_recovery() {
    for refused_at in ["quarantine", "delete", "publish"] {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("report.txt");
        let destination = root.path().join("destination");
        fs::write(&source, b"original").unwrap();
        fs::create_dir(&destination).unwrap();
        let staged = std::rc::Rc::new(std::cell::RefCell::new(None::<PathBuf>));
        let recorded = staged.clone();
        let mut observer = TestTransferObserver {
            force_staging: true,
            staged_source: Some(Box::new(move |path| {
                *recorded.borrow_mut() = Some(path.to_path_buf());
                Ok(())
            })),
            after_source_quarantine: Some(Box::new(|_| {
                if refused_at == "quarantine" {
                    Err(std::io::Error::other("grant revoked"))
                } else {
                    Ok(())
                }
            })),
            before_source_remove: Some(Box::new(|_| {
                if refused_at == "delete" {
                    Err(std::io::Error::other("grant revoked"))
                } else {
                    Ok(())
                }
            })),
            before_publish: Some(Box::new(|_| Err(std::io::Error::other("grant revoked")))),
            before_source_restore: Some(Box::new(|_| {
                Err(std::io::Error::other("no current source grant"))
            })),
            ..Default::default()
        };
        let error = transfer(&source, &destination, true, &mut observer)
            .unwrap_err()
            .to_string();
        assert!(
            !source.exists(),
            "lost authority must not be bypassed by restoration"
        );
        assert!(error.contains("no current source grant"));
        let recovery = staged.borrow().clone().expect("actual staging receipt");
        assert_eq!(fs::read(recovery).unwrap(), b"original");
        let quarantines: Vec<_> = fs::read_dir(root.path())
            .unwrap()
            .map(Result::unwrap)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".tilo-transfer-source-")
            })
            .collect();
        assert_eq!(quarantines.len(), usize::from(refused_at != "publish"));
        for entry in quarantines {
            assert_eq!(fs::read(entry.path()).unwrap(), b"original");
        }
    }
}

#[test]
fn clipboard_worker_unwind_after_cut_deletion_retains_complete_recovery() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("report.txt");
    let destination = root.path().join("destination");
    fs::write(&source, b"original").unwrap();
    fs::create_dir(&destination).unwrap();
    let staged = std::rc::Rc::new(std::cell::RefCell::new(None::<PathBuf>));
    let recorded = staged.clone();
    let mut observer = TestTransferObserver {
        force_staging: true,
        staged_source: Some(Box::new(move |path| {
            *recorded.borrow_mut() = Some(path.to_path_buf());
            Ok(())
        })),
        before_publish: Some(Box::new(|_| {
            panic!("injected worker failure after deletion")
        })),
        ..TestTransferObserver::default()
    };
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            transfer(&source, &destination, true, &mut observer)
        }))
        .is_err()
    );
    assert!(
        !source.exists(),
        "the fault exercised the post-deletion boundary"
    );
    let recovery = staged
        .borrow()
        .clone()
        .expect("owner received staging location");
    assert_eq!(fs::read(&recovery).unwrap(), b"original");
    assert_no_source_quarantine(root.path());
}

#[test]
fn clipboard_cancel_before_deletion_restores_exact_source_identity() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("draft.txt");
    let destination = root.path().join("destination");
    fs::write(&source, b"original").unwrap();
    fs::create_dir(&destination).unwrap();
    let metadata = fs::metadata(&source).unwrap();
    let mut observer = TestTransferObserver {
        force_staging: true,
        before_source_remove: Some(Box::new(|_| Err(std::io::Error::other("cancelled")))),
        ..Default::default()
    };
    let error = transfer(&source, &destination, true, &mut observer)
        .unwrap_err()
        .to_string();
    assert!(error.contains("cancelled"));
    assert_eq!(fs::read(&source).unwrap(), b"original");
    assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let current = fs::metadata(&source).unwrap();
        assert_eq!(
            (current.dev(), current.ino(), current.mode()),
            (metadata.dev(), metadata.ino(), metadata.mode())
        );
    }
    #[cfg(not(unix))]
    let _ = metadata;
    assert_no_source_quarantine(root.path());
}

#[test]
fn clipboard_receipt_refusal_keeps_cut_source_and_cleans_staging() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("draft.txt");
    let destination = root.path().join("destination");
    fs::write(&source, b"original").unwrap();
    fs::create_dir(&destination).unwrap();
    let mut observer = TestTransferObserver {
        force_staging: true,
        staged_source: Some(Box::new(|staged| {
            assert_eq!(fs::read(staged)?, b"original");
            Err(std::io::Error::other("receipt unavailable"))
        })),
        ..Default::default()
    };
    let error = transfer(&source, &destination, true, &mut observer)
        .unwrap_err()
        .to_string();
    assert!(error.contains("receipt unavailable"));
    assert_eq!(fs::read(&source).unwrap(), b"original");
    assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
    assert_no_source_quarantine(root.path());
}

#[test]
fn clipboard_quarantine_refusal_restores_original_without_publication() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("draft.txt");
    let destination = root.path().join("destination");
    fs::write(&source, b"original").unwrap();
    fs::create_dir(&destination).unwrap();
    let mut observer = TestTransferObserver {
        force_staging: true,
        after_source_quarantine: Some(Box::new(|quarantined| {
            assert_eq!(fs::read(quarantined)?, b"original");
            Err(std::io::Error::other("source grant revoked"))
        })),
        ..Default::default()
    };
    let error = transfer(&source, &destination, true, &mut observer)
        .unwrap_err()
        .to_string();
    assert!(error.contains("source grant revoked"));
    assert_eq!(fs::read(&source).unwrap(), b"original");
    assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
    assert_no_source_quarantine(root.path());
}

#[test]
fn clipboard_source_replacement_is_restored_without_deleting_either_file() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("draft.txt");
    let saved = root.path().join("saved.txt");
    let destination = root.path().join("destination");
    fs::write(&source, b"original").unwrap();
    fs::create_dir(&destination).unwrap();
    let replace = source.clone();
    let retain = saved.clone();
    let mut observer = TestTransferObserver {
        force_staging: true,
        staged_source: Some(Box::new(move |staged| {
            assert_eq!(fs::read(staged)?, b"original");
            fs::rename(&replace, &retain)?;
            fs::write(&replace, b"replacement")
        })),
        after_source_quarantine: Some(Box::new(|quarantined| {
            assert_eq!(fs::read(quarantined)?, b"replacement");
            Err(std::io::Error::other("selected source identity changed"))
        })),
        ..Default::default()
    };
    let error = transfer(&source, &destination, true, &mut observer)
        .unwrap_err()
        .to_string();
    assert!(error.contains("identity changed"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            fs::metadata(&source).unwrap().permissions().mode() & 0o777,
            fs::metadata(&saved).unwrap().permissions().mode() & 0o777,
            "refusing a replaced source must not privatize its permissions"
        );
    }
    assert_eq!(fs::read(&source).unwrap(), b"replacement");
    assert_eq!(fs::read(&saved).unwrap(), b"original");
    assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
    assert_no_source_quarantine(root.path());
}

fn assert_no_transfer_staging(directory: &Path) {
    assert!(
        fs::read_dir(directory).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".tilo-transfer-")
        }),
        "private transfer staging was cleaned"
    );
}

fn assert_no_source_quarantine(directory: &Path) {
    assert!(
        fs::read_dir(directory).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".tilo-transfer-source-")
        }),
        "source quarantine was cleaned"
    );
}

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

#[test]
fn copies_and_moves_never_overwrite_or_leave_staging() {
    {
        let d = tmp();
        let src = d.path().join("dir");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("inner.txt"), b"hello").unwrap();
        let destdir = d.path().join("dest");
        fs::create_dir(&destdir).unwrap();

        let copied = transfer(&src, &destdir, false, &mut ()).unwrap();
        assert!(copied.join("inner.txt").exists());
        assert_eq!(fs::read(copied.join("inner.txt")).unwrap(), b"hello");
        assert!(src.join("inner.txt").exists());
        let copied2 = transfer(&src, &destdir, false, &mut ()).unwrap();
        assert_eq!(copied2.file_name().unwrap().to_string_lossy(), "dir 2");
        assert_no_transfer_staging(&destdir);
    }

    {
        let d = tmp();
        let f = d.path().join("move-me.txt");
        fs::write(&f, b"x").unwrap();
        let destdir = d.path().join("dest");
        fs::create_dir(&destdir).unwrap();
        let moved = transfer(&f, &destdir, true, &mut ()).unwrap();
        assert!(moved.exists());
        assert!(!f.exists());
        assert_eq!(moved.parent().unwrap(), destdir);
        assert_no_transfer_staging(&destdir);
        assert_no_source_quarantine(d.path());
    }

    // --- exhausted names never overwrite and never lose the source ---
    {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("report.txt");
        let destination = directory.path().join("destination");
        fs::write(&source, b"selected bytes").unwrap();
        fs::create_dir(&destination).unwrap();
        let occupied = destination.join("report.txt");
        fs::write(&occupied, b"existing bytes").unwrap();
        for suffix in 2..MAX_NUMBER {
            fs::write(
                destination.join(format!("report {suffix}.txt")),
                b"suffix bytes",
            )
            .unwrap();
        }
        assert!(matches!(
            transfer(&source, &destination, false, &mut ()),
            Err(Failure::NamesExhausted { .. })
        ));
        assert_eq!(fs::read(&source).unwrap(), b"selected bytes");
        assert_eq!(fs::read(&occupied).unwrap(), b"existing bytes");
        assert!(matches!(
            transfer(&source, &destination, true, &mut ()),
            Err(Failure::NamesExhausted { .. })
        ));
        assert_eq!(fs::read(&source).unwrap(), b"selected bytes");
        assert_eq!(fs::read(&occupied).unwrap(), b"existing bytes");
        assert_eq!(fs::read_dir(&destination).unwrap().count(), 9_999);
        assert_no_transfer_staging(&destination);
    }

    // --- a folder refuses to go into itself ---
    {
        let d = tmp();
        let source = d.path().join("source");
        let nested_destination = source.join("nested");
        fs::create_dir_all(&nested_destination).unwrap();
        fs::write(source.join("file.txt"), b"x").unwrap();

        for remove_source in [false, true] {
            assert!(matches!(
                transfer(&source, &nested_destination, remove_source, &mut ()),
                Err(Failure::IntoItself)
            ));
        }
        assert!(
            source.join("file.txt").exists(),
            "refusal has no side effect"
        );
    }

    // --- a move into the folder it is already in is refused ---
    {
        let d = tmp();
        let root = d.path();
        let source = root.join("here.txt");
        fs::write(&source, b"x").unwrap();
        assert!(matches!(
            transfer(&source, root, true, &mut ()),
            Err(Failure::AlreadyHere)
        ));
        assert!(source.exists());
    }
}

/// Copies and moves publish atomically despite racing roots and descendants, clean staging on failure and refuse destinations inside the source.
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
)]
#[test]
fn copies_and_moves_publish_atomically_under_races() {
    {
        for remove_source in [false, true] {
            let directory = tmp();
            let source = directory.path().join("report.txt");
            let destination = directory.path().join("destination");
            fs::write(&source, b"selected bytes").unwrap();
            fs::create_dir(&destination).unwrap();

            let mut raced = false;
            let mut observer = TestTransferObserver {
                force_staging: true,
                before_publish: Some(Box::new(|candidate| {
                    if !raced {
                        raced = true;
                        fs::write(candidate, b"competitor bytes")?;
                    }
                    Ok(())
                })),
                ..Default::default()
            };
            let transferred =
                transfer(&source, &destination, remove_source, &mut observer).unwrap();
            drop(observer);

            assert!(raced, "the test forced the post-selection collision");
            assert_eq!(transferred, destination.join("report 2.txt"));
            assert_eq!(
                fs::read(destination.join("report.txt")).unwrap(),
                b"competitor bytes"
            );
            assert_eq!(fs::read(&transferred).unwrap(), b"selected bytes");
            assert_eq!(source.exists(), !remove_source);
            assert_no_transfer_staging(&destination);
        }
    }

    {
        let directory = tmp();
        let source = directory.path().join("report.txt");
        let destination = directory.path().join("destination");
        fs::write(&source, b"selected bytes").unwrap();
        fs::create_dir(&destination).unwrap();

        let mut raced = false;
        let mut observer = TestTransferObserver {
            before_publish: Some(Box::new(|candidate| {
                if !raced {
                    raced = true;
                    fs::write(candidate, b"competitor bytes")?;
                }
                Ok(())
            })),
            ..Default::default()
        };
        let moved = transfer(&source, &destination, true, &mut observer).unwrap();
        drop(observer);

        assert_eq!(moved, destination.join("report 2.txt"));
        assert_eq!(
            fs::read(destination.join("report.txt")).unwrap(),
            b"competitor bytes"
        );
        assert_eq!(fs::read(&moved).unwrap(), b"selected bytes");
        assert!(!source.exists());
    }

    {
        let directory = tmp();
        let source = directory.path().join("project");
        let destination = directory.path().join("destination");
        let outside = directory.path().join("outside.txt");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("selected.txt"), b"selected bytes").unwrap();
        fs::write(&outside, b"outside bytes").unwrap();
        fs::create_dir(&destination).unwrap();

        let mut raced = false;
        let mut observer = TestTransferObserver {
            force_staging: true,
            before_publish: Some(Box::new(|candidate| {
                if !raced {
                    raced = true;
                    fs::create_dir(candidate)?;
                    #[cfg(unix)]
                    std::os::unix::fs::symlink(&outside, candidate.join("selected.txt"))?;
                    #[cfg(not(unix))]
                    fs::write(candidate.join("selected.txt"), b"competitor bytes")?;
                }
                Ok(())
            })),
            ..Default::default()
        };
        let copied = transfer(&source, &destination, false, &mut observer).unwrap();
        drop(observer);

        assert_eq!(copied, destination.join("project 2"));
        #[cfg(unix)]
        assert_eq!(
            fs::read_link(destination.join("project/selected.txt")).unwrap(),
            outside
        );
        #[cfg(not(unix))]
        assert_eq!(
            fs::read(destination.join("project/selected.txt")).unwrap(),
            b"competitor bytes"
        );
        assert_eq!(
            fs::read(copied.join("selected.txt")).unwrap(),
            b"selected bytes"
        );
        assert_eq!(fs::read(&outside).unwrap(), b"outside bytes");
        assert_no_transfer_staging(&destination);
    }

    {
        let directory = tmp();
        let source = directory.path().join("project");
        let destination = directory.path().join("destination");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("first.txt"), b"first bytes").unwrap();
        fs::write(source.join("second.txt"), b"second bytes").unwrap();
        fs::create_dir(&destination).unwrap();

        let root = source.clone();
        let mut visited_child = false;
        let mut observer = TestTransferObserver {
            force_staging: true,
            before_copy_source: Some(Box::new(move |path| {
                if path != root {
                    if visited_child {
                        return Err(std::io::Error::other("injected recursive copy failure"));
                    }
                    visited_child = true;
                }
                Ok(())
            })),
            ..Default::default()
        };
        let result = transfer(&source, &destination, false, &mut observer);

        assert!(result.is_err());
        assert!(source.join("first.txt").exists());
        assert!(source.join("second.txt").exists());
        assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
        assert_no_transfer_staging(&destination);
    }

    {
        let directory = tmp();
        let source = directory.path().join("project");
        let destination = directory.path().join("destination");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("first.txt"), b"first bytes").unwrap();
        fs::write(source.join("second.txt"), b"second bytes").unwrap();
        fs::create_dir(&destination).unwrap();

        let mut observer = TestTransferObserver {
            force_staging: true,
            remove_source: Some(Box::new(|quarantine| {
                fs::remove_file(quarantine.join("first.txt"))?;
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "injected partial source deletion failure",
                ))
            })),
            ..Default::default()
        };
        let result = transfer(&source, &destination, true, &mut observer);

        assert!(result.is_err());
        assert_eq!(fs::read(source.join("first.txt")).unwrap(), b"first bytes");
        assert_eq!(
            fs::read(source.join("second.txt")).unwrap(),
            b"second bytes"
        );
        assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
        assert_no_transfer_staging(&destination);
        assert_no_source_quarantine(directory.path());

        let retried = transfer(&source, &destination, true, &mut ()).unwrap();
        assert_eq!(retried, destination.join("project"));
        assert_eq!(fs::read(retried.join("first.txt")).unwrap(), b"first bytes");
        assert_eq!(
            fs::read(retried.join("second.txt")).unwrap(),
            b"second bytes"
        );
        assert_eq!(fs::read_dir(&destination).unwrap().count(), 1);
    }
}

/// Symlink swaps are refused without traversal, symlinks and dangling entries are preserved, and metadata failures propagate.
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
)]
#[cfg(unix)]
#[test]
fn symlinks_and_metadata_failures_are_handled_without_following_links() {
    {
        use std::os::unix::fs::symlink;

        let directory = tmp();
        let source = directory.path().join("report.txt");
        let destination = directory.path().join("destination");
        let outside = directory.path().join("outside.txt");
        fs::write(&source, b"selected bytes").unwrap();
        fs::write(&outside, b"outside secret").unwrap();
        fs::create_dir(&destination).unwrap();

        let selected = source.clone();
        let target = outside.clone();
        let mut swapped = false;
        let mut observer = TestTransferObserver {
            force_staging: true,
            before_copy_source: Some(Box::new(move |path| {
                if path == selected && !swapped {
                    swapped = true;
                    fs::remove_file(path)?;
                    symlink(&target, path)?;
                }
                Ok(())
            })),
            ..Default::default()
        };
        let result = transfer(&source, &destination, false, &mut observer);

        assert!(result.is_err());
        assert_eq!(fs::read(&outside).unwrap(), b"outside secret");
        assert_eq!(fs::read_link(&source).unwrap(), outside);
        assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
        assert_no_transfer_staging(&destination);
    }

    {
        use std::os::unix::fs::symlink;

        let directory = tmp();
        let source = directory.path().join("project");
        let saved_source = directory.path().join("saved-project");
        let destination = directory.path().join("destination");
        let outside = directory.path().join("outside");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("secret.txt"), b"outside secret").unwrap();
        fs::create_dir(&destination).unwrap();

        let selected = source.clone();
        let saved = saved_source.clone();
        let target = outside.clone();
        let mut swapped = false;
        let mut observer = TestTransferObserver {
            force_staging: true,
            before_copy_source: Some(Box::new(move |path| {
                if path == selected && !swapped {
                    swapped = true;
                    fs::rename(path, &saved)?;
                    symlink(&target, path)?;
                }
                Ok(())
            })),
            ..Default::default()
        };
        let result = transfer(&source, &destination, false, &mut observer);

        assert!(result.is_err());
        assert_eq!(
            fs::read(outside.join("secret.txt")).unwrap(),
            b"outside secret"
        );
        assert!(saved_source.is_dir());
        assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
        assert_no_transfer_staging(&destination);
    }

    {
        use std::os::unix::fs::symlink;

        let d = tmp();
        let source = d.path().join("source");
        let destination = d.path().join("destination");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&destination).unwrap();
        // This deliberately points outside the selected source tree. A copy
        // must reproduce the link rather than dereferencing `/etc/passwd`.
        symlink("/etc/passwd", source.join("outside-link")).unwrap();
        symlink("missing-target", source.join("dangling-link")).unwrap();

        let copied = transfer(&source, &destination, false, &mut ()).unwrap();
        let outside = copied.join("outside-link");
        let dangling = copied.join("dangling-link");
        assert!(
            fs::symlink_metadata(&outside)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::read_link(&outside).unwrap(),
            PathBuf::from("/etc/passwd")
        );
        assert!(
            fs::symlink_metadata(&dangling)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::read_link(&dangling).unwrap(),
            PathBuf::from("missing-target")
        );
    }

    {
        use std::os::unix::fs::symlink;
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("report.txt");
        let destination = directory.path().join("destination");
        fs::write(&source, b"selected bytes").unwrap();
        symlink("destination", &destination).unwrap();
        for remove_source in [false, true] {
            match transfer(&source, &destination, remove_source, &mut ()) {
                Err(Failure::Inspect { error, .. }) => {
                    assert_ne!(error.kind(), std::io::ErrorKind::NotFound);
                }
                other => panic!("a destination that cannot be examined is refused: {other:?}"),
            }
            assert_eq!(fs::read(&source).unwrap(), b"selected bytes");
            assert_eq!(
                fs::read_link(&destination).unwrap(),
                PathBuf::from("destination")
            );
        }
    }

    {
        use std::os::unix::fs::symlink;

        for move_source in [false, true] {
            for numbered_collision in [false, true] {
                let directory = tempfile::tempdir().unwrap();
                let source = directory.path().join("report.txt");
                let destination = directory.path().join("destination");
                let outside = directory.path().join("unselected-target.txt");
                fs::write(&source, b"selected bytes").unwrap();
                fs::create_dir(&destination).unwrap();
                let occupied = destination.join("report.txt");
                let (link, expected) = if numbered_collision {
                    fs::write(&occupied, b"existing bytes").unwrap();
                    (
                        destination.join("report 2.txt"),
                        destination.join("report 3.txt"),
                    )
                } else {
                    (occupied.clone(), destination.join("report 2.txt"))
                };
                symlink(&outside, &link).unwrap();
                let result = transfer(&source, &destination, move_source, &mut ()).unwrap();
                assert_eq!(result, expected);
                assert_eq!(fs::read(&result).unwrap(), b"selected bytes");
                assert_eq!(fs::read_link(&link).unwrap(), outside);
                assert!(
                    !outside.exists(),
                    "copy must not write through the destination link"
                );
                assert_eq!(source.exists(), !move_source);
                if !move_source {
                    assert_eq!(fs::read(&source).unwrap(), b"selected bytes");
                }
                if numbered_collision {
                    assert_eq!(fs::read(&occupied).unwrap(), b"existing bytes");
                }
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn received_bytes_take_the_first_free_name_and_leave_no_staging() {
    let d = tmp();
    let destination = d.path().join("dest");
    fs::create_dir(&destination).unwrap();
    fs::write(destination.join("report.txt"), b"existing").unwrap();
    let receiving = std::fs::File::open(&destination).unwrap();

    let first = receive_bytes(
        &receiving,
        &destination,
        "report.txt".as_ref(),
        b"one",
        &mut (),
    )
    .unwrap();
    let second = receive_bytes(
        &receiving,
        &destination,
        "report.txt".as_ref(),
        b"two",
        &mut (),
    )
    .unwrap();
    assert_eq!(first, "report 2.txt");
    assert_eq!(second, "report 3.txt");
    assert_eq!(
        fs::read(destination.join("report.txt")).unwrap(),
        b"existing"
    );
    assert_eq!(fs::read(destination.join(first)).unwrap(), b"one");
    assert_eq!(fs::read(destination.join(second)).unwrap(), b"two");
    assert_no_transfer_staging(&destination);
}

#[cfg(unix)]
#[test]
fn a_receive_that_cannot_publish_leaves_nothing_under_a_real_name() {
    let d = tmp();
    let destination = d.path().join("dest");
    fs::create_dir(&destination).unwrap();
    let source = d.path().join("report.txt");
    fs::write(&source, b"received bytes").unwrap();
    let receiving = std::fs::File::open(&destination).unwrap();

    let mut refusing = TestTransferObserver {
        before_publish: Some(Box::new(|_| {
            Err(std::io::Error::other("the receive stopped before its name"))
        })),
        ..TestTransferObserver::default()
    };
    assert!(
        receive_file(
            &receiving,
            &destination,
            "report.txt".as_ref(),
            &source,
            &mut refusing
        )
        .is_err()
    );
    assert!(
        fs::read_dir(&destination).unwrap().next().is_none(),
        "a partly received item never appears under a real name"
    );
    assert_no_transfer_staging(&destination);

    assert!(
        receive_bytes(
            &receiving,
            &destination,
            "report.txt".as_ref(),
            b"received bytes",
            &mut refusing
        )
        .is_err()
    );
    assert!(fs::read_dir(&destination).unwrap().next().is_none());
    assert_no_transfer_staging(&destination);
    assert_eq!(fs::read(&source).unwrap(), b"received bytes");
}

#[cfg(unix)]
#[test]
fn a_received_folder_publishes_its_whole_staged_tree() {
    let d = tmp();
    let destination = d.path().join("dest");
    fs::create_dir(&destination).unwrap();
    let receiving = std::fs::File::open(&destination).unwrap();

    let mut staging = Staging::in_folder(&receiving, &destination).unwrap();
    staging
        .create_directory(Path::new("nested"))
        .expect("the staged tree takes folders");
    staging
        .create_file(Path::new("nested/report.txt"))
        .and_then(|mut file| std::io::Write::write_all(&mut file, b"received"))
        .unwrap();
    staging.sync().unwrap();
    let receiving_folder = receiving.try_clone().unwrap();
    let published = staging.publish_tree("folder".as_ref(), &mut ()).unwrap();
    assert_eq!(published, "folder");
    assert_eq!(
        fs::read(destination.join(published).join("nested/report.txt")).unwrap(),
        b"received"
    );
    assert_no_transfer_staging(&destination);

    let mut again = Staging::in_folder(&receiving_folder, &destination).unwrap();
    again.create_directory(Path::new("")).unwrap();
    again.sync().unwrap();
    assert_eq!(
        again.publish_tree("folder".as_ref(), &mut ()).unwrap(),
        "folder 2"
    );
    assert_no_transfer_staging(&destination);
    receiving_folder.sync_all().unwrap();
}

#[test]
fn bytes_received_through_a_path_number_like_a_copy() {
    let d = tmp();
    let destination = d.path().join("dest");
    fs::create_dir(&destination).unwrap();
    fs::write(destination.join("shot.png"), b"existing").unwrap();

    let first = transfer_bytes(b"one", &destination, "shot.png".as_ref(), &mut ()).unwrap();
    let second = transfer_bytes(b"two", &destination, "shot.png".as_ref(), &mut ()).unwrap();
    assert_eq!(first.file_name().unwrap(), "shot 2.png");
    assert_eq!(second.file_name().unwrap(), "shot 3.png");
    assert_eq!(fs::read(destination.join("shot.png")).unwrap(), b"existing");
    assert_eq!(fs::read(&first).unwrap(), b"one");
    assert_no_transfer_staging(&destination);
}
