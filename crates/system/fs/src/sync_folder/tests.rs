use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use collab_types::{Ed25519PublicKey, FileActor, FileProvenance, FileRecordId};
use iroh::endpoint::{Endpoint, presets};

use super::{Pass, STAGING_PREFIX, SyncFolder};
use crate::fs::QuickFS;
use crate::history::FileHistoryStore;
use crate::hydration::Hydrator;
use crate::namespace::Namespace;
use crate::smart::Heuristics;
use crate::store::ContentStore;

/// A workspace with history, as the runtime keeps one, and its folder.
struct Fixture {
    dir: tempfile::TempDir,
    fs: Arc<QuickFS>,
    history: Arc<FileHistoryStore>,
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let hydrator = Arc::new(Hydrator::new(endpoint, Arc::new(ContentStore::memory())));
        let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
        let history = Arc::new(FileHistoryStore::open(dir.path().join("history.json")).unwrap());
        let actor = FileActor {
            person: Ed25519PublicKey([1; 32]),
            device: Ed25519PublicKey([2; 32]),
        };
        let heuristics = Arc::new(Heuristics::new(Arc::clone(&hydrator)));
        let fs = QuickFS::new(namespace, hydrator, heuristics)
            .with_history(Arc::clone(&history), actor)
            .unwrap();
        Self {
            dir,
            fs: Arc::new(fs),
            history,
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

    /// A write to the workspace made elsewhere (an app, a peer).
    fn write_remote(&self, path: &str, bytes: &[u8]) {
        let source = self.dir.path().join("remote-write");
        std::fs::write(&source, bytes).unwrap();
        self.fs.write_path(Path::new(path), &source, 1_000).unwrap();
    }

    fn remote(&self, path: &str) -> Option<Vec<u8>> {
        self.fs.read_path(Path::new(path)).ok()
    }

    fn record(&self, path: &str) -> FileRecordId {
        self.fs.entry_at(Path::new(path)).unwrap().record.unwrap()
    }

    fn versions(&self, path: &str) -> Vec<collab_types::FileVersionV1> {
        self.history.snapshot(self.record(path)).unwrap().versions
    }
}

fn pass(imported: usize, exported: usize, removed: usize, renamed: usize) -> Pass {
    Pass {
        imported,
        exported,
        removed,
        renamed,
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
    let versions = fx.versions("notes/plan.txt");
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0].provenance, FileProvenance::Create);
    assert_eq!(versions[1].provenance, FileProvenance::Edit);

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
        names.iter().all(|name| !name.starts_with(STAGING_PREFIX)),
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
async fn concurrent_edits_keep_the_folder_version_and_the_workspace_history() {
    let fx = Fixture::new().await;
    let mut folder = fx.folder();
    std::fs::write(fx.local("draft.txt"), b"base").unwrap();
    folder.reconcile().unwrap();

    fx.write_remote("draft.txt", b"workspace edit");
    let workspace_edit = fx
        .fs
        .entry_at(Path::new("draft.txt"))
        .unwrap()
        .hash
        .unwrap();
    std::fs::write(fx.local("draft.txt"), b"folder edit").unwrap();
    assert_eq!(folder.reconcile().unwrap(), pass(1, 0, 0, 0));

    assert_eq!(fx.remote("draft.txt").unwrap(), b"folder edit");
    assert_eq!(
        std::fs::read(fx.local("draft.txt")).unwrap(),
        b"folder edit"
    );
    let versions = fx.versions("draft.txt");
    assert_eq!(versions.len(), 3, "{versions:?}");
    assert!(
        versions
            .iter()
            .any(|v| v.content_hash.map(|h| h.0) == Some(*workspace_edit.as_bytes())),
        "the overwritten workspace version stays in its history"
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
async fn a_legacy_format_file_is_imported_as_a_document_and_exported_again() {
    let fx = Fixture::new().await;
    let mut folder = fx.folder();
    std::fs::write(fx.local("readme.md"), "# Plan\n\nShip it.\n").unwrap();
    folder.reconcile().unwrap();

    assert!(fx.fs.document_at(Path::new("readme.md")));
    let exported = String::from_utf8(fx.remote("readme.md").unwrap()).unwrap();
    assert!(exported.contains("Ship it."), "{exported}");

    fx.write_remote("readme.md", b"# Plan\n\nShip it today.\n");
    assert_eq!(folder.reconcile().unwrap(), pass(0, 1, 0, 0));
    let local = std::fs::read_to_string(fx.local("readme.md")).unwrap();
    assert!(local.contains("Ship it today."), "{local}");
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
    /// `path` in the folder, with the folder created.
    fn local_parent_ready(&self, path: &str) -> std::path::PathBuf {
        std::fs::create_dir_all(self.folder_path()).unwrap();
        self.local(path)
    }
}
