//! One suite for every backend a target has: each case runs against a native
//! directory, a private memory tree and a shared memory location. The web
//! backend (OPFS) runs the same cases in a browser: `web/run.mjs`.

#![cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::future::Future;
use std::path::PathBuf;
use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::task::{Context, Poll, Wake, Waker};
use std::thread;
use std::time::Duration;

use tilo_files::{
    ChangeKind, Dir, Entry, Feature, FsError, Kind, Location, LockMode, VPath, VirtualRoot,
    blocking,
};

/// Polls `future` on this thread, parking until its waker fires.
fn block_on<F: Future>(future: F) -> F::Output {
    struct Unpark(thread::Thread);
    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(Unpark(thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut future = pin!(future);
    loop {
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(output) => return output,
            Poll::Pending => thread::park(),
        }
    }
}

fn p(path: &str) -> VPath {
    VPath::new(path).unwrap()
}

fn disk() -> (Dir, Option<tempfile::TempDir>) {
    let temp = tempfile::tempdir().unwrap();
    let dir = Dir::open(Location::Native(temp.path().to_path_buf())).unwrap();
    (dir, Some(temp))
}

fn memory() -> (Dir, Option<tempfile::TempDir>) {
    (Dir::memory(), None)
}

/// A fresh prefix of the process-wide memory tree, created like a first
/// run's app root.
fn memory_location() -> (Dir, Option<tempfile::TempDir>) {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let prefix = p(&format!(
        "conformance/{}",
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let dir = Dir::open(Location::Virtual(VirtualRoot::Memory, prefix)).unwrap();
    block_on(dir.create_dir_all(&VPath::root())).unwrap();
    (dir, None)
}

macro_rules! conformance {
    ($($case:ident),* $(,)?) => {
        mod on_disk {
            $(#[test] fn $case() {
                let (dir, _temp) = super::disk();
                super::block_on(super::$case(&dir));
            })*
        }
        mod in_memory {
            $(#[test] fn $case() {
                let (dir, _temp) = super::memory();
                super::block_on(super::$case(&dir));
            })*
        }
        mod at_a_memory_location {
            $(#[test] fn $case() {
                let (dir, _temp) = super::memory_location();
                super::block_on(super::$case(&dir));
            })*
        }
    };
}

conformance!(
    writes_replace_whole_contents_and_create_parents,
    kinds_in_the_way_are_reported_alike,
    creation_never_replaces,
    renames_never_replace,
    removal_takes_files_and_empty_directories,
    listings_are_sorted_with_kinds,
    metadata_reports_size_kind_and_time,
    the_root_is_not_a_file,
    locks_exclude_as_their_mode_says,
    an_async_lock_waits_for_release,
    blocking_calls_agree_with_async_ones,
    prefixes_read_only_the_start,
    appends_extend_or_create,
    cache_writes_replace_and_create_parents,
    trees_are_removed_whole,
    watches_report_changes_below_them,
);

async fn writes_replace_whole_contents_and_create_parents(dir: &Dir) {
    let file = p("notes/2026/a.md");
    assert_eq!(dir.read(&file).await, Err(FsError::NotFound));
    dir.write_atomic(&file, b"a long first draft")
        .await
        .unwrap();
    assert_eq!(dir.read(&file).await.unwrap(), b"a long first draft");
    dir.write_atomic(&file, b"short").await.unwrap();
    assert_eq!(dir.read(&file).await.unwrap(), b"short");
    dir.write_atomic(&file, b"").await.unwrap();
    assert_eq!(dir.read(&file).await.unwrap(), b"");
}

async fn kinds_in_the_way_are_reported_alike(dir: &Dir) {
    dir.create_dir_all(&p("folder")).await.unwrap();
    dir.create_dir_all(&p("folder")).await.unwrap();
    assert_eq!(dir.read(&p("folder")).await, Err(FsError::IsADirectory));
    assert_eq!(
        dir.write_atomic(&p("folder"), b"x").await,
        Err(FsError::IsADirectory)
    );

    dir.write_atomic(&p("file"), b"x").await.unwrap();
    assert_eq!(
        dir.write_atomic(&p("file/child"), b"y").await,
        Err(FsError::NotADirectory)
    );
    assert_eq!(
        dir.read(&p("file/child")).await,
        Err(FsError::NotADirectory)
    );
    assert_eq!(dir.list(&p("file")).await, Err(FsError::NotADirectory));
    assert_eq!(dir.read(&p("file")).await.unwrap(), b"x");
}

async fn creation_never_replaces(dir: &Dir) {
    let file = p("ids/broker");
    assert!(dir.create_new(&file, b"first").await.unwrap());
    assert!(!dir.create_new(&file, b"second").await.unwrap());
    assert_eq!(dir.read(&file).await.unwrap(), b"first");
    dir.create_dir_all(&p("ids/taken")).await.unwrap();
    assert!(!dir.create_new(&p("ids/taken"), b"x").await.unwrap());
    assert_eq!(
        dir.list(&p("ids")).await.unwrap(),
        vec![
            Entry {
                name: "broker".into(),
                kind: Kind::File
            },
            Entry {
                name: "taken".into(),
                kind: Kind::Dir
            },
        ],
        "no temporary file may remain"
    );
}

async fn renames_never_replace(dir: &Dir) {
    dir.write_atomic(&p("a"), b"a").await.unwrap();
    dir.write_atomic(&p("b"), b"b").await.unwrap();
    assert_eq!(
        dir.rename_noreplace(&p("a"), &p("b")).await,
        Err(FsError::AlreadyExists)
    );
    assert_eq!(dir.read(&p("a")).await.unwrap(), b"a");
    assert_eq!(dir.read(&p("b")).await.unwrap(), b"b");

    dir.rename_noreplace(&p("a"), &p("moved/a")).await.unwrap();
    assert_eq!(dir.read(&p("a")).await, Err(FsError::NotFound));
    assert_eq!(dir.read(&p("moved/a")).await.unwrap(), b"a");
    assert_eq!(
        dir.rename_noreplace(&p("a"), &p("c")).await,
        Err(FsError::NotFound)
    );

    dir.write_atomic(&p("tree/sub/leaf"), b"leaf")
        .await
        .unwrap();
    dir.rename_noreplace(&p("tree"), &p("grown")).await.unwrap();
    assert_eq!(dir.read(&p("grown/sub/leaf")).await.unwrap(), b"leaf");
    assert_eq!(dir.list(&p("tree")).await, Err(FsError::NotFound));
    assert!(matches!(
        dir.rename_noreplace(&p("grown"), &p("grown/inner")).await,
        Err(FsError::InvalidPath(_))
    ));
    assert_eq!(
        dir.rename_noreplace(&p("grown"), &p("grown")).await,
        Err(FsError::AlreadyExists)
    );
}

async fn removal_takes_files_and_empty_directories(dir: &Dir) {
    dir.write_atomic(&p("d/f"), b"x").await.unwrap();
    assert_eq!(dir.remove(&p("d")).await, Err(FsError::DirectoryNotEmpty));
    dir.remove(&p("d/f")).await.unwrap();
    assert_eq!(dir.remove(&p("d/f")).await, Err(FsError::NotFound));
    dir.remove(&p("d")).await.unwrap();
    assert_eq!(dir.list(&p("d")).await, Err(FsError::NotFound));
}

async fn listings_are_sorted_with_kinds(dir: &Dir) {
    for name in ["b", "a b", "c/d", "a"] {
        dir.write_atomic(&p(&format!("l/{name}")), b"")
            .await
            .unwrap();
    }
    let names: Vec<(String, Kind)> = dir
        .list(&p("l"))
        .await
        .unwrap()
        .into_iter()
        .map(|entry| (entry.name, entry.kind))
        .collect();
    assert_eq!(
        names,
        [
            ("a".to_owned(), Kind::File),
            ("a b".to_owned(), Kind::File),
            ("b".to_owned(), Kind::File),
            ("c".to_owned(), Kind::Dir),
        ]
    );
    assert!(
        dir.list(&VPath::root())
            .await
            .unwrap()
            .iter()
            .any(|entry| entry.name == "l" && entry.kind == Kind::Dir)
    );
    assert_eq!(dir.list(&p("missing")).await, Err(FsError::NotFound));
}

async fn metadata_reports_size_kind_and_time(dir: &Dir) {
    let before = tilo_clock::wall()
        .checked_sub(Duration::from_secs(5))
        .unwrap();
    dir.write_atomic(&p("m/file"), b"12345").await.unwrap();
    let file = dir.metadata(&p("m/file")).await.unwrap();
    assert_eq!((file.len, file.kind, file.symlink), (5, Kind::File, false));
    assert!(file.modified >= before, "{:?} < {before:?}", file.modified);
    let folder = dir.metadata(&p("m")).await.unwrap();
    assert_eq!(folder.kind, Kind::Dir);
    assert_eq!(dir.metadata(&p("m/none")).await, Err(FsError::NotFound));
}

async fn the_root_is_not_a_file(dir: &Dir) {
    let root = VPath::root();
    for result in [
        dir.write_atomic(&root, b"x").await,
        dir.create_new(&root, b"x").await.map(drop),
        dir.remove(&root).await,
        dir.rename_noreplace(&root, &p("x")).await,
        dir.lock(&root, LockMode::Exclusive).await.map(drop),
    ] {
        assert!(matches!(result, Err(FsError::InvalidPath(_))), "{result:?}");
    }
    assert_eq!(dir.metadata(&root).await.unwrap().kind, Kind::Dir);
    assert!(matches!(
        dir.trash(&p("x")).await,
        Err(FsError::Unsupported {
            feature: Feature::Trash,
            ..
        })
    ));
}

async fn locks_exclude_as_their_mode_says(dir: &Dir) {
    let path = p("locks/owner.lock");
    let exclusive = dir.lock(&path, LockMode::Exclusive).await.unwrap();
    assert_eq!(exclusive.mode(), LockMode::Exclusive);
    assert_eq!(dir.read(&path).await.unwrap(), b"", "the lock file exists");
    let other = dir.clone();
    assert!(
        blocking::try_lock(&other, &path, LockMode::Shared)
            .unwrap()
            .is_none()
    );
    drop(exclusive);

    let first = dir.lock(&path, LockMode::Shared).await.unwrap();
    let second = blocking::try_lock(dir, &path, LockMode::Shared)
        .unwrap()
        .expect("shared holders coexist");
    assert!(
        blocking::try_lock(dir, &path, LockMode::Exclusive)
            .unwrap()
            .is_none()
    );
    drop((first, second));
    assert!(
        blocking::try_lock(dir, &path, LockMode::Exclusive)
            .unwrap()
            .is_some()
    );
}

async fn an_async_lock_waits_for_release(dir: &Dir) {
    let path = p("wait.lock");
    let held = dir.lock(&path, LockMode::Exclusive).await.unwrap();
    let acquired = Arc::new(AtomicBool::new(false));
    let waiter = {
        let (dir, path, acquired) = (dir.clone(), path.clone(), Arc::clone(&acquired));
        thread::spawn(move || {
            let lock = block_on(dir.lock(&path, LockMode::Exclusive)).unwrap();
            acquired.store(true, Ordering::SeqCst);
            drop(lock);
        })
    };
    thread::sleep(Duration::from_millis(100));
    assert!(!acquired.load(Ordering::SeqCst), "granted while held");
    drop(held);
    waiter.join().unwrap();
    assert!(acquired.load(Ordering::SeqCst));
}

async fn appends_extend_or_create(dir: &Dir) {
    blocking::append(dir, &p("dl/track.part"), b"first ").unwrap();
    blocking::append(dir, &p("dl/track.part"), b"second").unwrap();
    assert_eq!(
        dir.read(&p("dl/track.part")).await.unwrap(),
        b"first second"
    );
    dir.create_dir_all(&p("dl/folder")).await.unwrap();
    assert_eq!(
        blocking::append(dir, &p("dl/folder"), b"x"),
        Err(FsError::IsADirectory)
    );
}

async fn cache_writes_replace_and_create_parents(dir: &Dir) {
    blocking::write(dir, &p("tiles/3/4/5.png"), b"first").unwrap();
    blocking::write(dir, &p("tiles/3/4/5.png"), b"2nd").unwrap();
    assert_eq!(dir.read(&p("tiles/3/4/5.png")).await.unwrap(), b"2nd");
    assert_eq!(
        blocking::write(dir, &p("tiles/3"), b"x"),
        Err(FsError::IsADirectory)
    );
    assert!(matches!(
        blocking::write(dir, &VPath::root(), b"x"),
        Err(FsError::InvalidPath(_))
    ));
}

async fn trees_are_removed_whole(dir: &Dir) {
    dir.write_atomic(&p("cache/a/b/c.png"), b"c").await.unwrap();
    dir.write_atomic(&p("cache/a/d.png"), b"d").await.unwrap();
    dir.write_atomic(&p("kept.png"), b"k").await.unwrap();
    blocking::remove_all(dir, &p("cache")).unwrap();
    assert_eq!(dir.metadata(&p("cache")).await, Err(FsError::NotFound));
    blocking::remove_all(dir, &p("kept.png")).unwrap();
    assert_eq!(dir.metadata(&p("kept.png")).await, Err(FsError::NotFound));
    assert_eq!(
        blocking::remove_all(dir, &p("cache")),
        Err(FsError::NotFound)
    );
    assert!(matches!(
        blocking::remove_all(dir, &VPath::root()),
        Err(FsError::InvalidPath(_))
    ));
}

async fn prefixes_read_only_the_start(dir: &Dir) {
    dir.write_atomic(&p("media/clip.bin"), b"header+body")
        .await
        .unwrap();
    assert_eq!(
        blocking::read_prefix(dir, &p("media/clip.bin"), 6).unwrap(),
        b"header"
    );
    assert_eq!(
        blocking::read_prefix(dir, &p("media/clip.bin"), 64).unwrap(),
        b"header+body"
    );
    assert_eq!(
        blocking::read_prefix(dir, &p("media/none"), 4),
        Err(FsError::NotFound)
    );
    assert_eq!(
        blocking::read_bounded(dir, &p("media/clip.bin"), 11).unwrap(),
        Some(b"header+body".to_vec())
    );
    assert_eq!(
        blocking::read_bounded(dir, &p("media/clip.bin"), 10).unwrap(),
        None
    );
}

async fn blocking_calls_agree_with_async_ones(dir: &Dir) {
    blocking::write_atomic(dir, &p("s/a"), b"sync").unwrap();
    assert_eq!(dir.read(&p("s/a")).await.unwrap(), b"sync");
    assert!(!blocking::create_new(dir, &p("s/a"), b"x").unwrap());
    blocking::rename_noreplace(dir, &p("s/a"), &p("s/b")).unwrap();
    assert_eq!(blocking::read(dir, &p("s/b")).unwrap(), b"sync");
    assert_eq!(blocking::metadata(dir, &p("s/b")).unwrap().len, 4);
    assert_eq!(blocking::list(dir, &p("s")).unwrap().len(), 1);
    blocking::remove(dir, &p("s/b")).unwrap();
    blocking::create_dir_all(dir, &p("s/e")).unwrap();
    drop(blocking::lock(dir, &p("s/l"), LockMode::Exclusive).unwrap());
    assert_eq!(dir.list(&p("s")).await.unwrap().len(), 2);
}

/// Native watches need the `watch` feature; memory watches always work.
async fn watches_report_changes_below_them(dir: &Dir) {
    dir.create_dir_all(&p("w")).await.unwrap();
    let watch = match dir.watch(&p("w")) {
        Ok(watch) => watch,
        Err(FsError::Unsupported {
            feature: Feature::Watch,
            ..
        }) => return,
        Err(error) => panic!("{error:?}"),
    };
    dir.write_atomic(&p("outside"), b"x").await.unwrap();
    dir.write_atomic(&p("w/a.md"), b"one").await.unwrap();
    dir.write_atomic(&p("w/a.md"), b"two").await.unwrap();
    dir.remove(&p("w/a.md")).await.unwrap();
    let mut seen = Vec::new();
    for _ in 0..100 {
        while let Some(change) = watch.try_next() {
            seen.push(change);
        }
        if seen
            .iter()
            .any(|change| change.path == p("w/a.md") && change.kind == ChangeKind::Removed)
        {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert!(
        seen.iter()
            .all(|change| change.path.as_str().starts_with('w')),
        "{seen:?}"
    );
    assert_eq!(
        seen.iter()
            .filter(|change| change.path == p("w/a.md"))
            .map(|change| change.kind)
            .next_back(),
        Some(ChangeKind::Removed),
        "{seen:?}"
    );
}

#[test]
fn memory_locations_are_shared_and_prefixes_isolated() {
    let location = |prefix: &str| Location::Virtual(VirtualRoot::Memory, p(prefix));
    let one = Dir::open(location("shared/app")).unwrap();
    let same = Dir::open(location("shared/app")).unwrap();
    let parent = Dir::open(location("shared")).unwrap();
    let other = Dir::open(location("shared/other")).unwrap();
    block_on(one.write_atomic(&p("state.json"), b"{}")).unwrap();
    assert_eq!(block_on(same.read(&p("state.json"))).unwrap(), b"{}");
    assert_eq!(block_on(parent.read(&p("app/state.json"))).unwrap(), b"{}");
    assert_eq!(
        block_on(other.read(&p("state.json"))),
        Err(FsError::NotFound)
    );
    assert_eq!(
        block_on(Dir::memory().read(&p("state.json"))),
        Err(FsError::NotFound)
    );
}

#[test]
fn only_native_directories_have_paths() {
    let (dir, temp) = disk();
    let root = temp.as_ref().unwrap().path();
    assert_eq!(
        dir.native_path(&p("a/b")).unwrap(),
        root.join("a").join("b")
    );
    assert_eq!(
        Dir::memory()
            .native_path(&p("a"))
            .map_err(|error| error.feature),
        Err(Feature::NativePaths)
    );
}

#[test]
fn browser_storage_is_unsupported_natively() {
    let opfs = Location::Virtual(
        VirtualRoot::Opfs {
            profile: "default".into(),
        },
        p("so.tilo.notes/data"),
    );
    assert!(matches!(
        Dir::open(opfs),
        Err(FsError::Unsupported {
            feature: Feature::Files,
            ..
        })
    ));
}

#[test]
fn a_first_run_root_is_created_by_the_first_write() {
    let temp = tempfile::tempdir().unwrap();
    let root: PathBuf = temp.path().join("so.tilo.notes").join("data");
    let dir = Dir::open(Location::Native(root.clone())).unwrap();
    assert_eq!(block_on(dir.list(&VPath::root())), Err(FsError::NotFound));
    block_on(dir.write_atomic(&p("a"), b"x")).unwrap();
    assert_eq!(std::fs::read(root.join("a")).unwrap(), b"x");
}

#[cfg(unix)]
#[test]
fn native_metadata_reports_modes_and_links() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let (dir, temp) = disk();
    let root = temp.as_ref().unwrap().path();
    block_on(dir.write_atomic(&p("f"), b"abc")).unwrap();
    std::fs::set_permissions(root.join("f"), std::fs::Permissions::from_mode(0o640)).unwrap();
    symlink("f", root.join("link")).unwrap();
    symlink("missing", root.join("dangling")).unwrap();

    let file = block_on(dir.metadata(&p("f"))).unwrap();
    assert_eq!(file.mode, Some(0o640));
    let link = block_on(dir.metadata(&p("link"))).unwrap();
    assert_eq!((link.symlink, link.kind, link.len), (true, Kind::File, 3));
    let dangling = block_on(dir.metadata(&p("dangling"))).unwrap();
    assert_eq!((dangling.symlink, dangling.kind), (true, Kind::Other));
    assert_eq!(
        block_on(Dir::memory().metadata(&VPath::root())).map(|meta| meta.mode),
        Ok(None)
    );
}

#[cfg(unix)]
#[test]
fn removing_a_tree_keeps_what_its_links_point_to() {
    use std::os::unix::fs::symlink;
    let (dir, temp) = disk();
    let root = temp.as_ref().unwrap().path();
    block_on(dir.write_atomic(&p("outside/keep.png"), b"k")).unwrap();
    block_on(dir.write_atomic(&p("cache/tile.png"), b"t")).unwrap();
    symlink(root.join("outside"), root.join("cache/linked")).unwrap();
    blocking::remove_all(&dir, &p("cache")).unwrap();
    assert!(!root.join("cache").exists());
    assert_eq!(std::fs::read(root.join("outside/keep.png")).unwrap(), b"k");
}

#[cfg(unix)]
#[test]
fn private_writes_are_owner_only_natively_and_plain_elsewhere() {
    use std::os::unix::fs::PermissionsExt;
    let (dir, temp) = disk();
    let root = temp.as_ref().unwrap().path();
    blocking::write_private(&dir, &p("secret/key"), b"k").unwrap();
    let mode = |path: &str| {
        std::fs::metadata(root.join(path))
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!((mode("secret/key"), mode("secret")), (0o600, 0o700));
    assert_eq!(blocking::read(&dir, &p("secret/key")).unwrap(), b"k");
    let memory = Dir::memory();
    block_on(memory.write_private(&p("a/b"), b"m")).unwrap();
    assert_eq!(blocking::read(&memory, &p("a/b")).unwrap(), b"m");
}

#[cfg(unix)]
#[test]
fn saving_a_document_keeps_its_mode_and_link() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let (dir, temp) = disk();
    let root = temp.as_ref().unwrap().path();
    std::fs::write(root.join("tool.sh"), b"old").unwrap();
    std::fs::set_permissions(root.join("tool.sh"), std::fs::Permissions::from_mode(0o751)).unwrap();
    symlink("tool.sh", root.join("link")).unwrap();
    blocking::save_document(&dir, &p("link"), b"new").unwrap();
    assert_eq!(std::fs::read(root.join("tool.sh")).unwrap(), b"new");
    assert!(
        std::fs::symlink_metadata(root.join("link"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let mode = std::fs::metadata(root.join("tool.sh"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o751);
    blocking::save_document(&dir, &p("fresh.txt"), b"f").unwrap();
    assert_eq!(std::fs::read(root.join("fresh.txt")).unwrap(), b"f");
    assert_eq!(
        blocking::save_document(&dir, &p("gone/doc.txt"), b"x"),
        Err(FsError::NotFound)
    );
    assert!(!root.join("gone").exists());
}

#[test]
fn a_native_file_is_reached_through_its_directory() {
    let (_, temp) = disk();
    let path = temp.as_ref().unwrap().path().join("report.csv");
    std::fs::write(&path, b"a,b").unwrap();
    let (dir, name) = tilo_files::file_at(&path).unwrap();
    assert_eq!(name, p("report.csv"));
    assert_eq!(blocking::read(&dir, &name).unwrap(), b"a,b");
    assert!(matches!(
        tilo_files::file_at(std::path::Path::new("/")),
        Err(FsError::InvalidPath(_))
    ));
    let (_, relative) = tilo_files::file_at(std::path::Path::new("notes.md")).unwrap();
    assert_eq!(relative, p("notes.md"));
}

#[test]
fn directory_handles_and_futures_cross_threads() {
    fn send_sync<T: Send + Sync>(_: &T) {}
    fn send<T: Send>(_: T) {}
    let dir = Dir::memory();
    send_sync(&dir);
    send(dir.read(&p("a")));
    send(dir.lock(&p("a"), LockMode::Shared));
}
