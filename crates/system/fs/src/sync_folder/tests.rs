use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use collab_types::Ed25519PublicKey;
use fs_protocol::{FileActor, FileDocumentId};

use super::{Pass, SyncFolder};
use crate::fs::QuickFS;
use crate::hydration::Hydrator;
use crate::namespace::Namespace;
use crate::smart::Heuristics;
use crate::store::ContentStore;

struct Fixture {
    dir: tempfile::TempDir,
    fs: Arc<QuickFS>,
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = test_server::direct_endpoint().await;
        let hydrator = Arc::new(Hydrator::new(endpoint, Arc::new(ContentStore::memory())));
        let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
        let actor = FileActor {
            person: Ed25519PublicKey([1; 32]),
            device: Ed25519PublicKey([2; 32]),
        };
        let heuristics = Arc::new(Heuristics::new(Arc::clone(&hydrator)));
        let fs = QuickFS::new(namespace, hydrator, heuristics).with_actor(actor);
        Self {
            dir,
            fs: Arc::new(fs),
        }
    }

    fn folder_path(&self) -> std::path::PathBuf {
        self.dir.path().join("Tilo/Home")
    }

    fn folder(&self) -> SyncFolder {
        SyncFolder::open(
            Arc::clone(&self.fs),
            &self.folder_path(),
            &self.dir.path().join("state"),
            Box::new(|| Ok(())),
        )
        .unwrap()
    }

    fn local(&self, path: &str) -> std::path::PathBuf {
        self.folder_path().join(path)
    }

    fn write_remote(&self, path: &str, bytes: &[u8]) {
        let source = self.dir.path().join("remote-write");
        std::fs::write(&source, bytes).unwrap();
        self.fs.write_path(Path::new(path), &source, 1_000).unwrap();
    }

    fn remote(&self, path: &str) -> Option<Vec<u8>> {
        self.fs.read_path(Path::new(path)).ok()
    }

    fn record(&self, path: &str) -> FileDocumentId {
        self.fs.entry_at(Path::new(path)).unwrap().record.unwrap()
    }
}

fn pass(imported: usize, exported: usize, removed: usize, renamed: usize) -> Pass {
    Pass {
        imported,
        exported,
        removed,
        renamed,
        ..Pass::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_saved_in_the_folder_becomes_a_workspace_version() {
    let fx = Fixture::new().await;
    let mut folder = fx.folder();
    std::fs::create_dir_all(fx.local("notes")).unwrap();
    std::fs::write(fx.local("notes/plan.txt"), b"first").unwrap();

    assert_eq!(folder.reconcile().unwrap(), pass(2, 0, 0, 0));
    assert_eq!(fx.remote("notes/plan.txt").unwrap(), b"first");

    std::fs::write(fx.local("notes/plan.txt"), b"second draft").unwrap();
    assert_eq!(folder.reconcile().unwrap(), pass(1, 0, 0, 0));
    assert_eq!(fx.remote("notes/plan.txt").unwrap(), b"second draft");

    assert_eq!(folder.reconcile().unwrap(), Pass::default());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_file_reaches_the_folder_whole() {
    let fx = Fixture::new().await;
    let mut folder = fx.folder();
    let report = vec![7; 70_000];
    fx.write_remote("reports/q3.bin", &report);

    assert_eq!(folder.reconcile().unwrap(), pass(0, 2, 0, 0));
    assert_eq!(std::fs::read(fx.local("reports/q3.bin")).unwrap(), report);
    let names: Vec<_> = std::fs::read_dir(fx.local("reports"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert!(
        names
            .iter()
            .all(|name| !tilo_files::native::atomic::is_temporary_name(std::ffi::OsStr::new(name))),
        "{names:?}"
    );

    fx.write_remote("reports/q3.bin", b"revised");
    assert_eq!(folder.reconcile().unwrap(), pass(0, 1, 0, 0));
    assert_eq!(
        std::fs::read(fx.local("reports/q3.bin")).unwrap(),
        b"revised"
    );
    assert_eq!(folder.reconcile().unwrap(), Pass::default());
}

#[tokio::test(flavor = "multi_thread")]
async fn removals_cross_in_both_directions() {
    let fx = Fixture::new().await;
    let mut folder = fx.folder();
    std::fs::create_dir_all(fx.local("old")).unwrap();
    std::fs::write(fx.local("old/a.txt"), b"a").unwrap();
    std::fs::write(fx.local("b.txt"), b"b").unwrap();
    std::fs::write(fx.local("c.txt"), b"c").unwrap();
    folder.reconcile().unwrap();

    std::fs::remove_dir_all(fx.local("old")).unwrap();
    assert_eq!(folder.reconcile().unwrap(), pass(0, 0, 2, 0));
    assert!(fx.fs.entry_at(Path::new("old")).is_none());

    fx.fs.apply_local_remove(Path::new("b.txt")).unwrap();
    assert_eq!(folder.reconcile().unwrap(), pass(0, 0, 1, 0));
    assert!(!fx.local("b.txt").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_edit_wins_over_a_removal_on_the_other_side() {
    let fx = Fixture::new().await;
    let mut folder = fx.folder();
    std::fs::write(fx.local("kept.txt"), b"v1").unwrap();
    std::fs::write(fx.local("restored.txt"), b"v1").unwrap();
    folder.reconcile().unwrap();

    fx.fs.apply_local_remove(Path::new("kept.txt")).unwrap();
    std::fs::write(fx.local("kept.txt"), b"edited in the folder").unwrap();
    std::fs::remove_file(fx.local("restored.txt")).unwrap();
    fx.write_remote("restored.txt", b"edited in the workspace");

    assert_eq!(folder.reconcile().unwrap(), pass(1, 1, 0, 0));
    assert_eq!(fx.remote("kept.txt").unwrap(), b"edited in the folder");
    assert_eq!(
        std::fs::read(fx.local("restored.txt")).unwrap(),
        b"edited in the workspace"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_folder_edit_over_a_workspace_edit_keeps_the_folder_version() {
    let fx = Fixture::new().await;
    let mut folder = fx.folder();
    std::fs::write(fx.local("draft.txt"), b"base").unwrap();
    folder.reconcile().unwrap();

    fx.write_remote("draft.txt", b"workspace edit");
    std::fs::write(fx.local("draft.txt"), b"folder edit").unwrap();
    assert_eq!(folder.reconcile().unwrap(), pass(1, 0, 0, 0));

    assert_eq!(fx.remote("draft.txt").unwrap(), b"folder edit");
    assert_eq!(
        std::fs::read(fx.local("draft.txt")).unwrap(),
        b"folder edit"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn renames_keep_the_record_in_both_directions() {
    let fx = Fixture::new().await;
    let mut folder = fx.folder();
    std::fs::create_dir_all(fx.local("inbox")).unwrap();
    std::fs::write(fx.local("inbox/letter.txt"), b"dear").unwrap();
    folder.reconcile().unwrap();
    let record = fx.record("inbox/letter.txt");

    std::fs::rename(fx.local("inbox"), fx.local("archive")).unwrap();
    assert_eq!(folder.reconcile().unwrap().renamed, 1);
    assert_eq!(fx.record("archive/letter.txt"), record);
    assert!(fx.fs.entry_at(Path::new("inbox")).is_none());

    fx.fs
        .apply_local_move(Path::new("archive/letter.txt"), Path::new("letter.txt"))
        .unwrap();
    assert_eq!(folder.reconcile().unwrap(), pass(0, 0, 0, 1));
    assert_eq!(std::fs::read(fx.local("letter.txt")).unwrap(), b"dear");
    assert!(!fx.local("archive/letter.txt").exists());
    assert_eq!(fx.record("letter.txt"), record);
    assert_eq!(folder.reconcile().unwrap(), Pass::default());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_change_made_while_stopped_is_found_after_a_restart() {
    let fx = Fixture::new().await;
    std::fs::write(fx.local_parent_ready("a.txt"), b"before").unwrap();
    fx.folder().reconcile().unwrap();

    std::fs::write(fx.local("a.txt"), b"while stopped").unwrap();
    std::fs::write(fx.local("new.txt"), b"new").unwrap();
    assert_eq!(fx.folder().reconcile().unwrap(), pass(2, 0, 0, 0));
    assert_eq!(fx.remote("a.txt").unwrap(), b"while stopped");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_emptied_workspace_never_empties_the_folder() {
    let fx = Fixture::new().await;
    let mut folder = fx.folder();
    std::fs::write(fx.local_parent_ready("precious.txt"), b"mine").unwrap();
    folder.reconcile().unwrap();

    fx.fs.apply_local_remove(Path::new("precious.txt")).unwrap();
    assert_eq!(folder.reconcile().unwrap(), pass(1, 0, 0, 0));
    assert_eq!(std::fs::read(fx.local("precious.txt")).unwrap(), b"mine");
    assert_eq!(fx.remote("precious.txt").unwrap(), b"mine");
}

#[tokio::test(flavor = "multi_thread")]
async fn links_and_host_files_stay_out_of_the_workspace() {
    let fx = Fixture::new().await;
    let mut folder = fx.folder();
    let secret = fx.dir.path().join("outside.txt");
    std::fs::write(&secret, b"host secret").unwrap();
    std::os::unix::fs::symlink(&secret, fx.local_parent_ready("link.txt")).unwrap();
    std::fs::write(fx.local(".DS_Store"), b"finder").unwrap();

    assert_eq!(folder.reconcile().unwrap(), Pass::default());
    assert!(fx.fs.tree_snapshot().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn watching_carries_changes_without_being_asked() {
    let fx = Fixture::new().await;
    let _watch = fx.folder().watch(Duration::from_millis(100)).unwrap();
    std::fs::write(fx.local("watched.txt"), b"seen").unwrap();
    fx.write_remote("from-workspace.txt", b"arrived");

    let deadline = tilo_test_env::hang_deadline();
    while fx.remote("watched.txt").as_deref() != Some(b"seen".as_slice())
        || std::fs::read(fx.local("from-workspace.txt"))
            .ok()
            .as_deref()
            != Some(b"arrived".as_slice())
    {
        assert!(
            Instant::now() < deadline,
            "the watch did not carry both changes"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

impl Fixture {
    fn local_parent_ready(&self, path: &str) -> std::path::PathBuf {
        std::fs::create_dir_all(self.folder_path()).unwrap();
        self.local(path)
    }
}

/// Bytes no two seeds share, with no stretch of them repeating.
fn bytes(seed: u32, length: usize) -> Vec<u8> {
    let mut state = u64::from(seed).wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cloned_repository_lands_whole_over_bounded_passes() {
    use std::io::Write as _;

    let fx = Fixture::new().await;
    let mut folder = fx.folder();
    let mut passes = Vec::new();
    let pack_dir = fx.local("repo/.git/objects/pack");
    std::fs::create_dir_all(&pack_dir).unwrap();
    std::fs::write(fx.local("repo/.git/HEAD"), b"ref: refs/heads/main\n").unwrap();
    // git grows the pack in a temporary file, which passes see half written,
    // and renames it into place once whole.
    let mut pack = Vec::new();
    {
        let mut file = std::fs::File::create(pack_dir.join("tmp_pack_10wUgj")).unwrap();
        for step in 0..6 {
            let part = bytes(10 + step, 1024 * 1024);
            file.write_all(&part).unwrap();
            file.flush().unwrap();
            pack.extend_from_slice(&part);
            passes.push(folder.reconcile().unwrap());
        }
    }
    std::fs::rename(
        pack_dir.join("tmp_pack_10wUgj"),
        pack_dir.join("pack-37ad.pack"),
    )
    .unwrap();
    let index = bytes(20, 90_000);
    std::fs::write(pack_dir.join("tmp_idx_k2"), &index).unwrap();
    std::fs::rename(pack_dir.join("tmp_idx_k2"), pack_dir.join("pack-37ad.idx")).unwrap();
    // Then the worktree lands at once: more files than one pass carries.
    let files = super::PASS_PATHS * 2 + 300;
    let mut worktree = Vec::new();
    for file in 0..files {
        let path = format!("repo/crates/c{}/src/m{file}.rs", file % 40);
        let content = bytes(1000 + u32::try_from(file).unwrap(), 200 + file % 900);
        std::fs::create_dir_all(fx.local(&path).parent().unwrap()).unwrap();
        std::fs::write(fx.local(&path), &content).unwrap();
        worktree.push((path, content));
    }
    let staged = bytes(30, 40_000);
    std::fs::write(fx.local("repo/.git/index.lock"), &staged).unwrap();
    std::fs::rename(
        fx.local("repo/.git/index.lock"),
        fx.local("repo/.git/index"),
    )
    .unwrap();

    let started = Instant::now();
    loop {
        let pass = folder.reconcile().unwrap();
        passes.push(pass);
        if pass.deferred == 0 {
            break;
        }
        assert!(pass.more(), "a pass that left work made progress: {pass:?}");
        assert!(started.elapsed() < Duration::from_secs(120), "{passes:?}");
    }
    for pass in &passes {
        assert_eq!(pass.failed, 0, "{passes:?}");
        assert!(
            pass.imported + pass.exported <= super::PASS_PATHS,
            "{passes:?}"
        );
    }
    assert!(
        passes.iter().filter(|pass| pass.deferred > 0).count() >= 2,
        "the worktree took several passes: {passes:?}"
    );
    assert_eq!(folder.unsynced().count(), 0);
    assert_eq!(
        fx.remote("repo/.git/objects/pack/pack-37ad.pack").unwrap(),
        pack
    );
    assert_eq!(
        fx.remote("repo/.git/objects/pack/pack-37ad.idx").unwrap(),
        index
    );
    assert_eq!(fx.remote("repo/.git/index").unwrap(), staged);
    assert!(
        fx.remote("repo/.git/objects/pack/tmp_pack_10wUgj")
            .is_none()
    );
    assert!(fx.remote("repo/.git/index.lock").is_none());
    for (path, content) in &worktree {
        assert_eq!(fx.remote(path).as_ref(), Some(content), "{path}");
    }
    assert_eq!(folder.reconcile().unwrap(), Pass::default());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_path_that_keeps_failing_waits_and_holds_nothing_else_back() {
    let fx = Fixture::new().await;
    let mut folder = fx.folder();
    std::fs::write(fx.local("stuck.txt"), b"first").unwrap();
    assert_eq!(folder.reconcile().unwrap(), pass(1, 0, 0, 0));
    fx.fs
        .fail_versions_at(Path::new("stuck.txt"), Some(libc::EIO));

    // Its new version fails; the file beside it is carried across.
    std::fs::write(fx.local("stuck.txt"), b"second").unwrap();
    std::fs::write(fx.local("other.txt"), b"other").unwrap();
    let failed = folder.reconcile().unwrap();
    assert_eq!((failed.imported, failed.failed), (1, 1), "{failed:?}");
    assert_eq!(fx.remote("other.txt").unwrap(), b"other");
    let unsynced: Vec<_> = folder.unsynced().collect();
    assert_eq!(unsynced.len(), 1, "{unsynced:?}");
    assert_eq!(unsynced[0].0, "stuck.txt");

    // The passes right after it leave it waiting, and carry what else changed.
    std::fs::write(fx.local("third.txt"), b"third").unwrap();
    let waited = folder.reconcile().unwrap();
    assert_eq!((waited.imported, waited.failed), (1, 0), "{waited:?}");
    assert_eq!(folder.reconcile().unwrap(), Pass::default());
    assert_eq!(fx.remote("stuck.txt").unwrap(), b"first");

    // A change to it is worth a try at once.
    std::fs::write(fx.local("stuck.txt"), b"third try").unwrap();
    assert_eq!(folder.reconcile().unwrap().failed, 1);
    fx.fs.fail_versions_at(Path::new("stuck.txt"), None);
    std::fs::write(fx.local("stuck.txt"), b"the fourth one").unwrap();
    assert_eq!(folder.reconcile().unwrap(), pass(1, 0, 0, 0));
    assert_eq!(fx.remote("stuck.txt").unwrap(), b"the fourth one");
    assert_eq!(folder.unsynced().count(), 0);
}
