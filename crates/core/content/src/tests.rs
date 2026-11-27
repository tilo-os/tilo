#![allow(
    clippy::string_slice,
    reason = "in a test these are the assertion, not a hazard"
)]

//! The content store as its callers drive it: a workspace keeping what people import, a store
//! that refuses, repairs and collects, and sealed content shared between members' devices.

use super::*;
use std::io::Read;
use std::time::{Duration, SystemTime};

const TINY_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
    0x42, 0x60, 0x82,
];

const WORKSPACE: &str = "6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b";
const OTHER_WORKSPACE: &str = "00000000-0000-4000-8000-000000000000";

fn limits(item: usize, total: usize, entries: usize) -> ContentLimits {
    ContentLimits {
        max_item_bytes: item,
        max_total_bytes: total,
        max_entries: entries,
        max_scan_entries: 64,
    }
}

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

/// Bytes that differ at every position a part could begin.
fn varied(len: usize) -> Vec<u8> {
    (0..len)
        .map(|n| u8::try_from(n.wrapping_mul(7).wrapping_add(n / 251) % 256).unwrap_or(0))
        .collect()
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("tilo-content-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// A store whose items are sealed under the workspace member's key: what the shell hands the
/// sync so a device gets ciphertext, never the file itself.
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

    fn spill(&self) -> std::io::Result<Option<Box<dyn crate::SpillFile>>> {
        self.0.spill()
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

/// A person imports files into a workspace: the inputs they picked are deduplicated and kept,
/// the workspace keeps its own content and answers for no other's, and collection lets go of
/// exactly what no record names, once its writer has had time to name it.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one journey through a workspace's store, kept together so each later check reuses what the person already did"
)]
fn a_workspace_keeps_what_people_import_and_lets_go_of_nothing_else() {
    // The picker's inputs: the same picture picked twice is one item.
    let mut inputs = ContentInputs::new();
    let address = inputs.add(TINY_PNG.to_vec());
    assert_eq!(inputs.add(TINY_PNG), address, "one address for one file");
    assert_eq!(inputs.content(&address).as_deref(), Some(TINY_PNG));

    let dir = scratch("workspace");
    let stores = WorkspaceStores::in_dir(&dir, ContentLimits::default());
    let store = stores.get(WORKSPACE).unwrap();
    let references = store.put_inputs(&inputs).unwrap();
    assert!(store.has(references.get(&address).unwrap()), "kept");
    let chain = (NoContent, &store);
    assert_eq!(chain.content(&address).as_deref(), Some(TINY_PNG));

    // A workspace is one store however its id is written; another's is another store.
    let written = store.put(b"the report").unwrap();
    assert_eq!(
        stores
            .get(&WORKSPACE.to_uppercase().replace('-', ""))
            .unwrap()
            .read(&written),
        Some(b"the report".to_vec()),
        "a workspace is one store however its id is written"
    );
    let other = stores.get(OTHER_WORKSPACE).unwrap();
    assert_eq!(other.read(&written), None, "no other workspace answers");
    assert!(stores.get("../elsewhere").is_err());
    assert!(stores.get("").is_err());
    assert!(other.put(b"twelve bytes").is_ok(), "its own room");

    // Collection spares what was written since, lets go of what no record names, and keeps
    // what a record does name.
    let collecting = WorkspaceStores::in_dir(dir.join("collecting"), ContentLimits::default());
    let store = collecting.get(WORKSPACE).unwrap();
    let named = store.put(b"a file the tree names").unwrap();
    let replaced = store.put(b"the version it replaced").unwrap();
    let live = BTreeSet::from([named.clone()]);
    let long_ago = SystemTime::UNIX_EPOCH;
    assert!(
        collecting
            .collect(WORKSPACE, &live, long_ago)
            .unwrap()
            .hashes
            .is_empty(),
        "content written since is spared: its writer may not have named it yet"
    );
    let later = SystemTime::now() + std::time::Duration::from_secs(60);
    let collected = collecting.collect(WORKSPACE, &live, later).unwrap();
    assert_eq!(collected.hashes, std::slice::from_ref(&replaced));
    assert!(store.has(&named), "what the workspace names stays");
    assert!(!store.has(&replaced));
    assert_eq!(collecting.workspaces(), [WORKSPACE]);

    // Writing content already here is a fresh write, so a record may name it soon.
    let again = store.put(b"uploaded again").unwrap();
    std::fs::File::options()
        .append(true)
        .open(item_path(&dir.join("collecting").join(WORKSPACE), &again))
        .unwrap()
        .set_modified(SystemTime::UNIX_EPOCH)
        .unwrap();
    let a_while_ago = SystemTime::now() - std::time::Duration::from_secs(60);
    assert_eq!(store.put(b"uploaded again").unwrap(), again);
    assert!(
        collecting
            .collect(WORKSPACE, &BTreeSet::new(), a_while_ago)
            .unwrap()
            .hashes
            .is_empty(),
        "a rewrite spares it from collection"
    );

    // A workspace is bounded by its own limits, and a page's upload outlives it.
    let tight = WorkspaceStores::in_dir(dir.join("tight"), limits(1 << 20, 24, 1024));
    let packed = tight.get(WORKSPACE).unwrap();
    packed.put(b"twelve bytes").unwrap();
    assert!(packed.put(b"and twelve more").is_err());
    assert!(
        tight
            .get(OTHER_WORKSPACE)
            .unwrap()
            .put(b"twelve bytes")
            .is_ok()
    );
    let app = ContentStore::new(FilesBackend::new(
        tilo_paths::AppPaths::memory("so.tilo.test").data(),
        ContentLimits::default(),
    ));
    let upload = app.put(b"a photo an unsent draft names").unwrap();
    let reopened = ContentStore::new(FilesBackend::new(
        tilo_paths::AppPaths::memory("so.tilo.test").data(),
        ContentLimits::default(),
    ));
    assert_eq!(
        reopened.read(&upload).as_deref(),
        Some(&b"a photo an unsent draft names"[..]),
        "the upload outlives the page that made it"
    );
    assert!(
        reopened
            .collect_unreferenced(&BTreeSet::from([upload.clone()]))
            .unwrap()
            .hashes
            .is_empty(),
        "what a record names stays"
    );
    assert_eq!(
        reopened
            .collect_unreferenced(&BTreeSet::new())
            .unwrap()
            .hashes,
        vec![upload.clone()]
    );
    assert!(!reopened.has(&upload));

    // An app's files store counts its quota and collects what no record names.
    let tight_files = ContentStore::new(FilesBackend::new(
        tilo_paths::AppPaths::memory("so.tilo.quota").data(),
        limits(1 << 20, 8, 1024),
    ));
    let kept = tight_files.put(b"12345").unwrap();
    assert!(matches!(
        tight_files.put(b"67890"),
        Err(ContentError::TotalQuotaExceeded { .. })
    ));
    assert_eq!(
        tight_files
            .collect_unreferenced(&BTreeSet::new())
            .expect("a files store collects")
            .hashes,
        vec![kept]
    );
    assert!(tight_files.put(b"67890").is_ok());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Stores keep addressing the same bytes, refuse past their limits without evicting, repair a
/// torn item on the next write, and are told what content is by its first bytes.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one journey through a store: keep, stream, refuse, repair, collect and sniff, sharing one fixture"
)]
fn stores_keep_refuse_and_repair_what_people_and_peers_put_in_them() {
    let dir = tempfile::tempdir().unwrap();
    for store in [ContentStore::memory(), ContentStore::in_dir(dir.path())] {
        let hash = store.put(TINY_PNG).unwrap();
        assert_eq!(hash, hash_bytes(TINY_PNG));
        assert_eq!(store.put(TINY_PNG).unwrap(), hash, "one address, put twice");
        assert_eq!(store.read(&hash).as_deref(), Some(TINY_PNG));
        assert!(store.has(&hash));
        assert_eq!(store.state(&hash), ContentState::Resolved);
        let missing = hash_bytes(b"never stored");
        assert_eq!(store.read(&missing), None);
        assert_eq!(store.state(&missing), ContentState::Unavailable);

        // A store is a handle: clones share it, separate stores share nothing.
        let clone = store.clone();
        assert!(clone.has(&hash));
        assert!(!ContentStore::memory().has(&hash));

        // Read and write stream, whatever the size.
        let big: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let stored = store
            .put_reader(&mut big.as_slice(), "application/octet-stream")
            .unwrap();
        assert_eq!(stored.hash, hash_bytes(&big));
        assert_eq!(stored.len, big.len() as u64);
        let mut back = Vec::new();
        store
            .open(&stored.hash)
            .unwrap()
            .read_to_end(&mut back)
            .unwrap();
        assert_eq!(back, big);

        // An address reaches here from documents and peers: nothing else is a name.
        assert!(store.read("../../etc/passwd").is_none());
        assert!(!store.has("../escape"));

        // A batch answers once per item, in order, and putting the set again is a no-op.
        let items: Vec<Vec<u8>> = (0..40).map(|n| format!("item {n}").into_bytes()).collect();
        let refs: Vec<&[u8]> = items.iter().map(Vec::as_slice).collect();
        let kept = store.put_public_many(&refs);
        assert_eq!(kept.len(), items.len(), "one answer for each item");
        for (item, kept) in items.iter().zip(&kept) {
            assert_eq!(store.read(kept.as_ref().unwrap()), Some(item.clone()));
        }
        assert!(store.put_public_many(&refs).iter().all(Result::is_ok));
    }

    // A torn item is never observable, and the next write of the right bytes repairs it.
    let dir = tempfile::tempdir().unwrap();
    let hash = hash_bytes(b"good");
    std::fs::create_dir_all(dir.path().join(&hash[..2])).unwrap();
    std::fs::write(item_path(dir.path(), &hash), b"corrupt").unwrap();
    std::fs::write(dir.path().join(".incoming-dead-0"), b"partial").unwrap();
    let store = ContentStore::new(DirBackend::with_limits(dir.path(), limits(8, 16, 4)));
    assert!(store.read(&hash).is_none(), "a verifying read refuses it");
    assert!(!store.has(&hash));
    let mut sink = Vec::new();
    assert_eq!(
        store
            .open(&hash)
            .unwrap()
            .read_to_end(&mut sink)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::InvalidData,
        "a streaming read fails at the end"
    );
    assert_eq!(store.put(b"good").unwrap(), hash);
    assert_eq!(store.read(&hash).as_deref(), Some(b"good".as_slice()));

    // A file changed behind the store's back is verified again.
    let hash = store.put(b"original").unwrap();
    let path = item_path(dir.path(), &hash);
    let before = std::fs::metadata(&path).unwrap().modified().unwrap();
    std::fs::write(&path, b"tampered").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(before.checked_add(Duration::from_secs(1)).unwrap())
        .unwrap();
    assert!(!store.has(&hash), "a changed file is verified again");
    assert_eq!(store.state(&hash), ContentState::Unavailable);
    std::fs::write(dir.path().join("replacement"), b"original").unwrap();
    std::fs::rename(dir.path().join("replacement"), &path).unwrap();
    assert!(
        store.has(&hash),
        "the right bytes under the name are intact"
    );

    // Quotas hold across a restart and refuse without evicting what is kept.
    let dir = tempfile::tempdir().unwrap();
    {
        let s = ContentStore::new(DirBackend::with_limits(dir.path(), limits(4, 6, 4)));
        s.put(b"abc").unwrap();
        s.put(b"de").unwrap();
    }
    let s = ContentStore::new(DirBackend::with_limits(dir.path(), limits(4, 6, 4)));
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

    // Two writers racing one root cannot both take the last room.
    let dir = tempfile::tempdir().unwrap();
    let first = ContentStore::new(DirBackend::with_limits(dir.path(), limits(4, 6, 4)));
    let second = ContentStore::new(DirBackend::with_limits(dir.path(), limits(4, 6, 4)));
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

    // An index that over-counts after an external deletion is rescanned before it refuses.
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = probed(DirBackend::with_limits(dir.path(), limits(4, 6, 4)));
    let abc = store.put(b"abc").unwrap();
    store.put(b"de").unwrap();
    std::fs::remove_file(item_path(dir.path(), &abc)).unwrap();
    assert_eq!(
        store.put(b"fgh"),
        Ok(hash_bytes(b"fgh")),
        "the over-counting index is rescanned rather than refusing"
    );
    assert_eq!(
        store.put(b"ij"),
        Err(ContentError::TotalQuotaExceeded {
            attempted: 7,
            limit: 6
        })
    );

    // An earlier layout still reads, and the session's shared store wins where there is one.
    let old = tempfile::tempdir().unwrap();
    let new = tempfile::tempdir().unwrap();
    ContentStore::in_dir(old.path()).put(TINY_PNG).unwrap();
    let store = ContentStore::new(DirBackend::new(new.path()).reading_also(old.path()));
    assert_eq!(store.read(&hash_bytes(TINY_PNG)).as_deref(), Some(TINY_PNG));
    let shared = tempfile::tempdir().unwrap();
    let path = shared.path().to_path_buf();
    let store = ContentStore::for_app_with(&|name| {
        (name == SHARED_STORE_ENV).then(|| path.clone().into_os_string())
    });
    let hash = store.put(b"shared").unwrap();
    assert_eq!(
        ContentStore::in_dir(shared.path()).read(&hash).as_deref(),
        Some(b"shared".as_slice())
    );

    // Collection keeps exactly the live set.
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

    // Durable content waits for the disk that gained it; a cache does not pay for that.
    let dir = tempfile::tempdir().unwrap();
    let (durable, backend) = probed(DirBackend::new(dir.path()));
    durable.put(b"first").unwrap();
    assert!(
        count(&backend.probes().dir_syncs) > 0,
        "durable content syncs the directory that gained it"
    );
    let (dirs, files) = (
        count(&backend.probes().dir_syncs),
        count(&backend.probes().file_syncs),
    );
    assert!(files > 0, "durable content syncs what it gained");
    durable.put(b"first").unwrap();
    assert_eq!(
        (
            count(&backend.probes().dir_syncs),
            count(&backend.probes().file_syncs)
        ),
        (dirs, files),
        "content already kept is put again without waiting on the disk"
    );
    let cache = tempfile::tempdir().unwrap();
    let (cache, cache_backend) = probed(DirBackend::new(cache.path()).regenerable());
    cache.put(b"first").unwrap();
    assert_eq!(
        count(&cache_backend.probes().dir_syncs),
        0,
        "regenerable content does not pay for a sync"
    );

    // Content describes itself from its first bytes, or says only how long it is.
    let m = sniff(TINY_PNG);
    assert_eq!(m.mime, "image/png");
    assert_eq!((m.w, m.h), (1, 1));
    assert_eq!(m.len, TINY_PNG.len() as u64);
    let m = sniff(b"not an image at all");
    assert_eq!(m.mime, "");
    assert_eq!((m.w, m.h), (0, 0));
    assert_eq!(m.len, 19, "length is knowable even when the format is not");
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

/// Members' devices seal the same bytes to one item under their key; a reader with the
/// reference opens it, a store without the secret still serves it, and a large item waits in a
/// file beside the store rather than in memory.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one journey through sealing: put, reseal, read elsewhere, spill, parts, all on one store"
)]
fn sealed_content_is_one_item_for_members_and_reads_by_reference_elsewhere() {
    let dir = tempfile::tempdir().unwrap();
    let store = ContentStore::in_dir(dir.path()).sealing();
    let picture = [TINY_PNG, &vec![7_u8; 3 * 64 * 1024 + 5]].concat();
    let reference = store.put(&picture).unwrap();
    assert!(is_sealed(&reference));
    assert_eq!(store.read(&reference).as_deref(), Some(picture.as_slice()));
    assert!(names(&reference, &picture));
    assert!(!names(&reference, TINY_PNG), "other bytes");
    assert_eq!(store.size(&reference), Some(picture.len() as u64));

    // A piece reads from the chunk it is in, whatever the offset.
    for (from, len) in [(0, 10), (65_530, 20), (3 * 65_536, 200), (5, 0)] {
        let (size, piece) = store.read_range(&reference, from, len).unwrap();
        assert_eq!(size, picture.len() as u64);
        let start = usize::try_from(from).unwrap();
        let end = (start + usize::try_from(len).unwrap()).min(picture.len());
        assert_eq!(piece, picture[start..end], "{from}+{len}");
    }

    // Content kept in the clear before sealing reads, is resealed, and the clear copy goes.
    let plain = ContentStore::in_dir(dir.path());
    let old = plain.put(TINY_PNG).unwrap();
    assert_eq!(old, hash_bytes(TINY_PNG));
    assert!(
        plain.reseal(&old).is_none(),
        "a store that does not seal keeps it"
    );
    assert_eq!(
        store.read(&old).as_deref(),
        Some(TINY_PNG),
        "an old reference reads"
    );
    let resealed = store.reseal(&old).unwrap();
    assert!(is_sealed(&resealed));
    assert!(store.reseal(&resealed).is_none());
    assert!(store.reseal(&"0".repeat(64)).is_none(), "content not here");
    assert_eq!(
        store
            .collect_unreferenced(&BTreeSet::from([resealed, reference]))
            .unwrap()
            .hashes,
        std::slice::from_ref(&old)
    );
    assert!(!store.has(&old));

    // The picker's inputs sealed on their way in keep nothing in the clear.
    let mut inputs = ContentInputs::new();
    let address = inputs.add(TINY_PNG);
    let sealed = ContentStore::memory().sealing();
    let references = sealed.put_inputs(&inputs).unwrap();
    assert!(is_sealed(references.get(&address).unwrap()));
    assert_eq!(
        sealed.read(references.get(&address).unwrap()).as_deref(),
        Some(TINY_PNG)
    );
    assert!(!sealed.has(&address), "nothing kept in the clear");
    assert_eq!(
        ContentStore::memory()
            .put_inputs(&inputs)
            .unwrap()
            .get(&address),
        Some(&address)
    );

    // A reference is an address, or an address and a key, and nothing else is one.
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

    // Another device keeps the writer's reference from the reference and the bytes alone.
    let picture = [TINY_PNG, &vec![3_u8; 70 * 1024]].concat();
    let reference = ContentStore::memory().sealing().put(&picture).unwrap();
    assert!(names(&reference, &picture));
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

    // Past the item limit the read stops at the limit, not after holding it whole.
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
    assert_eq!(read.get(), 4097, "refused at the limit");
    assert!(files_under(dir.path()).is_empty(), "and nothing is kept");

    // Past what a store keeps in memory, the item waits in a file beside it and seals from
    // there, leaving nothing behind.
    let secret = tilo_seal::SymmetricKey::generate().unwrap();
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
    let mut hash = convergent_hasher(&secret);
    hash.update(&picture);
    let key = tilo_seal::SymmetricKey::from_bytes(*hash.finalize().as_bytes());
    let buffered = tilo_seal::content::seal(&key, &picture).unwrap();
    assert_eq!(content_address(&sealed.hash), Some(hash_bytes(&buffered)));
    assert_eq!(
        store.read(&sealed.hash).as_deref(),
        Some(picture.as_slice())
    );
    assert_eq!(
        std::fs::read_dir(dir.path())
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(".sealing-"))
            .count(),
        0,
        "no spill file is left behind"
    );
    let in_memory = ContentStore::new(Members(MemoryBackend::default(), secret)).sealing();
    let mut again = Metered {
        bytes: &picture,
        read: &read,
    };
    assert_eq!(
        in_memory.put_reader(&mut again, "image/png").unwrap().hash,
        sealed.hash
    );

    // Members of one workspace seal the same bytes to one item, kept once; another workspace's
    // members seal them to other ciphertext; a reader with only the reference opens it.
    let picture = [TINY_PNG, &vec![9_u8; 64 * 1024 + 3]].concat();
    let dir = tempfile::tempdir().unwrap();
    let store = ContentStore::new(Members(
        DirBackend::new(dir.path()),
        tilo_seal::SymmetricKey::generate().unwrap(),
    ))
    .sealing();
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
    assert!(
        files_under(dir.path())
            .iter()
            .all(|file| !file.windows(64).any(|w| w == &picture[..64])),
        "sealed all the same"
    );
    let reader = ContentStore::in_dir(dir.path());
    assert_eq!(reader.read(&reference).as_deref(), Some(picture.as_slice()));
    let fresh = reader.sealing().put(&picture).unwrap();
    assert_ne!(fresh, reference);
    assert_eq!(store.read(&fresh).as_deref(), Some(picture.as_slice()));
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

    // Content of any size is kept in parts and reads whole; a piece reads only its parts.
    let big = limits(64 << 20, 1 << 30, 64);
    for store in [
        ContentStore::new(MemoryBackend::new(big)),
        ContentStore::new(MemoryBackend::new(big)).sealing(),
    ] {
        let bytes = varied(2 * PART_BYTES + 12_345);
        let stored = store.put_parts(&mut bytes.as_slice()).unwrap();
        assert_eq!(stored.len, bytes.len() as u64);
        assert_eq!(store.size(&stored.hash), Some(bytes.len() as u64));
        assert!(store.read(&stored.hash).unwrap() == bytes, "read whole");
        let from = PART_BYTES as u64 - 10;
        let (size, piece) = store.read_range(&stored.hash, from, 30).unwrap();
        assert_eq!(size, bytes.len() as u64);
        assert_eq!(piece, bytes[PART_BYTES - 10..PART_BYTES + 20]);
        let (_, tail) = store
            .read_range(&stored.hash, bytes.len() as u64 - 5, 100)
            .unwrap();
        assert_eq!(tail, bytes[bytes.len() - 5..]);
        let small = store.put_parts(&mut &bytes[..1000]).unwrap();
        assert_eq!(store.read(&small.hash).unwrap(), bytes[..1000]);
        let exact = store.put_parts(&mut &bytes[..PART_BYTES]).unwrap();
        assert!(
            store.read(&exact.hash).unwrap() == bytes[..PART_BYTES],
            "one whole part"
        );
        let empty = store.put_parts(&mut &[][..]).unwrap();
        assert_eq!(store.read(&empty.hash).unwrap(), Vec::<u8>::new());
    }

    // A part missing leaves the whole not here; a file that begins like a list is itself.
    let whole = ContentStore::new(MemoryBackend::new(limits(64 << 20, 1 << 30, 64)));
    let bytes = varied(PART_BYTES + 3);
    let stored = whole.put_parts(&mut bytes.as_slice()).unwrap();
    let other = ContentStore::new(MemoryBackend::new(limits(64 << 20, 1 << 30, 64)));
    other.put(&whole.read_item_whole(&stored.hash)).unwrap();
    assert!(other.read(&stored.hash).is_none(), "a part is missing");
    let store = ContentStore::new(MemoryBackend::new(limits(1 << 20, 1 << 22, 64)));
    let mut bytes = b"TCL\x01".to_vec();
    bytes.extend_from_slice(&[2, 0, 0, 0]);
    bytes.extend_from_slice(&varied(500));
    let reference = store.put(&bytes).unwrap();
    assert_eq!(store.read(&reference).unwrap(), bytes);
    assert_eq!(store.size(&reference), Some(bytes.len() as u64));
}
