//! Daemon heap that document media and loose-blob reads cost.
//!
//! An app reads media in ranges of at most [`MAX_READ_CHUNK`], and asks
//! whether content is readable with an empty range. Each of those must cost
//! the daemon the range, not the attachment: a status check reads no data,
//! a range near the end of the largest attachment holds one range, and a
//! streamed read of the whole attachment never holds more than one range at
//! a time. The store is reopened before measuring, as after a daemon
//! restart, so nothing is served from the blob cache.
//!
//! Storing and uploading cost the same bound. A stored attachment is not
//! kept in the daemon's heap afterwards, an upload of the largest attachment
//! never holds more than a few messages of it at once, and many large reads
//! leave no more behind than the blob cache's byte budget.
//!
//! The figure is the process-wide peak of live heap bytes above what was
//! live when the operation began, counted by [`PeakCounting`]: the store's
//! actors, the capability's server and the app's blocking client run as
//! tasks and threads of their own, so a per-thread count would miss them.
//! The one test runs alone in this binary, so no other test's allocations
//! land in the figure.
//!
//! It runs on a single-threaded runtime. The blob cache frees an evicted
//! entry only once the thread that evicted it collects its deferred
//! garbage; on a multi-threaded runtime which worker last touched the cache
//! varies from run to run, and with it how many evicted 1 MiB blobs are
//! still awaiting collection when the reads end.
//!
//! Each footprint is also printed as a `PERF <name>: <x> MiB (SLO <= <y> MiB)`
//! line, the KPI the performance record keeps (`tools/perf/kpi-budgets.toml`).

#![allow(
    clippy::unwrap_used,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use fs::ContentStore;
use fs::document_media::DocumentMedia;
use fs::store::{BLOB_CACHE_BYTES, MAX_CACHED_BLOB};
use runtime_fs::blobs::{
    BlobBackend, BlobChunks, BlobClient, BlobError, BlobFuture, BlobPrincipal, BlobRef, BlobScope,
    MAX_BLOB_BYTES, MAX_PUT_CHUNK, MAX_READ_CHUNK, ReadOutcome, write_handoff,
};

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

/// Live heap bytes above `base` now.
fn live_above(base: usize) -> usize {
    LIVE.load(Ordering::Relaxed).saturating_sub(base)
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

/// Daemon heap a stored attachment may leave behind once `store` returned:
/// the room's reference and the store's bookkeeping, none of the content.
/// Before, the content store cached every stored blob whole, 64 MiB for the
/// largest attachment. Measured at 0 B.
const STORE_RETAINED_HEAP_SLO: usize = 256 * 1024;

/// Heap an upload of the largest attachment may hold at once, the app's end
/// included (it runs in this process): the app's chunk and the message
/// carrying it, the message as the daemon reads it, the pieces in flight to
/// the store and the store's spill writes. Four messages' worth
/// (4 × [`MAX_PUT_CHUNK`]); measured at 8.2 MiB (debug build, macOS).
/// Before, the daemon gathered the whole upload before storing it: 101.6 MiB
/// at the peak, its buffer growing past 64 MiB.
const UPLOAD_PEAK_HEAP_SLO: usize = 16 * 1024 * 1024;
const _: () = assert!(UPLOAD_PEAK_HEAP_SLO == 4 * MAX_PUT_CHUNK);

/// Heap an upload may leave behind once the app's connection closed: the
/// thread pool and connection bookkeeping, measured at 149 KiB, none of the
/// content. Before, the stored attachment stayed cached, 64 MiB.
const UPLOAD_RETAINED: usize = 1024 * 1024;

/// What many large reads through the whole-blob API may leave behind: the
/// blob cache's byte budget and 1 MiB of its own bookkeeping; measured at
/// 32.1 MiB. Before, the cache was bounded by entry count and kept every
/// blob read.
const CACHE_RETAINED_HEAP_SLO: usize = 33 * 1024 * 1024;
const _: () = assert!(CACHE_RETAINED_HEAP_SLO as u64 == BLOB_CACHE_BYTES + 1024 * 1024);

const PHOTOS: (&str, &str) = ("room-1", "so.tilo.photos");

fn photos() -> BlobScope {
    BlobScope::new(PHOTOS.0, PHOTOS.1).unwrap()
}

/// The xorshift stream behind [`deterministic_bytes`] and [`Generated`].
struct Xorshift(u64);

impl Xorshift {
    const fn new() -> Self {
        Self(0x2545_f491_4f6c_dd1d)
    }

    const fn next(&mut self) -> u8 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0.to_le_bytes()[0]
    }
}

fn deterministic_bytes(len: usize) -> Vec<u8> {
    let mut bytes = Xorshift::new();
    (0..len).map(|_| bytes.next()).collect()
}

/// [`deterministic_bytes`] produced as they are read, so an upload's source
/// holds none of the content itself.
struct Generated {
    bytes: Xorshift,
    left: usize,
}

impl Generated {
    const fn new(len: usize) -> Self {
        Self {
            bytes: Xorshift::new(),
            left: len,
        }
    }
}

impl std::io::Read for Generated {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = buf.len().min(self.left);
        for byte in &mut buf[..n] {
            *byte = self.bytes.next();
        }
        self.left -= n;
        Ok(n)
    }
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

/// Print a footprint KPI and hold it to its ceiling.
fn report_heap(name: &str, bytes: usize, ceiling: usize) {
    eprintln!(
        "PERF {name}: {:.3} MiB (SLO <= {:.3} MiB)",
        mib(bytes),
        mib(ceiling)
    );
    assert!(
        bytes <= ceiling,
        "{name}: {bytes} B of daemon heap (budget {ceiling} B)"
    );
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

/// A document-media backend that gathers a streamed upload whole before
/// storing it, as the capability's server did before uploads streamed. The
/// probe must register what it holds, or the upload bound proves nothing.
struct Gathering(Arc<DocumentMedia>);

impl BlobBackend for Gathering {
    fn put(
        self: Arc<Self>,
        scope: BlobScope,
        bytes: Vec<u8>,
        mime: String,
    ) -> BlobFuture<Result<BlobRef, BlobError>> {
        Arc::clone(&self.0).put(scope, bytes, mime)
    }

    fn put_stream(
        self: Arc<Self>,
        scope: BlobScope,
        mut chunks: BlobChunks,
        mime: String,
    ) -> BlobFuture<Result<BlobRef, BlobError>> {
        Box::pin(async move {
            let mut whole = Vec::new();
            while let Some(piece) = chunks.next().await {
                whole.extend_from_slice(
                    &piece.map_err(|error| BlobError::Input(error.to_string()))?,
                );
            }
            Arc::clone(&self.0).put(scope, whole, mime).await
        })
    }

    fn read(
        self: Arc<Self>,
        scope: BlobScope,
        hash: String,
        offset: u64,
        max_len: u32,
    ) -> BlobFuture<ReadOutcome> {
        Arc::clone(&self.0).read(scope, hash, offset, max_len)
    }

    fn has(self: Arc<Self>, scope: BlobScope, hash: String) -> BlobFuture<bool> {
        Arc::clone(&self.0).has(scope, hash)
    }

    fn size(self: Arc<Self>, scope: BlobScope, hash: String) -> BlobFuture<Option<u64>> {
        Arc::clone(&self.0).size(scope, hash)
    }
}

/// Serve `backend` on a fresh socket under `dir`, as the daemon serves its
/// document media.
fn serve(dir: &Path, name: &str, backend: Arc<dyn BlobBackend>) -> PathBuf {
    let socket = dir.join(name);
    runtime_fs::blobs::spawn_blob_server(
        UnixListener::bind(&socket).unwrap(),
        backend,
        tokio::runtime::Handle::current(),
    )
    .unwrap();
    socket
}

/// Upload `len` generated bytes through `socket` as the photos app,
/// returning the stored reference and the peak heap the upload took.
async fn upload(socket: PathBuf, len: usize) -> (BlobRef, Duration, usize) {
    let base = reset_peak();
    let started = Instant::now();
    let stored = tokio::task::spawn_blocking(move || {
        let mut stream = UnixStream::connect(&socket).unwrap();
        write_handoff(&mut stream, &BlobPrincipal::App(photos())).unwrap();
        let client = BlobClient::from_app_stream(stream).unwrap();
        client.put_reader(&mut Generated::new(len), "video/mp4")
    })
    .await
    .unwrap()
    .unwrap();
    (stored, started.elapsed(), peak_above(base))
}

#[tokio::test]
async fn document_media_and_blob_reads_hold_ranges_not_attachments() {
    cold_reads_hold_one_range_of_the_largest_attachment().await;
    storing_and_uploading_hold_no_whole_attachment().await;
    many_large_reads_leave_no_more_than_the_cache_budget().await;
}

async fn cold_reads_hold_one_range_of_the_largest_attachment() {
    let dir = tempfile::tempdir().unwrap();
    let photos = photos();
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
    // blob loaded and verified, then sliced. This is also the probe's own
    // check: a counter that cannot see a whole-blob load must fail here
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

async fn storing_and_uploading_hold_no_whole_attachment() {
    let dir = tempfile::tempdir().unwrap();
    let len = usize::try_from(MAX_BLOB_BYTES).unwrap();
    let store = Arc::new(ContentStore::open(dir.path().join("store")).await.unwrap());
    let media = DocumentMedia::new(Arc::clone(&store));

    // Storing: the caller hands over the attachment, and once the call
    // returns none of it may stay in the daemon's heap.
    let data = deterministic_bytes(len);
    let expected = blake3::hash(&data);
    let base = reset_peak();
    let started = Instant::now();
    let stored = media.store(&photos(), data).await.unwrap();
    let store_time = started.elapsed();
    // `base` counted the attachment the call consumed.
    let stored_retained = (LIVE.load(Ordering::Relaxed) + len).saturating_sub(base);
    assert_eq!(stored.as_bytes(), expected.as_bytes());

    // Uploading over the capability, as an app does: the app's source
    // produces the bytes as they are read, so all the heap counted is what
    // the app's client and the daemon hold on the way.
    let socket = serve(
        dir.path(),
        "media.sock",
        Arc::clone(&media) as Arc<dyn BlobBackend>,
    );
    let base = LIVE.load(Ordering::Relaxed);
    let (uploaded, upload_time, upload_peak) = upload(socket, len).await;
    let upload_retained = live_above(base);
    assert_eq!(uploaded.len, MAX_BLOB_BYTES);
    assert_eq!(uploaded.hash, expected.to_hex().as_str());
    let hash: iroh_blobs::Hash = uploaded.hash.parse().unwrap();
    assert_eq!(
        stream_whole(&media, &photos(), hash, MAX_BLOB_BYTES)
            .await
            .as_ref()
            .map(blake3::Hash::as_bytes),
        Some(expected.as_bytes()),
        "the uploaded attachment reads back whole and verified"
    );

    // The probe's own check: the same upload through a server that gathers
    // it whole must register the whole attachment, or the bound above
    // cannot see the buffering it is there to catch.
    let gathering = serve(
        dir.path(),
        "gathering.sock",
        Arc::new(Gathering(Arc::clone(&media))),
    );
    let (_, _, gathered_peak) = upload(gathering, len).await;

    eprintln!(
        "document media, {:.0} MiB attachment: store {:.3} ms, {:.1} KiB left behind; \
         upload {:.3} ms / {:.2} MiB peak, {:.1} KiB left behind; \
         gathering the upload whole {:.2} MiB peak",
        mib(len),
        ms(store_time),
        kib(stored_retained),
        ms(upload_time),
        mib(upload_peak),
        kib(upload_retained),
        mib(gathered_peak),
    );
    report_heap(
        "document-media-store-64m-retained-heap",
        stored_retained,
        STORE_RETAINED_HEAP_SLO,
    );
    report_heap(
        "document-media-upload-64m-peak-heap",
        upload_peak,
        UPLOAD_PEAK_HEAP_SLO,
    );
    assert!(
        upload_retained <= UPLOAD_RETAINED,
        "an upload of {len} B left {upload_retained} B of heap behind (budget {UPLOAD_RETAINED} B)"
    );
    assert!(
        gathered_peak >= len,
        "an upload gathered whole registered {gathered_peak} B for {len} B: the heap probe \
         cannot observe the buffering the upload bound is there to catch"
    );
    store.store().shutdown().await.unwrap();
}

async fn many_large_reads_leave_no_more_than_the_cache_budget() {
    let dir = tempfile::tempdir().unwrap();
    // Twice the cache budget in blobs the cache admits, and as much again in
    // blobs too large for it.
    let small = 2 * usize::try_from(BLOB_CACHE_BYTES).unwrap() / MAX_CACHED_BLOB;
    let large_len = 16 * 1024 * 1024;
    let large = 4;
    let blob = |index: usize, len: usize| {
        let mut bytes = vec![0x5a_u8; len];
        bytes[..8].copy_from_slice(&(index as u64).to_le_bytes());
        Bytes::from(bytes)
    };
    let hashes = {
        let store = ContentStore::open(dir.path()).await.unwrap();
        let mut hashes = Vec::new();
        for index in 0..small {
            hashes.push(store.ingest(blob(index, MAX_CACHED_BLOB)).await.unwrap());
        }
        for index in small..small + large {
            hashes.push(store.ingest(blob(index, large_len)).await.unwrap());
        }
        store.store().shutdown().await.unwrap();
        hashes
    };
    let store = ContentStore::open(dir.path()).await.unwrap();
    let read = small * MAX_CACHED_BLOB + large * large_len;

    let base = reset_peak();
    let started = Instant::now();
    for (index, hash) in hashes.iter().enumerate() {
        let bytes = store.local(*hash).await.unwrap();
        assert_eq!(bytes[..8], (index as u64).to_le_bytes());
    }
    let read_time = started.elapsed();
    let retained = live_above(base);
    let peak = peak_above(base);

    eprintln!(
        "{} loose blobs, {:.0} MiB read whole once each: {:.3} ms, {:.2} MiB peak, \
         {:.2} MiB left behind (cache budget {:.0} MiB)",
        hashes.len(),
        mib(read),
        ms(read_time),
        mib(peak),
        mib(retained),
        mib(usize::try_from(BLOB_CACHE_BYTES).unwrap()),
    );
    report_heap(
        "blob-cache-large-reads-retained-heap",
        retained,
        CACHE_RETAINED_HEAP_SLO,
    );
    // The probe's own check: the cache does keep what fits its budget, so a
    // probe that registered nothing could not tell a bounded cache from one
    // that keeps everything.
    assert!(
        retained >= usize::try_from(BLOB_CACHE_BYTES).unwrap() / 2,
        "{retained} B left behind by reads that fill a {BLOB_CACHE_BYTES} B cache: the heap probe \
         cannot observe what the cache keeps"
    );
    store.store().shutdown().await.unwrap();
}
