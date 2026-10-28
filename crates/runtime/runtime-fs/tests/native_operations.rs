#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::too_many_lines,
    reason = "assertions on bounded disposable filesystem fixtures"
)]
use runtime_fs::operations::{ConflictPolicy, FsJob, FsOperation, FsUpdate, ItemState};
use runtime_fs::recovery::{OperationOwner, OperationState};
use runtime_fs::{
    DirectoryGrant, EntryName, FsError, FsOwner, FsRight, FsSession, GrantedEntry,
    NativeGrantAuthority,
};
use std::{fs, sync::Arc, time::Instant};
use tempfile::TempDir;

fn name(value: &str) -> EntryName {
    EntryName::new(value.into()).unwrap()
}
fn owner() -> FsOwner {
    FsOwner {
        workspace: "test-room".into(),
        app_id: "so.tilo.file-explorer".into(),
    }
}
fn rights() -> Vec<FsRight> {
    vec![
        FsRight::Enumerate,
        FsRight::Read,
        FsRight::Create,
        FsRight::Rename,
        FsRight::Copy,
        FsRight::Move,
        FsRight::Trash,
    ]
}
struct Fixture {
    authority: NativeGrantAuthority,
    session: FsSession,
    source: DirectoryGrant,
    destination: DirectoryGrant,
    store: Arc<OperationOwner>,
    // These roots outlive all descriptor/worker owners by declaration order.
    disk: TempDir,
    metadata: TempDir,
}
impl Fixture {
    fn new(attach: bool) -> Self {
        Self::with_rights(attach, rights())
    }
    fn with_rights(attach: bool, granted_rights: Vec<FsRight>) -> Self {
        let disk = tempfile::tempdir().unwrap();
        let metadata = tempfile::Builder::new()
            .permissions(<fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700))
            .tempdir()
            .unwrap();
        fs::create_dir(disk.path().join("source")).unwrap();
        fs::create_dir(disk.path().join("destination")).unwrap();
        let authority = NativeGrantAuthority::new();
        let source = authority
            .admit_directory(
                fs::File::open(disk.path().join("source")).unwrap(),
                owner(),
                granted_rights.clone(),
            )
            .unwrap();
        let destination = authority
            .admit_directory(
                fs::File::open(disk.path().join("destination")).unwrap(),
                owner(),
                granted_rights,
            )
            .unwrap();
        let session = authority
            .bind(
                owner(),
                uuid::Uuid::from_u128(0xa11),
                &[source.clone(), destination.clone()],
                &[],
            )
            .unwrap();
        let store =
            OperationOwner::open(fs::File::open(metadata.path()).unwrap(), owner()).unwrap();
        if attach {
            authority.attach_operations(Arc::clone(&store)).unwrap();
        }
        Self {
            authority,
            session,
            source,
            destination,
            store,
            disk,
            metadata,
        }
    }
    fn src(&self, leaf: &str) -> std::path::PathBuf {
        self.disk.path().join("source").join(leaf)
    }
    fn dst(&self, leaf: &str) -> std::path::PathBuf {
        self.disk.path().join("destination").join(leaf)
    }
    fn entry(&self, leaf: &str) -> GrantedEntry {
        self.session.lookup(&self.source, &name(leaf)).unwrap()
    }
    fn run(&self, operation: FsOperation) -> Vec<FsUpdate> {
        let mut job = self.session.start(operation).unwrap();
        let id = job.operation_id();
        let updates = drain(&mut job);
        let report = self.store.report(&id).unwrap();
        assert_ne!(report.state, OperationState::Running);
        let items: Vec<_> = updates
            .iter()
            .filter_map(|u| {
                if let FsUpdate::Item(item) = u {
                    Some(item)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(report.items.len(), items.len());
        for (recorded, live) in report.items.iter().zip(&items) {
            assert_eq!(recorded.state, live.state);
            assert_eq!(recorded.recoveries, live.recoveries);
        }
        let (completed, cancelled) = (
            items
                .iter()
                .filter(|i| i.state == ItemState::Completed)
                .count(),
            items
                .iter()
                .filter(|i| i.state == ItemState::Cancelled)
                .count(),
        );
        assert!(
            matches!(updates.last(), Some(FsUpdate::Finished { completed: c, failed: f, cancelled: x }) if *c == completed && *x == cancelled && *f == items.len()-completed-cancelled)
        );
        assert!(
            !fs::read_dir(self.metadata.path())
                .unwrap()
                .collect::<Vec<_>>()
                .is_empty()
        );
        updates
    }
}
fn drain(job: &mut FsJob) -> Vec<FsUpdate> {
    let deadline = tilo_test_env::hang_deadline();
    let mut all = Vec::new();
    loop {
        all.extend(job.poll().unwrap());
        if matches!(all.last(), Some(FsUpdate::Finished { .. })) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "terminal outcome deadline; fixture retains roots during worker teardown"
        );
        std::thread::yield_now();
    }
    let mut sequence = 0;
    for update in &all {
        if let FsUpdate::Progress { sequence: next, .. } = update {
            assert!(*next > sequence);
            sequence = *next;
        }
    }
    all
}
fn first(updates: &[FsUpdate]) -> &runtime_fs::operations::ItemOutcome {
    updates
        .iter()
        .find_map(|u| {
            if let FsUpdate::Item(i) = u {
                Some(i)
            } else {
                None
            }
        })
        .unwrap()
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn copies_and_moves_preserve_bytes_links_and_conflicts() {
    // phase: rename_conflict_preserves_both_bytes
    {
        let f = Fixture::new(true);
        fs::write(f.src("original"), b"source").unwrap();
        fs::write(f.src("taken"), b"destination").unwrap();
        let u = f.run(FsOperation::Rename {
            entry: f.entry("original"),
            name: name("taken"),
        });
        assert!(matches!(first(&u).error, Some(FsError::Conflict)));
        assert!(first(&u).destination.is_none());
        assert_eq!(fs::read(f.src("original")).unwrap(), b"source");
        assert_eq!(fs::read(f.src("taken")).unwrap(), b"destination");
    }
    // phase: copy_tree_keeps_source
    {
        let destination = tempfile::tempdir().unwrap();
        let f = Fixture::new(true);
        fs::create_dir_all(f.src("tree/empty")).unwrap();
        fs::create_dir(f.src("tree/nested")).unwrap();
        let bytes = vec![73; 2 * 1024 * 1024 + 19];
        fs::write(f.src("tree/nested/large"), &bytes).unwrap();
        let target = f
            .authority
            .admit_directory(
                fs::File::open(destination.path()).unwrap(),
                owner(),
                rights(),
            )
            .unwrap();
        let session = f
            .authority
            .bind(
                owner(),
                uuid::Uuid::from_u128(0xb22),
                &[f.source.clone(), target.clone()],
                &[],
            )
            .unwrap();
        let mut job = session
            .start(FsOperation::Copy {
                entries: vec![f.entry("tree")],
                into: target,
                conflict: ConflictPolicy::Fail,
            })
            .unwrap();
        let u = drain(&mut job);
        assert_eq!(
            f.store.report(&job.operation_id()).unwrap().items[0].state,
            ItemState::Completed
        );
        assert_eq!(first(&u).state, ItemState::Completed);
        assert!(first(&u).destination.is_some());
        for path in [
            f.src("tree/nested/large"),
            destination.path().join("tree/nested/large"),
        ] {
            assert_eq!(fs::read(path).unwrap(), bytes);
        }
        assert!(f.src("tree/empty").is_dir());
        assert!(destination.path().join("tree/empty").is_dir());
        assert!(u.iter().any(
            |u| matches!(u, FsUpdate::Progress { phase:"copy", bytes:b, .. } if *b>1024*1024)
        ));
    }
    // phase: copy_preserves_dangling_link
    {
        let f = Fixture::new(true);
        std::os::unix::fs::symlink("missing-target", f.src("dangling")).unwrap();
        let u = f.run(FsOperation::Copy {
            entries: vec![f.entry("dangling")],
            into: f.destination.clone(),
            conflict: ConflictPolicy::Fail,
        });
        assert_eq!(first(&u).state, ItemState::Completed);
        for p in [f.src("dangling"), f.dst("dangling")] {
            assert_eq!(
                fs::read_link(&p).unwrap(),
                std::path::Path::new("missing-target")
            );
            assert!(!p.exists());
        }
    }
    // phase: move_same_directory_is_not_copy
    {
        use std::os::unix::fs::MetadataExt;
        let f = Fixture::new(true);
        fs::write(f.src("a"), b"same").unwrap();
        let inode = fs::metadata(f.src("a")).unwrap().ino();
        let u = f.run(FsOperation::Move {
            entries: vec![f.entry("a")],
            into: f.source.clone(),
            conflict: ConflictPolicy::KeepBoth,
        });
        assert_eq!(first(&u).state, ItemState::Failed);
        assert_eq!(fs::metadata(f.src("a")).unwrap().ino(), inode);
        assert_eq!(fs::read(f.src("a")).unwrap(), b"same");
        assert_eq!(fs::read_dir(f.src("")).unwrap().count(), 1);
        assert!(
            !u.iter()
                .any(|u| matches!(u, FsUpdate::Progress { phase: "copy", .. }))
        );
    }
    // phase: recursive_destination_rejected_before_creation
    {
        let f = Fixture::new(true);
        fs::create_dir_all(f.src("tree/child")).unwrap();
        fs::write(f.src("tree/file"), b"unchanged").unwrap();
        let tree = f.entry("tree");
        let GrantedEntry::Directory(ref dir) = tree else {
            panic!()
        };
        let GrantedEntry::Directory(child) = f.session.lookup(dir, &name("child")).unwrap() else {
            panic!()
        };
        let u = f.run(FsOperation::Copy {
            entries: vec![tree.clone()],
            into: child,
            conflict: ConflictPolicy::Fail,
        });
        assert_eq!(first(&u).state, ItemState::Failed);
        std::os::unix::fs::symlink(f.src("tree/child"), f.dst("alias")).unwrap();
        let alias = f
            .authority
            .admit_directory(fs::File::open(f.dst("alias")).unwrap(), owner(), rights())
            .unwrap();
        let session = f
            .authority
            .bind(
                owner(),
                uuid::Uuid::from_u128(0xb22),
                &[f.source.clone(), alias.clone()],
                &[],
            )
            .unwrap();
        let mut job = session
            .start(FsOperation::Copy {
                entries: vec![tree],
                into: alias,
                conflict: ConflictPolicy::Fail,
            })
            .unwrap();
        assert_eq!(first(&drain(&mut job)).state, ItemState::Failed);
        assert_eq!(fs::read(f.src("tree/file")).unwrap(), b"unchanged");
        assert_eq!(fs::read_dir(f.src("tree/child")).unwrap().count(), 0);
    }
    // phase: keep_both_uses_exclusive_native_candidates
    {
        let f = Fixture::new(true);
        for n in ["report.txt", "report 2.txt"] {
            fs::write(f.dst(n), b"old").unwrap();
        }
        std::os::unix::fs::symlink("absent", f.dst("report 3.txt")).unwrap();
        let u = f.run(FsOperation::CreateFile {
            parent: f.destination.clone(),
            name: name("report.txt"),
            bytes: b"new".to_vec(),
            conflict: ConflictPolicy::KeepBoth,
        });
        assert_eq!(first(&u).state, ItemState::Completed);
        assert_eq!(fs::read(f.dst("report 4.txt")).unwrap(), b"new");
        let dest = first(&u).destination.as_ref().unwrap();
        assert_eq!(f.session.metadata(dest).unwrap().size, 3);
        for n in ["report.txt", "report 2.txt"] {
            assert_eq!(fs::read(f.dst(n)).unwrap(), b"old");
        }
        assert_eq!(
            fs::read_link(f.dst("report 3.txt")).unwrap(),
            std::path::Path::new("absent")
        );
    }
}
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn operations_publish_exact_bytes_and_durable_reports() {
    // phase: batch_reports_success_and_missing_item
    {
        let f = Fixture::new(true);
        fs::write(f.src("valid"), b"valid").unwrap();
        fs::write(f.src("missing"), b"missing").unwrap();
        let entries = vec![f.entry("valid"), f.entry("missing")];
        fs::remove_file(f.src("missing")).unwrap();
        let u = f.run(FsOperation::Copy {
            entries,
            into: f.destination.clone(),
            conflict: ConflictPolicy::Fail,
        });
        let states: Vec<_> = u
            .iter()
            .filter_map(|u| {
                if let FsUpdate::Item(i) = u {
                    Some(i.state)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(states, vec![ItemState::Completed, ItemState::Failed]);
        assert_eq!(fs::read(f.dst("valid")).unwrap(), b"valid");
        assert!(!f.dst("missing").exists());
    }
    // phase: missing_operation_store_prevents_effect
    {
        let f = Fixture::new(false);
        fs::write(f.src("a"), b"original").unwrap();
        assert!(matches!(
            f.session.start(FsOperation::Copy {
                entries: vec![f.entry("a")],
                into: f.destination.clone(),
                conflict: ConflictPolicy::Fail
            }),
            Err(FsError::StoreUnavailable)
        ));
        assert_eq!(fs::read(f.src("a")).unwrap(), b"original");
        assert_eq!(fs::read_dir(f.dst("")).unwrap().count(), 0);
    }
    // phase: create_directory_and_file_publish_exact_bytes
    {
        let f = Fixture::new(true);
        let u = f.run(FsOperation::CreateDirectory {
            parent: f.destination.clone(),
            name: name("folder"),
            conflict: ConflictPolicy::Fail,
        });
        assert_eq!(first(&u).state, ItemState::Completed);
        let bytes = vec![91; 1024 * 1024 + 31];
        let u = f.run(FsOperation::CreateFile {
            parent: f.destination.clone(),
            name: name("file"),
            bytes: bytes.clone(),
            conflict: ConflictPolicy::Fail,
        });
        assert_eq!(first(&u).state, ItemState::Completed);
        assert_eq!(fs::read(f.dst("file")).unwrap(), bytes);
        assert!(f.dst("folder").is_dir());
    }
    // phase: closed_widget_report_remains_queryable
    {
        let f = Fixture::new(true);
        fs::write(f.src("a"), vec![41; 2 * 1024 * 1024]).unwrap();
        let job = f
            .session
            .start(FsOperation::Copy {
                entries: vec![f.entry("a")],
                into: f.destination.clone(),
                conflict: ConflictPolicy::Fail,
            })
            .unwrap();
        let id = job.operation_id();
        f.session.close();
        drop(job);
        let deadline = tilo_test_env::hang_deadline();
        while f.store.report(&id).unwrap().state == OperationState::Running {
            assert!(
                Instant::now() < deadline,
                "terminal durability deadline; roots retained through teardown"
            );
            std::thread::yield_now();
        }
        let expected = f.store.report(&id).unwrap().state;
        let Fixture {
            authority,
            session,
            source,
            destination,
            store,
            disk,
            metadata,
        } = f;
        drop(authority);
        drop(session);
        drop(source);
        drop(destination);
        drop(store);
        let reopened =
            OperationOwner::open(fs::File::open(metadata.path()).unwrap(), owner()).unwrap();
        assert_eq!(reopened.report(&id).unwrap().state, expected);
        drop(reopened);
        drop(disk);
        drop(metadata);
    }
    // phase: distinct_owner_stores_are_selected_once
    {
        let f = Fixture::new(true);
        let other = FsOwner {
            workspace: "test-room".into(),
            app_id: "other".into(),
        };
        let meta = tempfile::Builder::new()
            .permissions(<fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700))
            .tempdir()
            .unwrap();
        let root = f
            .authority
            .admit_directory(fs::File::open(f.dst("")).unwrap(), other.clone(), rights())
            .unwrap();
        let session = f
            .authority
            .bind(
                other.clone(),
                uuid::Uuid::from_u128(0xb22),
                std::slice::from_ref(&root),
                &[],
            )
            .unwrap();
        assert!(matches!(
            session.start(FsOperation::CreateDirectory {
                parent: root.clone(),
                name: name("missing-store"),
                conflict: ConflictPolicy::Fail
            }),
            Err(FsError::StoreUnavailable)
        ));
        assert!(!f.dst("missing-store").exists());
        let store = OperationOwner::open(fs::File::open(meta.path()).unwrap(), other).unwrap();
        f.authority.attach_operations(store.clone()).unwrap();
        f.authority.attach_operations(store.clone()).unwrap();
        let replacement_meta = tempfile::Builder::new()
            .permissions(<fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700))
            .tempdir()
            .unwrap();
        let replacement =
            OperationOwner::open(fs::File::open(replacement_meta.path()).unwrap(), owner())
                .unwrap();
        assert!(matches!(
            f.authority.attach_operations(replacement),
            Err(FsError::Denied)
        ));
        let mut job = session
            .start(FsOperation::CreateDirectory {
                parent: root,
                name: name("other"),
                conflict: ConflictPolicy::Fail,
            })
            .unwrap();
        let id = job.operation_id();
        assert_eq!(first(&drain(&mut job)).state, ItemState::Completed);
        assert_eq!(store.report(&id).unwrap().owner.app_id, "other");
        assert!(matches!(f.store.report(&id), Err(FsError::NotFound)));
        drop(job);
        drop(session);
        drop(f);
        drop(store);
        drop(meta);
        drop(replacement_meta);
    }
}
#[test]
#[ignore = "requires two explicitly supplied disposable volumes"]
fn real_cross_volume_move_copy_remove_stages() {
    use std::os::unix::fs::MetadataExt;
    let source = tempfile::tempdir_in(
        std::env::var_os("RUNTIME_FS_TEST_SOURCE_VOLUME")
            .expect("explicit disposable source volume required"),
    )
    .unwrap();
    let destination = tempfile::tempdir_in(
        std::env::var_os("RUNTIME_FS_TEST_DESTINATION_VOLUME")
            .expect("explicit disposable destination volume required"),
    )
    .unwrap();
    assert_ne!(
        fs::metadata(source.path()).unwrap().dev(),
        fs::metadata(destination.path()).unwrap().dev(),
        "distinct device prerequisite"
    );
    let meta = tempfile::Builder::new()
        .permissions(<fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700))
        .tempdir()
        .unwrap();
    let authority = NativeGrantAuthority::new();
    let src = authority
        .admit_directory(fs::File::open(source.path()).unwrap(), owner(), rights())
        .unwrap();
    let dst = authority
        .admit_directory(
            fs::File::open(destination.path()).unwrap(),
            owner(),
            rights(),
        )
        .unwrap();
    let session = authority
        .bind(
            owner(),
            uuid::Uuid::from_u128(0xa11),
            &[src.clone(), dst.clone()],
            &[],
        )
        .unwrap();
    let store = OperationOwner::open(fs::File::open(meta.path()).unwrap(), owner()).unwrap();
    authority.attach_operations(store.clone()).unwrap();
    fs::write(source.path().join("a"), b"cross-volume").unwrap();
    let entry = session.lookup(&src, &name("a")).unwrap();
    let mut job = session
        .start(FsOperation::Move {
            entries: vec![entry],
            into: dst,
            conflict: ConflictPolicy::Fail,
        })
        .unwrap();
    let u = drain(&mut job);
    assert_eq!(first(&u).state, ItemState::Completed);
    assert_eq!(
        fs::read(destination.path().join("a")).unwrap(),
        b"cross-volume"
    );
    assert!(!source.path().join("a").exists());
    let phases: Vec<_> = u
        .iter()
        .filter_map(|u| {
            if let FsUpdate::Progress { phase, .. } = u {
                Some(*phase)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(phases, vec!["copy", "publish", "remove-source"]);
    assert_eq!(
        store.report(&job.operation_id()).unwrap().items[0].state,
        ItemState::Completed
    );
}

#[test]
fn mutations_require_their_exact_rights() {
    // phase: rename_requires_rename_without_create_or_read
    {
        let f = Fixture::with_rights(true, vec![FsRight::Enumerate, FsRight::Rename]);
        fs::write(f.src("original"), b"rename-only").unwrap();
        let u = f.run(FsOperation::Rename {
            entry: f.entry("original"),
            name: name("renamed"),
        });
        assert_eq!(first(&u).state, ItemState::Completed);
        assert!(first(&u).destination.is_some());
        assert!(!f.src("original").exists());
        assert_eq!(fs::read(f.src("renamed")).unwrap(), b"rename-only");
    }
    // phase: missing_read_and_isolated_file_do_not_authorize_mutations
    {
        let f = Fixture::with_rights(
            true,
            vec![
                FsRight::Enumerate,
                FsRight::Copy,
                FsRight::Create,
                FsRight::Rename,
            ],
        );
        fs::write(f.src("a"), b"preserve").unwrap();
        let u = f.run(FsOperation::Copy {
            entries: vec![f.entry("a")],
            into: f.destination.clone(),
            conflict: ConflictPolicy::Fail,
        });
        assert!(matches!(first(&u).error, Some(FsError::Denied)));
        assert_eq!(fs::read_dir(f.dst("")).unwrap().count(), 0);
        let isolated = f
            .authority
            .admit_file(fs::File::open(f.src("a")).unwrap(), owner(), rights())
            .unwrap();
        let session = f
            .authority
            .bind(
                owner(),
                uuid::Uuid::from_u128(0xb22),
                &[],
                std::slice::from_ref(&isolated),
            )
            .unwrap();
        let mut job = session
            .start(FsOperation::Rename {
                entry: GrantedEntry::File(isolated),
                name: name("sibling"),
            })
            .unwrap();
        assert!(matches!(
            first(&drain(&mut job)).error,
            Some(FsError::Denied)
        ));
        assert_eq!(fs::read(f.src("a")).unwrap(), b"preserve");
        assert!(!f.src("sibling").exists());
    }
}

#[test]
fn receipt_publication_rejects_public_directory_before_any_effect() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let receipt = root.path().join("untouched.receipt");
    fs::write(&receipt, b"invalid preexisting receipt must not be read").unwrap();
    let before = fs::metadata(&receipt).unwrap().modified().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o770)).unwrap();
    assert!(matches!(
        OperationOwner::open(fs::File::open(root.path()).unwrap(), owner()),
        Err(FsError::Denied)
    ));
    assert_eq!(
        fs::read(&receipt).unwrap(),
        b"invalid preexisting receipt must not be read"
    );
    assert_eq!(fs::metadata(&receipt).unwrap().modified().unwrap(), before);
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    assert_eq!(
        fs::metadata(root.path()).unwrap().permissions().mode() & 0o777,
        0o770
    );
}

#[cfg(target_os = "macos")]
#[test]
fn receipt_publication_rejects_directory_acl_before_any_effect() {
    use std::{os::unix::fs::PermissionsExt, process::Command};
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("untouched.receipt"), b"unchanged").unwrap();
    let before = fs::metadata(root.path().join("untouched.receipt"))
        .unwrap()
        .modified()
        .unwrap();
    assert!(
        Command::new("/bin/chmod")
            .args(["+a", "everyone allow read,write,file_inherit,only_inherit"])
            .arg(root.path())
            .status()
            .unwrap()
            .success()
    );
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(
        OperationOwner::open(fs::File::open(root.path()).unwrap(), owner()),
        Err(FsError::Denied)
    ));
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    assert_eq!(
        fs::read(root.path().join("untouched.receipt")).unwrap(),
        b"unchanged"
    );
    assert_eq!(
        fs::metadata(root.path().join("untouched.receipt"))
            .unwrap()
            .modified()
            .unwrap(),
        before
    );
}

#[test]
fn receipt_publication_retains_renamed_directory_and_writer_exclusion() {
    use std::os::unix::fs::PermissionsExt;
    for rename_before_open in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("journal");
        let moved = root.path().join("moved");
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let handle = fs::File::open(&path).unwrap();
        let rename = || {
            fs::rename(&path, &moved).unwrap();
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        };
        if rename_before_open {
            rename();
        }
        let store = OperationOwner::open(handle, owner()).unwrap();
        if !rename_before_open {
            rename();
        }
        assert!(matches!(
            OperationOwner::open(fs::File::open(&moved).unwrap(), owner()),
            Err(FsError::Busy)
        ));
        assert_eq!(fs::read_dir(&path).unwrap().count(), 0);
        let f = Fixture::new(false);
        f.authority.attach_operations(Arc::clone(&store)).unwrap();
        fs::write(f.src("a"), b"held authority").unwrap();
        let mut job = f
            .session
            .start(FsOperation::Copy {
                entries: vec![f.entry("a")],
                into: f.destination.clone(),
                conflict: ConflictPolicy::Fail,
            })
            .unwrap();
        drain(&mut job);
        assert!(fs::read_dir(&moved).unwrap().any(|e| {
            e.unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".receipt")
        }));
        assert_eq!(fs::read_dir(&path).unwrap().count(), 0);
        let _independent = OperationOwner::open(fs::File::open(&path).unwrap(), owner()).unwrap();
        assert_eq!(fs::read_dir(&path).unwrap().count(), 1);
    }
}
