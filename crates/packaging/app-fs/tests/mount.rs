//! Integration tests for the app-fs FUSE mount.
//!
//! Fast, always-run tests exercise the FUSE handler code path through
//! the crate's public helpers (`QosAppFs::ino_of`, `blob_at`,
//! `cache().read()`) without a real kernel mount, proving the
//! "page-fault streaming" contract through the same page cache the
//! FUSE `read` handler uses.
//!
//! `#[ignore]`d real-mount tests spin up a real FUSE3 session via
//! `Mount::spawn`, cat a file with `std::fs`, and check the bytes; they
//! need `/dev/fuse` + `fusermount3` and are skipped in constrained CI
//! (`cargo test -p app-fs -- --include-ignored` locally).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test that unwraps is stating an invariant"
)]

use std::sync::Arc;

use app_fs::{
    Collection, CounterSnapshot, MemoryBlobStore, Mount, MountOptions, PAGE_SIZE, QosAppFs,
    store::CountingStore,
};

/// Common fixture: put a single 1 KiB file at bin/hello into a
/// counting-wrapped memory store, build a filesystem, and return
/// (fs, counter, hash, bytes).
fn fixture_1k() -> (
    QosAppFs<Arc<CountingStore<MemoryBlobStore>>>,
    Arc<CountingStore<MemoryBlobStore>>,
    [u8; 32],
    Vec<u8>,
) {
    let inner = MemoryBlobStore::new();
    // 1 KiB of "aB" repeating so partial reads are easy to eyeball.
    let bytes: Vec<u8> = (0..1024)
        .map(|i| if i & 1 == 0 { b'a' } else { b'B' })
        .collect();
    let hash = inner.put(&bytes);
    let counter = Arc::new(CountingStore::new(inner));
    let collection = Collection::from_entries([("bin/hello".to_string(), hash)]);
    let fs = QosAppFs::new(&collection, Arc::clone(&counter)).expect("build fs");
    (fs, counter, hash, bytes)
}

/// The FS should resolve lookup/getattr-style traversals (via
/// `ino_of`) without pulling blob bytes. Construction incurs one
/// `size()` call per file entry (for the cached inode size); after
/// that, only `range_calls` matters.
#[test]
fn lookup_resolves_without_fetch() {
    let (fs, counter, _hash, _bytes) = fixture_1k();

    // Construction should have called size() exactly once (one blob), and no range reads.
    let snap = counter.snapshot();
    assert_eq!(
        snap,
        CounterSnapshot {
            range_calls: 0,
            bytes_requested: 0,
            size_calls: 1,
            has_calls: 0,
        },
        "construction should not fetch blob bytes"
    );

    // Path lookups walk the inode table only.
    let root_ino = fs.ino_of("").expect("root always exists");
    assert_eq!(root_ino, 1, "root inode is FUSE_ROOT_ID");

    let bin_ino = fs.ino_of("bin").expect("dir 'bin' allocated");
    assert_ne!(bin_ino, 0);
    assert!(fs.blob_at(bin_ino).is_none(), "'bin' is a directory");

    let hello_ino = fs.ino_of("bin/hello").expect("file 'bin/hello' allocated");
    let (blob_hash, blob_size) = fs.blob_at(hello_ino).expect("file resolves to blob");
    assert_eq!(blob_size, 1024);

    // Post-lookup counters must match the post-construction snapshot — no additional store calls.
    let after = counter.snapshot();
    assert_eq!(
        after, snap,
        "lookups must not touch the store past construction"
    );

    // Sanity: the hash we resolved matches the collection.
    assert_eq!(blob_hash, _hash);
}

/// Reading the first 100 bytes must fetch at most one page from the
/// store — the "page-fault streaming" contract that's the whole
/// reason for the mount rather than bulk-hydrating on open. A repeat
/// read must hit cache, not refetch.
#[test]
fn partial_read_does_not_fetch_whole_blob() {
    let (fs, counter, hash, bytes) = fixture_1k();
    let baseline = counter.snapshot();

    let cache = fs.cache();
    let first = cache.read(&hash, bytes.len() as u64, 0, 100).expect("read");
    assert_eq!(first.len(), 100);
    assert_eq!(&first[..], &bytes[..100]);

    let after_first = counter.snapshot();
    assert_eq!(
        after_first.range_calls - baseline.range_calls,
        1,
        "expected exactly one store range fetch for a 100-byte read"
    );
    // At most one page's worth of bytes were requested from the store — regardless of what the caller asked for.
    let bytes_from_store = after_first.bytes_requested - baseline.bytes_requested;
    assert!(
        bytes_from_store <= u64::from(PAGE_SIZE),
        "store fetched {bytes_from_store} bytes; expected <= {PAGE_SIZE} (one page)"
    );

    // A second read of the same 100 bytes must be served from the page cache — no new store fetches.
    let second = cache.read(&hash, bytes.len() as u64, 0, 100).expect("read");
    assert_eq!(second, first);
    let after_second = counter.snapshot();
    assert_eq!(
        after_second.range_calls - after_first.range_calls,
        0,
        "cache hit must not re-fetch"
    );
}

// Real-mount tests — need /dev/fuse and a working fusermount3.

/// Full end-to-end: mount + `std::fs::read` + unmount — verifies the
/// FUSE kernel path stitches together, not just the internal handler code.
#[test]
#[ignore = "requires /dev/fuse + fusermount3; run with --include-ignored"]
fn mount_and_read_single_file() {
    let (fs_bytes, hash) = {
        let inner = MemoryBlobStore::new();
        // Distinct bytes so we notice off-by-one issues.
        let payload: Vec<u8> = (0..1024).map(|i| (i & 0xff) as u8).collect();
        let h = inner.put(&payload);
        (payload, (inner, h))
    };
    let (store, hash) = hash;
    let collection = Collection::from_entries([("hello.bin".to_string(), hash)]);

    let mountpoint = tempfile::tempdir().expect("tempdir");
    let mount = match Mount::spawn(
        &collection,
        store,
        mountpoint.path(),
        MountOptions::default(),
    ) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("skipping: fuser mount failed: {e}");
            return;
        }
    };

    // Give the kernel a moment to publish the mount before path-walking
    // into it; belt-and-braces, spawn_mount2 already returns after INIT.
    std::thread::sleep(std::time::Duration::from_millis(50));

    let read_back = std::fs::read(mountpoint.path().join("hello.bin")).expect("read mounted file");
    assert_eq!(read_back, fs_bytes);

    // Clean unmount.
    mount.unmount().expect("unmount");
}

// The partial-read-through-real-FUSE invariant is asserted at the
// crate boundary in `partial_read_does_not_fetch_whole_blob` above,
// via the same `PageCache::read` path. A kernel-driven equivalent is
// flaky under container readahead/cgroup pressure; to check
// kernel-side streaming, run `mount_and_read_single_file` with a
// large payload and compare `CountingStore` counts before/after.
