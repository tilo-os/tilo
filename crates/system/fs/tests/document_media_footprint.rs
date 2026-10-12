//! Daemon heap a document-media read costs on a cold store.
//!
//! An app reads media in ranges of at most [`MAX_READ_CHUNK`], and asks
//! whether content is readable with an empty range. Each of those must cost
//! the daemon the range, not the attachment: a status check reads no data,
//! a range near the end of the largest attachment holds one range, and a
//! streamed read of the whole attachment never holds more than one range at
//! a time. The store is reopened before measuring, as after a daemon
//! restart, so nothing is served from the blob cache.
//!
//! The figure is the process-wide peak of live heap bytes above what was
//! live when the read began, counted by [`PeakCounting`]: the store's actors
//! run on threads of their own, so a per-thread count would miss them. The
//! one test runs alone in this binary, so no other test's allocations land
//! in the figure.

#![allow(
    clippy::unwrap_used,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use fs::ContentStore;
use fs::document_media::DocumentMedia;
use runtime_fs::blobs::{BlobScope, MAX_BLOB_BYTES, MAX_READ_CHUNK, ReadOutcome};

/// The system allocator, counting live heap bytes across every thread and
/// the peak since the last [`reset_peak`].
struct PeakCounting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grew(bytes: usize) {
    let live = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK.fetch_max(live, Ordering::Relaxed);
}

fn shrank(bytes: usize) {
    LIVE.fetch_sub(bytes, Ordering::Relaxed);
}

// SAFETY: every call forwards to the system allocator with the caller's
// arguments; the counters only record sizes.
unsafe impl GlobalAlloc for PeakCounting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract, which `System` shares.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            grew(layout.size());
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: as for `alloc`.
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            grew(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        shrank(layout.size());
        // SAFETY: `pointer` came from this allocator, i.e. from `System`, with this layout.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: as for `dealloc`, and the caller upholds `realloc`'s size contract.
        let new = unsafe { System.realloc(pointer, layout, new_size) };
        if !new.is_null() {
            grew(new_size);
            shrank(layout.size());
        }
        new
    }
}

#[global_allocator]
static HEAP: PeakCounting = PeakCounting;

/// Live heap bytes now; the peak starts again from here.
fn reset_peak() -> usize {
    let live = LIVE.load(Ordering::Relaxed);
    PEAK.store(live, Ordering::Relaxed);
    live
}

/// Peak live heap bytes above `base` since [`reset_peak`] returned it.
fn peak_above(base: usize) -> usize {
    PEAK.load(Ordering::Relaxed).saturating_sub(base)
}

/// Daemon heap an empty-range status read may hold: a tag lookup, a status
/// query and the store actors' bookkeeping for them, no blob data. Measured
/// at 1.4 KiB (debug build, macOS); before ranged reads it was 128 MiB, the
/// whole attachment twice over while it was collected.
const STATUS_READ_PEAK: usize = 64 * 1024;

/// Daemon heap one range may hold beyond the range it returns: the 16 KiB
/// chunk groups in flight between the store actor and the reader, the
/// outboard pages that verify them, and the store actors' bookkeeping.
/// Measured at 0.07 MiB to 0.19 MiB over the 4 MiB range.
const RANGE_READ_OVERHEAD: usize = 1024 * 1024;

fn deterministic_bytes(len: usize) -> Vec<u8> {
    let mut state = 0x2545_f491_4f6c_dd1d_u64;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes().first().copied().unwrap_or_default()
        })
        .collect()
}

fn ms(elapsed: Duration) -> f64 {
    elapsed.as_secs_f64() * 1_000.0
}

fn kib(bytes: usize) -> f64 {
    f64::from(u32::try_from(bytes).unwrap_or(u32::MAX)) / 1024.0
}

fn mib(bytes: usize) -> f64 {
    kib(bytes) / 1024.0
}

/// What `run` returns, how long it took and the peak heap above what was
/// live when it began.
async fn measured<T>(run: impl Future<Output = T>) -> (T, Duration, usize) {
    let base = reset_peak();
    let started = Instant::now();
    let value = run.await;
    (value, started.elapsed(), peak_above(base))
}

/// The hash of the whole of `hash`'s content, read a range at a time as
/// `BlobReader` reads it, each range consumed before the next is asked for;
/// `None` when a range is unreadable or empty before the end.
async fn stream_whole(
    media: &DocumentMedia,
    scope: &BlobScope,
    hash: iroh_blobs::Hash,
    total_len: u64,
) -> Option<blake3::Hash> {
    let mut hasher = blake3::Hasher::new();
    let mut offset = 0_u64;
    while offset < total_len {
        let ReadOutcome::Ready { bytes, .. } = media
            .read_range(scope.clone(), hash, offset, MAX_READ_CHUNK)
            .await
        else {
            return None;
        };
        if bytes.is_empty() {
            return None;
        }
        hasher.update(&bytes);
        offset += bytes.len() as u64;
    }
    Some(hasher.finalize())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cold_document_media_reads_hold_one_range_of_the_largest_attachment() {
    let dir = tempfile::tempdir().unwrap();
    let photos = BlobScope::new("room-1", "so.tilo.photos").unwrap();
    let len = usize::try_from(MAX_BLOB_BYTES).unwrap();
    let chunk = MAX_READ_CHUNK as usize;
    let data = deterministic_bytes(len);
    // Near the end, and not on a 16 KiB chunk-group boundary.
    let tail_offset = len - chunk - 12_345;
    let tail = data[tail_offset..tail_offset + chunk].to_vec();
    let hash = {
        let store = Arc::new(ContentStore::open(dir.path()).await.unwrap());
        let media = DocumentMedia::new(Arc::clone(&store));
        let hash = media.store(&photos, data).await.unwrap();
        store.store().shutdown().await.unwrap();
        hash
    };
    let store = Arc::new(ContentStore::open(dir.path()).await.unwrap());
    let media = DocumentMedia::new(Arc::clone(&store));
    let total_len = MAX_BLOB_BYTES;

    // What `state()` sends: an empty range.
    let (status, status_time, status_peak) =
        measured(media.read_range(photos.clone(), hash, 0, 0)).await;
    assert_eq!(
        status,
        ReadOutcome::Ready {
            bytes: Vec::new(),
            total_len
        }
    );

    // One range near the end, as a seek to the end of a video asks for.
    let (read, tail_time, tail_peak) =
        measured(media.read_range(photos.clone(), hash, tail_offset as u64, MAX_READ_CHUNK)).await;
    assert_eq!(
        read,
        ReadOutcome::Ready {
            bytes: tail,
            total_len
        }
    );
    drop(read);

    // The whole attachment, streamed a range at a time.
    let (streamed, stream_time, stream_peak) =
        measured(stream_whole(&media, &photos, hash, total_len)).await;
    assert_eq!(
        streamed.as_ref().map(blake3::Hash::as_bytes),
        Some(hash.as_bytes())
    );

    // The same tail range the way reads were answered before: the whole
    // blob loaded, verified and cached, then sliced. This is also the probe's
    // own check: a counter that cannot see a whole-blob load must fail here
    // rather than pass the bounds above. It also fails if any read above left
    // the blob in the cache, since this load would then allocate nothing.
    let ((), whole_time, whole_peak) = measured(async {
        let whole = store.local(hash).await.unwrap();
        drop(whole[tail_offset..tail_offset + chunk].to_vec());
    })
    .await;

    eprintln!(
        "document media, cold {:.0} MiB attachment: status {:.3} ms / {:.1} KiB; \
         4 MiB tail range {:.3} ms / {:.2} MiB; streamed whole {:.3} ms / {:.2} MiB peak; \
         whole-blob load of the tail range {:.3} ms / {:.2} MiB",
        mib(len),
        ms(status_time),
        kib(status_peak),
        ms(tail_time),
        mib(tail_peak),
        ms(stream_time),
        mib(stream_peak),
        ms(whole_time),
        mib(whole_peak),
    );
    assert!(
        status_peak <= STATUS_READ_PEAK,
        "an empty-range status read held {status_peak} B of daemon heap (budget {STATUS_READ_PEAK} B)"
    );
    assert!(
        tail_peak <= chunk + RANGE_READ_OVERHEAD,
        "a {chunk} B range near the end of a {len} B attachment held {tail_peak} B of daemon heap \
         (budget {} B)",
        chunk + RANGE_READ_OVERHEAD
    );
    // Each range is consumed before the next is asked for, as `BlobReader`
    // does, so a stream holds no more than one range at a time.
    assert!(
        stream_peak <= chunk + RANGE_READ_OVERHEAD,
        "streaming a {len} B attachment held {stream_peak} B of daemon heap at once (budget {} B)",
        chunk + RANGE_READ_OVERHEAD
    );
    assert!(
        whole_peak >= len,
        "a whole-blob load of {len} B registered {whole_peak} B: either the heap probe cannot \
         observe the reads it bounds, or a read above left the blob in the cache"
    );
    store.store().shutdown().await.unwrap();
}
