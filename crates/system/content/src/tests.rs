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
