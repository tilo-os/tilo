#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use super::*;
use std::io::Read;

/// A 1x1 PNG.
const TINY_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
    0x42, 0x60, 0x82,
];

fn limits(item: usize, total: usize, entries: usize) -> ContentLimits {
    ContentLimits {
        max_item_bytes: item,
        max_total_bytes: total,
        max_entries: entries,
        max_scan_entries: 64,
    }
}

#[test]
fn png_dimensions_come_from_the_header() {
    let m = sniff(TINY_PNG);
    assert_eq!(m.mime, "image/png");
    assert_eq!((m.w, m.h), (1, 1));
    assert_eq!(m.len, TINY_PNG.len() as u64);
}

#[test]
fn unknown_content_still_reports_its_length() {
    let m = sniff(b"not an image at all");
    assert_eq!(m.mime, "");
    assert_eq!((m.w, m.h), (0, 0));
    assert_eq!(m.len, 19, "length is knowable even when the format is not");
}

#[test]
fn memory_and_directory_stores_round_trip_by_address() {
    let dir = tempfile::tempdir().unwrap();
    for store in [ContentStore::memory(), ContentStore::in_dir(dir.path())] {
        let hash = store.put(TINY_PNG).unwrap();
        assert_eq!(hash, hash_bytes(TINY_PNG));
        assert_eq!(store.read(&hash).as_deref(), Some(TINY_PNG));
        assert!(store.has(&hash));
        assert_eq!(store.state(&hash), ContentState::Resolved);
        let missing = hash_bytes(b"never stored");
        assert_eq!(store.read(&missing), None);
        assert_eq!(store.state(&missing), ContentState::Unavailable);
        // Storing the same content again is a no-op that names the same address.
        assert_eq!(store.put(TINY_PNG).unwrap(), hash);
    }
}

#[test]
fn a_store_is_a_handle_not_a_process_global() {
    let a = ContentStore::memory();
    let b = ContentStore::memory();
    let hash = a.put(b"only in a").unwrap();
    let clone = a.clone();
    drop(a);
    assert!(clone.has(&hash), "clones share one store");
    assert!(!b.has(&hash), "separate stores share nothing");
}

#[test]
fn directory_writes_and_reads_stream() {
    let dir = tempfile::tempdir().unwrap();
    let store = ContentStore::in_dir(dir.path());
    // Larger than the copy buffer, so the write takes several chunks.
    let big: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    let stored = store
        .put_reader(&mut big.as_slice(), "application/octet-stream")
        .unwrap();
    assert_eq!(stored.hash, hash_bytes(&big));
    assert_eq!(stored.len, big.len() as u64);
    let mut reader = store.open(&stored.hash).unwrap();
    let mut back = Vec::new();
    reader.read_to_end(&mut back).unwrap();
    assert_eq!(back, big);
}

#[test]
fn directory_store_rejects_traversal_and_malformed_addresses() {
    let dir = tempfile::tempdir().unwrap();
    let store = ContentStore::in_dir(dir.path());
    assert!(store.read("../../etc/passwd").is_none());
    assert!(!store.has("../escape"));
}

#[test]
fn corrupt_content_is_never_observable() {
    let dir = tempfile::tempdir().unwrap();
    let hash = hash_bytes(b"good");
    let shard = dir.path().join(&hash[..2]);
    std::fs::create_dir_all(&shard).unwrap();
    std::fs::write(shard.join(&hash), b"corrupt").unwrap();
    std::fs::write(dir.path().join(".incoming-dead-0"), b"partial").unwrap();
    let store = ContentStore::new(DirBackend::with_limits(dir.path(), limits(8, 16, 4)));
    assert!(store.read(&hash).is_none(), "a verifying read refuses it");
    assert!(!store.has(&hash));
    let mut reader = store.open(&hash).unwrap();
    let mut sink = Vec::new();
    assert_eq!(
        reader.read_to_end(&mut sink).unwrap_err().kind(),
        std::io::ErrorKind::InvalidData,
        "a streaming read fails at the end"
    );
    // Storing the real content replaces the corrupt file.
    assert_eq!(store.put(b"good").unwrap(), hash);
    assert_eq!(store.read(&hash).as_deref(), Some(b"good".as_slice()));
}

#[test]
fn quotas_hold_across_restarts_and_refuse_without_evicting() {
    let dir = tempfile::tempdir().unwrap();
    let l = limits(4, 6, 4);
    {
        let s = ContentStore::new(DirBackend::with_limits(dir.path(), l));
        s.put(b"abc").unwrap();
        s.put(b"de").unwrap();
    }
    let s = ContentStore::new(DirBackend::with_limits(dir.path(), l));
    assert_eq!(
        s.put(b"fgh"),
        Err(ContentError::TotalQuotaExceeded {
            attempted: 8,
            limit: 6
        })
    );
    assert_eq!(
        s.put(b"12345"),
        Err(ContentError::ItemTooLarge {
            actual: 5,
            limit: 4
        })
    );
    assert!(s.has(&hash_bytes(b"abc")), "a refusal evicts nothing");
    let memory = ContentStore::new(MemoryBackend::new(limits(4, 6, 2)));
    memory.put(b"abc").unwrap();
    memory.put(b"de").unwrap();
    assert_eq!(
        memory.put(b"x"),
        Err(ContentError::EntryLimitExceeded { limit: 2 })
    );
}

#[test]
fn collection_removes_exactly_what_the_live_set_omits() {
    let dir = tempfile::tempdir().unwrap();
    for store in [ContentStore::memory(), ContentStore::in_dir(dir.path())] {
        let keep = store.put(b"kept").unwrap();
        let a = store.put(b"a").unwrap();
        let b = store.put(b"b").unwrap();
        let collected = store
            .collect_unreferenced(&BTreeSet::from([keep.clone()]))
            .unwrap();
        let mut expected = vec![a.clone(), b.clone()];
        expected.sort();
        assert_eq!(collected.hashes, expected);
        assert_eq!(collected.bytes, 2);
        assert!(store.has(&keep));
        assert!(!store.has(&a) && !store.has(&b));
    }
}

#[test]
fn concurrent_writers_on_one_root_cannot_race_past_the_quota() {
    let dir = tempfile::tempdir().unwrap();
    let l = limits(4, 6, 4);
    let first = ContentStore::new(DirBackend::with_limits(dir.path(), l));
    let second = ContentStore::new(DirBackend::with_limits(dir.path(), l));
    let barrier = std::sync::Barrier::new(2);
    let (one, two) = std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            barrier.wait();
            first.put(b"abcd")
        });
        let b = scope.spawn(|| {
            barrier.wait();
            second.put(b"efgh")
        });
        (a.join().unwrap(), b.join().unwrap())
    });
    assert_eq!(usize::from(one.is_ok()) + usize::from(two.is_ok()), 1);
}

#[test]
fn an_earlier_layout_still_reads() {
    let old = tempfile::tempdir().unwrap();
    let new = tempfile::tempdir().unwrap();
    ContentStore::in_dir(old.path()).put(TINY_PNG).unwrap();
    let store = ContentStore::new(DirBackend::new(new.path()).reading_also(old.path()));
    assert_eq!(store.read(&hash_bytes(TINY_PNG)).as_deref(), Some(TINY_PNG));
}

#[test]
fn the_shared_session_directory_wins() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let store = ContentStore::for_app_with(&|name| {
        (name == SHARED_STORE_ENV).then(|| path.clone().into_os_string())
    });
    let hash = store.put(b"shared").unwrap();
    assert_eq!(
        ContentStore::in_dir(dir.path()).read(&hash).as_deref(),
        Some(b"shared".as_slice())
    );
}

#[test]
fn inputs_dedupe_and_persist_into_a_store() {
    let mut inputs = ContentInputs::new();
    let a = inputs.add(TINY_PNG.to_vec());
    assert_eq!(inputs.add(TINY_PNG), a);
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs.content(&a).as_deref(), Some(TINY_PNG));
    let store = ContentStore::memory();
    store.put_inputs(&inputs).unwrap();
    assert!(store.has(&a));
    // A chain answers from the first source that has the content.
    let chain = (NoContent, &store);
    assert_eq!(chain.content(&a).as_deref(), Some(TINY_PNG));
}

/// A store over a directory backend the test can still inspect.
fn probed(backend: DirBackend) -> (ContentStore, Arc<DirBackend>) {
    let backend = Arc::new(backend);
    (ContentStore::from_backend(backend.clone()), backend)
}

fn count(counter: &std::sync::atomic::AtomicUsize) -> usize {
    counter.load(std::sync::atomic::Ordering::Relaxed)
}

fn item_path(root: &std::path::Path, hash: &str) -> std::path::PathBuf {
    root.join(&hash[..2]).join(hash)
}

#[test]
fn has_does_not_rehash_a_verified_item() {
    let dir = tempfile::tempdir().unwrap();
    let big: Vec<u8> = (0..300_000u32).map(|i| (i % 253) as u8).collect();
    let (store, backend) = probed(DirBackend::new(dir.path()));
    let hash = store.put(&big).unwrap();
    for _ in 0..1000 {
        assert!(store.has(&hash));
        assert_eq!(store.state(&hash), ContentState::Resolved);
    }
    assert_eq!(
        count(&backend.probes().verifications),
        0,
        "the writer hashed the item as it streamed in"
    );
    // Another backend on the same root has hashed nothing yet: once, then never again.
    let (other, other_backend) = probed(DirBackend::new(dir.path()));
    for _ in 0..1000 {
        assert!(other.has(&hash));
    }
    assert_eq!(count(&other_backend.probes().verifications), 1);
    // A repeated put of the same content is idempotent without reading the item again either.
    assert_eq!(other.put(&big).unwrap(), hash);
    assert_eq!(count(&other_backend.probes().verifications), 1);
}

#[test]
fn has_reverifies_after_external_modification() {
    let dir = tempfile::tempdir().unwrap();
    let store = ContentStore::in_dir(dir.path());
    let hash = store.put(b"original").unwrap();
    assert!(store.has(&hash));
    let path = item_path(dir.path(), &hash);
    let before = std::fs::metadata(&path).unwrap().modified().unwrap();
    // Same length, different bytes, rewritten in place (same inode), with a later mtime.
    std::fs::write(&path, b"tampered").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(
            before
                .checked_add(std::time::Duration::from_secs(1))
                .unwrap(),
        )
        .unwrap();
    assert!(!store.has(&hash), "a changed file is verified again");
    assert_eq!(store.state(&hash), ContentState::Unavailable);
    assert!(store.read(&hash).is_none());
    // Replaced by another file holding the right bytes: verified again, and intact.
    let replacement = dir.path().join("replacement");
    std::fs::write(&replacement, b"original").unwrap();
    std::fs::rename(&replacement, &path).unwrap();
    assert!(store.has(&hash));
}

#[test]
fn new_put_does_not_rescan_when_generation_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let (first, first_backend) = probed(DirBackend::new(dir.path()));
    first.put(b"one").unwrap();
    first.put(b"two").unwrap();
    first.put(b"three").unwrap();
    assert_eq!(
        count(&first_backend.probes().scans),
        1,
        "only the cold start scans the store"
    );
    let (second, second_backend) = probed(DirBackend::new(dir.path()));
    second.put(b"four").unwrap();
    assert_eq!(count(&second_backend.probes().scans), 1);
    first.put(b"five").unwrap();
    assert_eq!(
        count(&first_backend.probes().scans),
        2,
        "another writer's put makes the kept index stale"
    );
    first.put(b"six").unwrap();
    assert_eq!(count(&first_backend.probes().scans), 2);
}

#[test]
fn a_kept_index_still_counts_every_writer() {
    let dir = tempfile::tempdir().unwrap();
    // "one" + "two" + "three" = 11 bytes; room for one more byte.
    let l = limits(8, 12, 64);
    let (first, _) = probed(DirBackend::with_limits(dir.path(), l));
    let (second, second_backend) = probed(DirBackend::with_limits(dir.path(), l));
    first.put(b"one").unwrap();
    second.put(b"two").unwrap();
    first.put(b"three").unwrap();
    first.put(b"4").unwrap();
    assert_eq!(
        second.put(b"5"),
        Err(ContentError::TotalQuotaExceeded {
            attempted: 13,
            limit: 12
        }),
        "the second backend's kept total misses the first's later writes"
    );
    assert_eq!(count(&second_backend.probes().scans), 2);
}

#[test]
fn index_recovers_from_external_deletion_before_refusing() {
    let dir = tempfile::tempdir().unwrap();
    let (store, backend) = probed(DirBackend::with_limits(dir.path(), limits(4, 6, 4)));
    let abc = store.put(b"abc").unwrap();
    store.put(b"de").unwrap();
    assert_eq!(count(&backend.probes().scans), 1);
    std::fs::remove_file(item_path(dir.path(), &abc)).unwrap();
    assert_eq!(store.put(b"fgh"), Ok(hash_bytes(b"fgh")));
    assert_eq!(
        count(&backend.probes().scans),
        2,
        "the over-counting index was rescanned before refusing"
    );
    // A real refusal still refuses, against the store's true total.
    assert_eq!(
        store.put(b"ij"),
        Err(ContentError::TotalQuotaExceeded {
            attempted: 7,
            limit: 6
        })
    );
}

#[test]
fn durable_publish_syncs_the_directory_and_regenerable_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let (store, backend) = probed(DirBackend::new(dir.path()));
    let first = store.put(b"first").unwrap();
    assert_eq!(
        count(&backend.probes().dir_syncs),
        2,
        "a new shard: the root that gained it and the shard that gained the item"
    );
    // Content whose address shares the first one's shard lands in an existing directory.
    // One candidate in 256 matches; ten thousand candidates all missing is not a real outcome.
    let same_shard = (0u32..10_000)
        .map(|i| i.to_string())
        .find(|candidate| hash_bytes(candidate.as_bytes())[..2] == first[..2])
        .unwrap();
    store.put(same_shard.as_bytes()).unwrap();
    assert_eq!(
        count(&backend.probes().dir_syncs),
        3,
        "an existing shard: only the shard"
    );
    let cache = tempfile::tempdir().unwrap();
    let (store, backend) = probed(DirBackend::new(cache.path()).regenerable());
    store.put(b"first").unwrap();
    store.put(same_shard.as_bytes()).unwrap();
    assert_eq!(count(&backend.probes().dir_syncs), 0);
}
