//! Close, fsync and release never report success for bytes that were not
//! persisted, and never discard bytes that were not committed.

use super::*;
use collab_types::{Ed25519PublicKey, FileActor};
use ed25519_dalek::SigningKey;
use iroh::endpoint::{Endpoint, presets};
use uuid::Uuid;

struct Fixture {
    dir: tempfile::TempDir,
    namespace: Arc<Mutex<Namespace>>,
    hydrator: Arc<Hydrator>,
    history: Arc<crate::history::FileHistoryStore>,
    publisher: Arc<crate::record_publisher::RecordPublisher>,
    actor: FileActor,
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let store = Arc::new(crate::store::ContentStore::memory());
        let hydrator = Arc::new(Hydrator::new(endpoint, store));
        let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
        let history = Arc::new(
            crate::history::FileHistoryStore::open(dir.path().join("history.json")).unwrap(),
        );
        let repository = Arc::new(
            crate::record_repository::RecordRepository::open(dir.path().join("records")).unwrap(),
        );
        let person = SigningKey::from_bytes(&[0x41; 32]);
        let device = SigningKey::from_bytes(&[0x52; 32]);
        let actor = FileActor {
            person: Ed25519PublicKey(person.verifying_key().to_bytes()),
            device: Ed25519PublicKey(device.verifying_key().to_bytes()),
        };
        let workspace_id = Uuid::from_u128(0xd0);
        let publisher = Arc::new(
            crate::record_publisher::RecordPublisher::new(repository, workspace_id, actor, device)
                .unwrap(),
        );
        Self {
            dir,
            namespace,
            hydrator,
            history,
            publisher,
            actor,
        }
    }

    /// A signed mount over the fixture's durable state, as after a
    /// (re)start. `staging` attaches the unsaved-write store.
    fn mount(&self, staging: bool) -> QuickFS {
        let heuristics = Arc::new(Heuristics::new(self.hydrator.clone()));
        let fs = QuickFS::new(self.namespace.clone(), self.hydrator.clone(), heuristics)
            .with_history(self.history.clone(), self.actor)
            .unwrap()
            .with_record_publisher(self.publisher.clone())
            .unwrap();
        if staging {
            fs.with_unsaved_store(crate::unsaved::UnsavedStore::open(self.staging_dir()).unwrap())
        } else {
            fs
        }
    }

    fn staging_dir(&self) -> std::path::PathBuf {
        self.dir.path().join("unsaved")
    }

    fn staged(&self) -> Vec<crate::unsaved::LoadedUnsaved> {
        let (loaded, damaged) = crate::unsaved::UnsavedStore::open(self.staging_dir())
            .unwrap()
            .load_all();
        assert!(damaged.is_empty());
        loaded
    }

    fn versions(&self, record_id: collab_types::FileRecordId) -> usize {
        self.history.snapshot(record_id).unwrap().versions.len()
    }
}

/// What the FUSE `create` callback leaves behind: a committed empty file
/// with an empty buffer and one writable handle.
fn create_open(fs: &QuickFS, name: &str) -> (u64, u64) {
    let (ino, delta) = fs
        .create_empty_file(crate::namespace::ROOT_INO, name)
        .unwrap();
    fs.publish(delta);
    fs.buffers().insert(ino, Vec::new());
    fs.buffer_states().insert(
        ino,
        BufferState {
            committed: Some(0),
            ..BufferState::default()
        },
    );
    (ino, open_handle(fs, ino, true))
}

fn open_handle(fs: &QuickFS, ino: u64, writable: bool) -> u64 {
    let handle = fs.next_file_handle.fetch_add(1, Ordering::Relaxed);
    let opened_path = fs.ns().path_for(ino).unwrap();
    fs.open_file_handles.lock().unwrap().insert(
        handle,
        OpenFileHandleState {
            ino,
            opened_path,
            writable,
        },
    );
    handle
}

async fn committed_bytes(fs: &QuickFS, ino: u64) -> Vec<u8> {
    let hash = fs.ns().hash_for(ino);
    match hash {
        Some(hash) => fs.hydrator.read(hash, 0, u64::MAX).await.unwrap(),
        None => Vec::new(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_or_unstaged_fsync_commits_or_reports_without_discarding_bytes() {
    // Phase `a_failed_commit_fails_close_and_never_discards_the_bytes`.
    {
        let fixture = Fixture::new().await;
        let fs = fixture.mount(true);
        let (ino, fh) = create_open(&fs, "notes.txt");
        let record_id = fs.ns().record_id_for(ino).unwrap();
        fs.write_buffer(ino, 0, b"precious bytes").unwrap();

        // Make the durable signed publication fail.
        let repository = fs.record_publisher.as_ref().unwrap().repository();
        repository.fail_writes(true);

        assert_eq!(
            fs.flush_inode(ino),
            Err(EIO),
            "close must report the failure"
        );
        assert_eq!(fs.fsync_inode(ino), Ok(()), "fsync stages durably instead");
        fs.write_buffer(ino, 8, b"BYTES").unwrap();
        assert_eq!(fs.release_handle(ino, fh), Err(EIO));
        assert_eq!(
            fs.buffers().get(&ino).map(Vec::as_slice),
            Some(b"preciousBYTESs".as_slice()),
            "the last close must not drop bytes that were never committed"
        );
        let staged = fixture.staged();
        assert_eq!(staged.len(), 1);
        assert_eq!(staged[0].bytes, b"preciousBYTESs");
        assert!(staged[0].entry.error.is_some());
        assert!(committed_bytes(&fs, ino).await.is_empty());

        repository.fail_writes(false);

        // Any later close retries; success settles the retained buffer.
        let reader = open_handle(&fs, ino, false);
        assert_eq!(fs.flush_inode(ino), Ok(()));
        assert_eq!(fs.release_handle(ino, reader), Ok(()));
        assert!(!fs.buffers().contains_key(&ino));
        assert_eq!(committed_bytes(&fs, ino).await, b"preciousBYTESs");
        assert!(fixture.staged().is_empty());
        assert_eq!(fixture.versions(record_id), 2);
    }
    // Phase `fsync_without_staging_commits_and_reports_failure`.
    {
        let fixture = Fixture::new().await;
        let fs = fixture.mount(false);
        let (ino, fh) = create_open(&fs, "plain.bin");
        let record_id = fs.ns().record_id_for(ino).unwrap();
        fs.write_buffer(ino, 0, b"abc").unwrap();
        *fs.commit_fault.lock().unwrap() = Some((ino, EIO));
        assert_eq!(fs.fsync_inode(ino), Err(EIO));
        assert_eq!(fs.release_handle(ino, fh), Err(EIO));
        assert!(fs.buffers().contains_key(&ino));
        *fs.commit_fault.lock().unwrap() = None;
        assert_eq!(fs.fsync_inode(ino), Ok(()));
        assert_eq!(committed_bytes(&fs, ino).await, b"abc");
        assert_eq!(fixture.versions(record_id), 2);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn more_fsyncs_than_the_version_bound_lose_nothing() {
    const VERSION_BOUND: usize = 64;
    let fixture = Fixture::new().await;
    fixture
        .publisher
        .repository()
        .set_max_record_versions(VERSION_BOUND);
    let fs = fixture.mount(true);
    let (ino, fh) = create_open(&fs, "app.sqlite");
    let record_id = fs.ns().record_id_for(ino).unwrap();
    let syncs = VERSION_BOUND + 100;
    let mut expected = vec![0_u8; 4096];
    fs.write_buffer(ino, 0, &expected).unwrap();
    for i in 0..syncs {
        let offset = (i % 512) * 8;
        let word = (i as u64).to_le_bytes();
        expected[offset..offset + 8].copy_from_slice(&word);
        fs.write_buffer(ino, offset as u64, &word).unwrap();
        assert_eq!(fs.fsync_inode(ino), Ok(()), "fsync {i}");
    }
    assert_eq!(
        fixture.versions(record_id),
        1,
        "fsync makes bytes durable without minting a version per call"
    );
    assert_eq!(fixture.staged()[0].bytes, expected);
    assert_eq!(fs.flush_inode(ino), Ok(()));
    assert_eq!(fs.release_handle(ino, fh), Ok(()));
    assert_eq!(fixture.versions(record_id), 2);
    assert_eq!(committed_bytes(&fs, ino).await, expected);
    assert!(fixture.staged().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unacceptable_or_uncommitted_bytes_survive_as_copies_and_across_crashes() {
    // Phase `bytes_a_record_cannot_take_are_kept_as_a_sibling_copy`.
    {
        let fixture = Fixture::new().await;
        let fs = fixture.mount(true);
        let (ino, fh) = create_open(&fs, "journal.log");
        fs.write_buffer(ino, 0, b"entry that must survive").unwrap();
        *fs.commit_fault.lock().unwrap() = Some((ino, EDQUOT));
        assert_eq!(fs.flush_inode(ino), Err(EDQUOT));
        assert_eq!(fs.release_handle(ino, fh), Err(EDQUOT));

        let copy_name = unsaved_copy_name(b"journal.log", b"entry that must survive");
        let copy = fs
            .ns()
            .lookup(crate::namespace::ROOT_INO, &copy_name)
            .expect("the bytes are preserved as a visible sibling")
            .ino
            .0;
        assert_eq!(committed_bytes(&fs, copy).await, b"entry that must survive");
        assert!(committed_bytes(&fs, ino).await.is_empty());
        assert!(!fs.buffers().contains_key(&ino));
        assert!(fixture.staged().is_empty());
    }
    // Phase `fsynced_bytes_survive_a_crash_before_close`.
    {
        let fixture = Fixture::new().await;
        let (ino, moved_on) = {
            let fs = fixture.mount(true);
            let (ino, _fh) = create_open(&fs, "wal.db");
            fs.write_buffer(ino, 0, b"committed transaction").unwrap();
            assert_eq!(fs.fsync_inode(ino), Ok(()));
            let (other, _fh) = create_open(&fs, "other.db");
            fs.write_buffer(other, 0, b"synced before a later edit")
                .unwrap();
            assert_eq!(fs.fsync_inode(other), Ok(()));
            // Another version lands before the restart.
            fs.commit_content(other, b"later edit").unwrap();
            // Dropped without close: the process died.
            (ino, other)
        };
        let fs = fixture.mount(true);
        assert_eq!(fs.recover_unsaved_writes(), 2);
        assert_eq!(committed_bytes(&fs, ino).await, b"committed transaction");
        assert_eq!(
            committed_bytes(&fs, moved_on).await,
            b"later edit",
            "replay never overwrites content that moved on"
        );
        let copy_name = unsaved_copy_name(b"other.db", b"synced before a later edit");
        let copy = fs
            .ns()
            .lookup(crate::namespace::ROOT_INO, &copy_name)
            .unwrap()
            .ino
            .0;
        assert_eq!(
            committed_bytes(&fs, copy).await,
            b"synced before a later edit"
        );
        assert!(fixture.staged().is_empty());
        assert_eq!(fs.recover_unsaved_writes(), 0);
    }
}
