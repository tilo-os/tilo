#![allow(
    clippy::string_slice,
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
fn image_format_reads_the_signature_alone() {
    let format = |bytes: &[u8]| image_format(bytes).map(|format| format.extension);
    assert_eq!(format(b"\x89PNG\r\n\x1a\n"), Some("png"));
    assert_eq!(format(&[0xff, 0xd8, 0xff, 0xe0]), Some("jpg"));
    assert_eq!(format(b"GIF89a"), Some("gif"));
    assert_eq!(format(b"RIFFxxxxWEBP"), Some("webp"));
    assert_eq!(format(b"  <svg xmlns=\"\"/>"), Some("svg"));
    assert_eq!(format(b"<?xml version=\"1.0\"?><svg/>"), Some("svg"));
    assert_eq!(format(b"<?xml version=\"1.0\"?><html/>"), None);
    assert_eq!(format(b"not an image"), None);
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

/// Every file under `root`, read whole.
fn files_under(root: &std::path::Path) -> Vec<Vec<u8>> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(root).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(files_under(&path));
        } else if let Ok(bytes) = std::fs::read(&path) {
            found.push(bytes);
        }
    }
    found
}

#[test]
fn a_sealing_store_keeps_ciphertext_its_references_alone_open() {
    let dir = tempfile::tempdir().unwrap();
    let store = ContentStore::in_dir(dir.path()).sealing();
    let picture = [TINY_PNG, &vec![7_u8; 3 * 64 * 1024 + 5]].concat();
    let reference = store.put(&picture).unwrap();

    let parsed = ContentRef::parse(&reference).unwrap();
    assert!(parsed.is_sealed());
    assert_eq!(parsed.to_string(), reference);
    assert_ne!(
        parsed.address(),
        hash_bytes(&picture),
        "stored under the ciphertext's address"
    );
    assert_eq!(
        content_address(&reference).as_deref(),
        Some(parsed.address())
    );
    assert_eq!(store.read(&reference).as_deref(), Some(picture.as_slice()));
    let mut streamed = Vec::new();
    store
        .open(&reference)
        .unwrap()
        .read_to_end(&mut streamed)
        .unwrap();
    assert_eq!(streamed, picture);
    assert!(store.has(&reference));
    assert_eq!(store.state(&reference), ContentState::Resolved);

    let on_disk = files_under(dir.path());
    assert!(!on_disk.is_empty());
    assert!(
        on_disk
            .iter()
            .all(|file| !file.windows(64).any(|w| w == &picture[..64])),
        "nothing on disk holds the plaintext"
    );
    let stored = store.read(parsed.address()).unwrap();
    assert!(
        verify(parsed.address(), &stored),
        "the address alone reads ciphertext"
    );
    assert_ne!(stored, picture);

    // The same store opened without sealing (a host, another device) reads the
    // sealed reference; the same bytes put again get a key of their own.
    let plain = ContentStore::in_dir(dir.path());
    assert_eq!(plain.read(&reference).as_deref(), Some(picture.as_slice()));
    assert_ne!(store.put(&picture).unwrap(), reference);
    let mut forged = parsed.address().to_owned();
    forged.push_str(".AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
    assert!(store.read(&forged).is_none(), "another key opens nothing");
}

#[test]
fn plain_references_still_read_and_reseal_on_the_next_write() {
    let dir = tempfile::tempdir().unwrap();
    let plain = ContentStore::in_dir(dir.path());
    let old = plain.put(TINY_PNG).unwrap();
    assert_eq!(old, hash_bytes(TINY_PNG));
    assert!(
        plain.reseal(&old).is_none(),
        "a store that does not seal keeps it"
    );

    let store = ContentStore::in_dir(dir.path()).sealing();
    assert_eq!(
        store.read(&old).as_deref(),
        Some(TINY_PNG),
        "an old reference reads"
    );
    let resealed = store.reseal(&old).unwrap();
    assert!(is_sealed(&resealed));
    assert!(store.reseal(&resealed).is_none());
    assert_eq!(store.read(&resealed).as_deref(), Some(TINY_PNG));
    assert!(store.reseal(&"0".repeat(64)).is_none(), "content not here");

    // Once nothing names the plain copy, it goes; the sealed one stays.
    let live = BTreeSet::from([resealed.clone()]);
    let collected = store.collect_unreferenced(&live).unwrap();
    assert_eq!(collected.hashes, vec![old.clone()]);
    assert!(!store.has(&old));
    assert_eq!(store.read(&resealed).as_deref(), Some(TINY_PNG));
}

#[test]
fn inputs_persisted_into_a_sealing_store_answer_their_references() {
    let mut inputs = ContentInputs::new();
    let address = inputs.add(TINY_PNG);
    let store = ContentStore::memory().sealing();
    let references = store.put_inputs(&inputs).unwrap();
    let reference = references.get(&address).unwrap();
    assert!(is_sealed(reference));
    assert_eq!(store.read(reference).as_deref(), Some(TINY_PNG));
    assert!(!store.has(&address), "nothing kept in the clear");

    let plain = ContentStore::memory();
    assert_eq!(
        plain.put_inputs(&inputs).unwrap().get(&address),
        Some(&address)
    );
}

#[test]
fn references_parse_strictly() {
    let address = "a".repeat(64);
    assert!(valid_reference(&address));
    assert!(!is_sealed(&address));
    assert!(!valid_reference(&format!("{address}.short")));
    assert!(!valid_reference(&format!(
        "{}.{}",
        "a".repeat(63),
        "A".repeat(43)
    )));
    assert!(valid_reference(&format!("{address}.{}", "A".repeat(43))));
    assert_eq!(
        content_address(&format!("{}.{}", "B".repeat(64), "A".repeat(43))),
        Some("b".repeat(64))
    );
}

#[test]
fn another_device_keeps_sealed_content_exactly_from_its_reference_and_bytes() {
    let picture = [TINY_PNG, &vec![3_u8; 70 * 1024]].concat();
    let reference = ContentStore::memory().sealing().put(&picture).unwrap();
    assert!(names(&reference, &picture));
    assert!(!names(&reference, TINY_PNG), "other bytes");
    assert!(names(&hash_bytes(TINY_PNG), TINY_PNG), "a plain reference");

    let elsewhere = ContentStore::memory();
    elsewhere.put_as(&reference, &picture).unwrap();
    assert!(
        elsewhere.has(content_address(&reference).as_deref().unwrap()),
        "kept under the writer's address"
    );
    assert_eq!(
        elsewhere.read(&reference).as_deref(),
        Some(picture.as_slice())
    );
    assert!(!elsewhere.has(&hash_bytes(&picture)), "never in the clear");
    assert_eq!(
        elsewhere.put_as(&reference, TINY_PNG),
        Err(ContentError::HashMismatch)
    );
}

/// A store whose members hold `secret`.
struct Members<B>(B, tilo_seal::SymmetricKey);

impl<B: ContentBackend> ContentBackend for Members<B> {
    fn put(&self, reader: &mut dyn Read, mime: &str) -> Result<Stored, ContentError> {
        self.0.put(reader, mime)
    }

    fn open(&self, hash: &str) -> Result<Option<ContentReader>, ContentError> {
        self.0.open(hash)
    }

    fn has(&self, hash: &str) -> bool {
        self.0.has(hash)
    }

    fn max_item_bytes(&self) -> usize {
        self.0.max_item_bytes()
    }

    fn scratch_dir(&self) -> Option<std::path::PathBuf> {
        self.0.scratch_dir()
    }

    fn content_secret(&self) -> Option<tilo_seal::SymmetricKey> {
        Some(self.1.clone())
    }
}

/// A reader of `bytes` that adds what it hands out to `read`.
struct Metered<'a> {
    bytes: &'a [u8],
    read: &'a std::cell::Cell<usize>,
}

impl Read for Metered<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.bytes.len().min(buf.len());
        let (handed, rest) = self.bytes.split_at(n);
        buf.get_mut(..n).unwrap_or_default().copy_from_slice(handed);
        self.bytes = rest;
        self.read.set(self.read.get().saturating_add(n));
        Ok(n)
    }
}

#[test]
fn a_sealing_store_refuses_an_oversized_item_at_the_limit() {
    let dir = tempfile::tempdir().unwrap();
    let store = ContentStore::new(Members(
        DirBackend::with_limits(dir.path(), limits(4096, 1 << 20, 16)),
        tilo_seal::SymmetricKey::generate().unwrap(),
    ))
    .sealing();
    let read = std::cell::Cell::new(0);
    let oversized = vec![7_u8; 64 * 1024];
    let mut reader = Metered {
        bytes: &oversized,
        read: &read,
    };
    assert_eq!(
        store.put_reader(&mut reader, "video/mp4"),
        Err(ContentError::ItemTooLarge {
            actual: 4097,
            limit: 4096
        })
    );
    assert_eq!(
        read.get(),
        4097,
        "refused at the limit, not once it has been read and held whole"
    );
    assert!(files_under(dir.path()).is_empty(), "and nothing is kept");
}

#[test]
fn a_large_item_is_sealed_from_a_file_beside_the_store() {
    let secret = tilo_seal::SymmetricKey::generate().unwrap();
    // Past what a sealing store keeps in memory: what it hashes waits in a file beside its own
    // content, and the second pass seals from there.
    let picture = [TINY_PNG, &vec![4_u8; 2 * 1024 * 1024 + 17]].concat();
    let dir = tempfile::tempdir().unwrap();
    let store = ContentStore::new(Members(DirBackend::new(dir.path()), secret.clone())).sealing();
    let read = std::cell::Cell::new(0);
    let mut reader = Metered {
        bytes: &picture,
        read: &read,
    };
    let sealed = store.put_reader(&mut reader, "image/png").unwrap();
    assert_eq!(read.get(), picture.len(), "read once");

    // What sealing the plaintext whole under its key names: the buffered path's reference.
    let mut hash = convergent_hasher(&secret);
    hash.update(&picture);
    let key = tilo_seal::SymmetricKey::from_bytes(*hash.finalize().as_bytes());
    let buffered = tilo_seal::content::seal(&key, &picture).unwrap();
    assert_eq!(content_address(&sealed.hash), Some(hash_bytes(&buffered)));
    assert_eq!(
        store.read(&sealed.hash).as_deref(),
        Some(picture.as_slice()),
        "and it opens as the reference says"
    );
    let leftover = std::fs::read_dir(dir.path())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(".sealing-"))
        .count();
    assert_eq!(leftover, 0, "no spill file is left behind");

    // A store with no directory of its own keeps what it hashes in memory, and names it alike.
    let in_memory = ContentStore::new(Members(MemoryBackend::default(), secret)).sealing();
    let mut again = Metered {
        bytes: &picture,
        read: &read,
    };
    assert_eq!(
        in_memory.put_reader(&mut again, "image/png").unwrap().hash,
        sealed.hash
    );
}

#[test]
fn the_same_bytes_stored_twice_by_members_are_one_sealed_item() {
    let picture = [TINY_PNG, &vec![9_u8; 64 * 1024 + 3]].concat();
    let secret = tilo_seal::SymmetricKey::generate().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = ContentStore::new(Members(DirBackend::new(dir.path()), secret)).sealing();
    let reference = store.put(&picture).unwrap();
    assert!(is_sealed(&reference));
    let kept = files_under(dir.path());
    let mut streamed = picture.as_slice();
    assert_eq!(
        store.put_reader(&mut streamed, "image/png").unwrap().hash,
        reference,
        "the same bytes, put again, are named alike"
    );
    assert_eq!(files_under(dir.path()), kept, "and kept once");
    assert_eq!(store.read(&reference).as_deref(), Some(picture.as_slice()));
    assert!(
        files_under(dir.path())
            .iter()
            .all(|file| !file.windows(64).any(|w| w == &picture[..64])),
        "sealed all the same"
    );
    // A reader with no secret (a device opening the record, a host) reads it
    // by its reference, as any sealed item.
    let reader = ContentStore::in_dir(dir.path());
    assert_eq!(reader.read(&reference).as_deref(), Some(picture.as_slice()));
    // A reference an earlier writer sealed under a fresh key still opens.
    let fresh = reader.sealing().put(&picture).unwrap();
    assert_ne!(fresh, reference);
    assert_eq!(store.read(&fresh).as_deref(), Some(picture.as_slice()));
    assert!(names(&reference, &picture));

    let elsewhere = tempfile::tempdir().unwrap();
    let other = ContentStore::new(Members(
        DirBackend::new(elsewhere.path()),
        tilo_seal::SymmetricKey::generate().unwrap(),
    ))
    .sealing();
    let theirs = other.put(&picture).unwrap();
    assert_ne!(
        content_address(&theirs),
        content_address(&reference),
        "members of another workspace seal the same bytes to other ciphertext"
    );
    assert_ne!(
        other.read(content_address(&theirs).unwrap().as_str()),
        store.read(content_address(&reference).unwrap().as_str())
    );
}
