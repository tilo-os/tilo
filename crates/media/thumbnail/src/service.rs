//! The off-thread thumbnail service: request a preview, get a placeholder now
//! and the real thing on a later frame.
//!
//! Mirrors the ergonomics of `ui::image_store::ImageStore` (a background worker
//! pool, a per-key slot, `Loading`→`Ready`, `block_until_idle` for `--shots`
//! pre-warm) but generalised to every previewable type and fronted by the
//! content-addressed [`ThumbCache`] so work is never repeated across frames,
//! folders, sessions or peers.
//!
//! A request is keyed by a [`ThumbKey`]. Two ways in:
//!
//! - [`ThumbnailService::request_file`] — generate from a file on disk
//!   (image/PDF/ebook/video by category; a badge category returns immediately).
//! - [`ThumbnailService::request_encoded`] — adopt an already-encoded PNG the
//!   host handed over (the FUSE `user.qos.thumb` xattr), decoding + caching it
//!   without ever touching the source content.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::cache::{ThumbCache, ThumbKey};
use crate::category::FileCategory;
use crate::generate::{self, DEFAULT_EDGE, RgbaThumb, Source};

/// What a client should paint for a given request right now.
#[derive(Clone)]
pub enum ThumbState {
    /// Generation is in flight — paint a placeholder (usually the badge).
    Loading,
    /// A real preview is ready.
    Ready(Arc<RgbaThumb>),
    /// No real preview: an unsupported type, or a decode that failed. The client
    /// draws a type badge for `category`.
    Badge(FileCategory),
}

impl ThumbState {
    pub fn ready(&self) -> Option<&Arc<RgbaThumb>> {
        match self {
            ThumbState::Ready(img) => Some(img),
            _ => None,
        }
    }
}

/// One in-flight or settled slot.
#[derive(Clone)]
enum Slot {
    Loading,
    Ready(Arc<RgbaThumb>),
    Badge(FileCategory),
}

enum JobKind {
    File {
        path: PathBuf,
        category: FileCategory,
    },
    Encoded {
        /// Reading a filesystem xattr may cross a FUSE boundary and the daemon
        /// may need to obtain a tiny projected preview from a peer. Keep that
        /// operation on the worker just like decoding; calling it while a
        /// client paints would turn scrolling into network I/O.
        load: Box<dyn FnOnce() -> Option<Vec<u8>> + Send + 'static>,
    },
    EncodedOrFile {
        /// Prefer host-generated metadata, but generate from an already-local
        /// file when the host has no preview. Both branches stay off the
        /// requesting/render thread.
        load: Box<dyn FnOnce() -> Option<Vec<u8>> + Send + 'static>,
        path: PathBuf,
        category: FileCategory,
    },
}

struct Job {
    key: ThumbKey,
    kind: JobKind,
    edge: u32,
    category: FileCategory,
}

struct Shared {
    slots: Mutex<HashMap<ThumbKey, Slot>>,
    pending: AtomicUsize,
    cache: ThumbCache,
}

/// A cheap-to-clone handle to the shared background thumbnail pool + cache.
#[derive(Clone)]
pub struct ThumbnailService {
    shared: Arc<Shared>,
    tx: Sender<Job>,
    edge: u32,
}

impl ThumbnailService {
    /// Open a service backed by a cache at `cache_dir`, with `workers` decode
    /// threads and the default [`DEFAULT_EDGE`] target size.
    ///
    /// # Errors
    ///
    /// Returns an error if [`ThumbCache::open`] fails to open or create the
    /// cache directory at `cache_dir`.
    pub fn open(cache_dir: impl AsRef<std::path::Path>, workers: usize) -> std::io::Result<Self> {
        Ok(Self::new(
            ThumbCache::open(cache_dir)?,
            workers,
            DEFAULT_EDGE,
        ))
    }

    /// Build a service over an existing cache with an explicit worker count and
    /// target edge.
    pub fn new(cache: ThumbCache, workers: usize, edge: u32) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<Job>();
        let shared = Arc::new(Shared {
            slots: Mutex::new(HashMap::new()),
            pending: AtomicUsize::new(0),
            cache,
        });
        let rx: Arc<Mutex<Receiver<Job>>> = Arc::new(Mutex::new(rx));
        for i in 0..workers.max(1) {
            let rx = rx.clone();
            let shared = shared.clone();
            let _ = std::thread::Builder::new()
                .name(format!("qos-thumb-{i}"))
                .spawn(move || worker_loop(&rx, &shared));
        }
        Self { shared, tx, edge }
    }

    /// The configured target longest-edge for generated thumbnails.
    pub fn edge(&self) -> u32 {
        self.edge
    }

    /// The cache backing this service (for direct content-hash lookups, e.g.
    /// from the FUSE host path).
    pub fn cache(&self) -> &ThumbCache {
        &self.shared.cache
    }

    /// Request a preview for a file on disk. A badge-only `category` returns
    /// [`ThumbState::Badge`] immediately with no work. Otherwise the first call
    /// schedules generation and returns [`ThumbState::Loading`]; a later call
    /// returns [`ThumbState::Ready`] (or `Badge` if it could not be decoded).
    pub fn request_file(&self, key: ThumbKey, category: FileCategory, path: PathBuf) -> ThumbState {
        if !category.has_preview() {
            return ThumbState::Badge(category);
        }
        self.request(key, category, || JobKind::File { path, category })
    }

    /// Adopt an already-encoded PNG thumbnail the host provided (e.g. read from
    /// the `user.qos.thumb` xattr). `png` is a closure so it is only invoked on
    /// a genuine miss — a warm slot never re-reads the xattr.
    pub fn request_encoded(
        &self,
        key: ThumbKey,
        category: FileCategory,
        png: impl FnOnce() -> Option<Vec<u8>> + Send + 'static,
    ) -> ThumbState {
        self.request(key, category, move || JobKind::Encoded {
            load: Box::new(png),
        })
    }

    /// Adopt a host-provided PNG, falling back to generating a preview from a
    /// file whose content is already local. The metadata read, fallback read,
    /// decode, and cache write all execute on the worker pool.
    pub fn request_encoded_or_file(
        &self,
        key: ThumbKey,
        category: FileCategory,
        path: PathBuf,
        png: impl FnOnce() -> Option<Vec<u8>> + Send + 'static,
    ) -> ThumbState {
        self.request(key, category, move || JobKind::EncodedOrFile {
            load: Box::new(png),
            path,
            category,
        })
    }

    fn request(
        &self,
        key: ThumbKey,
        category: FileCategory,
        make_kind: impl FnOnce() -> JobKind,
    ) -> ThumbState {
        {
            let mut slots = self
                .shared
                .slots
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(slot) = slots.get(&key) {
                return slot_to_state(slot);
            }
            slots.insert(key, Slot::Loading);
        }
        self.shared.pending.fetch_add(1, Ordering::SeqCst);
        let job = Job {
            key,
            kind: make_kind(),
            edge: self.edge,
            category,
        };
        if self.tx.send(job).is_err() {
            // Workers gone: undo the pending bump and settle as a badge.
            self.shared.pending.fetch_sub(1, Ordering::SeqCst);
            self.shared
                .slots
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(key, Slot::Badge(category));
            return ThumbState::Badge(category);
        }
        ThumbState::Loading
    }

    /// Number of in-flight generations.
    pub fn pending(&self) -> usize {
        self.shared.pending.load(Ordering::SeqCst)
    }

    /// Whether every scheduled generation has settled.
    pub fn is_idle(&self) -> bool {
        self.pending() == 0
    }

    /// Block until all in-flight generations settle or `timeout` elapses. For
    /// the synchronous `--shots` pre-warm only — never on a live UI thread.
    pub fn block_until_idle(&self, timeout: Duration) {
        let start = Instant::now();
        while self.pending() > 0 && start.elapsed() < timeout {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn slot_to_state(slot: &Slot) -> ThumbState {
    match slot {
        Slot::Loading => ThumbState::Loading,
        Slot::Ready(img) => ThumbState::Ready(img.clone()),
        Slot::Badge(cat) => ThumbState::Badge(*cat),
    }
}

fn worker_loop(rx: &Arc<Mutex<Receiver<Job>>>, shared: &Arc<Shared>) {
    loop {
        let job = {
            let Ok(guard) = rx.lock() else {
                break;
            };
            let Ok(j) = guard.recv() else {
                break; // service dropped
            };
            j
        };
        let key = job.key;
        let slot = run_job(&shared.cache, job);
        shared
            .slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key, slot);
        shared.pending.fetch_sub(1, Ordering::SeqCst);
    }
}

fn run_job(cache: &ThumbCache, job: Job) -> Slot {
    // A prior session (or a peer, via content-addressing) may already hold it.
    if let Some(png) = cache.get(&job.key)
        && let Some(thumb) = generate::decode_png(&png)
    {
        return Slot::Ready(Arc::new(thumb));
    }
    match job.kind {
        JobKind::File { path, category } => {
            match generate::generate(&Source::Path(&path), category, job.edge) {
                Some(thumb) => {
                    if let Some(png) = generate::encode_png(&thumb) {
                        cache.put(&job.key, png);
                    }
                    Slot::Ready(Arc::new(thumb))
                }
                None => Slot::Badge(category),
            }
        }
        JobKind::Encoded { load } => match load() {
            Some(png) => match generate::decode_png(&png) {
                Some(thumb) => {
                    cache.put(&job.key, png);
                    Slot::Ready(Arc::new(thumb))
                }
                None => Slot::Badge(job.category),
            },
            None => Slot::Badge(job.category),
        },
        JobKind::EncodedOrFile {
            load,
            path,
            category,
        } => {
            if let Some(png) = load()
                && let Some(thumb) = generate::decode_png(&png)
            {
                cache.put(&job.key, png);
                return Slot::Ready(Arc::new(thumb));
            }
            match generate::generate(&Source::Path(&path), category, job.edge) {
                Some(thumb) => {
                    if let Some(png) = generate::encode_png(&thumb) {
                        cache.put(&job.key, png);
                    }
                    Slot::Ready(Arc::new(thumb))
                }
                None => Slot::Badge(category),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service() -> (tempfile::TempDir, ThumbnailService) {
        let dir = tempfile::tempdir().unwrap();
        let cache = ThumbCache::open(dir.path()).unwrap();
        (dir, ThumbnailService::new(cache, 3, 128))
    }

    #[test]
    fn image_file_loads_then_ready_and_second_request_hits() {
        let (_dir, svc) = service();
        let src = tempfile::NamedTempFile::with_suffix(".png").unwrap();
        std::fs::write(
            src.path(),
            crate::testutil::solid_png(300, 200, [40, 90, 200]),
        )
        .unwrap();
        let key = ThumbKey::of_bytes(b"img-key", 128);

        // First look: placeholder + a load is scheduled.
        assert!(matches!(
            svc.request_file(key, FileCategory::Image, src.path().to_path_buf()),
            ThumbState::Loading
        ));
        assert!(!svc.is_idle());

        svc.block_until_idle(Duration::from_secs(5));
        assert!(svc.is_idle(), "load settled");

        let state = svc.request_file(key, FileCategory::Image, src.path().to_path_buf());
        let img = state.ready().expect("ready after the async load");
        assert!(img.is_valid());
        assert_eq!(img.max_edge(), 128, "downscaled to the target edge");

        // The cache now holds it content-addressed on disk.
        assert!(svc.cache().contains(&key), "generation populated the cache");
    }

    #[test]
    fn badge_category_settles_immediately_without_work() {
        let (_dir, svc) = service();
        let key = ThumbKey::of_bytes(b"archive", 128);
        assert!(matches!(
            svc.request_file(key, FileCategory::Archive, PathBuf::from("/x/a.zip")),
            ThumbState::Badge(FileCategory::Archive)
        ));
        assert!(svc.is_idle(), "a badge schedules no background work");
    }

    #[test]
    fn a_broken_image_settles_as_a_badge() {
        let (_dir, svc) = service();
        let src = tempfile::NamedTempFile::with_suffix(".png").unwrap();
        std::fs::write(src.path(), b"not really a png").unwrap();
        let key = ThumbKey::of_bytes(b"broken", 128);
        svc.request_file(key, FileCategory::Image, src.path().to_path_buf());
        svc.block_until_idle(Duration::from_secs(5));
        assert!(matches!(
            svc.request_file(key, FileCategory::Image, src.path().to_path_buf()),
            ThumbState::Badge(FileCategory::Image)
        ));
    }

    #[test]
    fn encoded_host_thumbnail_is_adopted_without_touching_source() {
        // The FUSE host path: a ready-made PNG comes in over the xattr channel.
        let (_dir, svc) = service();
        let png = crate::generate::encode_png(
            &crate::generate::image_thumb(
                &crate::testutil::solid_png(120, 120, [10, 220, 60]),
                128,
            )
            .unwrap(),
        )
        .unwrap();
        let key = ThumbKey::of_hash([7u8; 32], 128);
        let png2 = png;
        svc.request_encoded(key, FileCategory::Image, move || Some(png2));
        svc.block_until_idle(Duration::from_secs(5));
        let state = svc.request_encoded(key, FileCategory::Image, || {
            panic!("must not re-read a warm slot")
        });
        assert!(
            state.ready().is_some(),
            "host-provided thumbnail is adopted"
        );
    }

    #[test]
    fn encoded_thumbnail_loader_never_runs_on_the_requesting_thread() {
        let (_dir, svc) = service();
        let key = ThumbKey::of_hash([8u8; 32], 128);
        let caller = std::thread::current().id();
        let (tx, rx) = std::sync::mpsc::channel();

        assert!(matches!(
            svc.request_encoded(key, FileCategory::Image, move || {
                tx.send(std::thread::current().id()).unwrap();
                None
            }),
            ThumbState::Loading
        ));
        let worker = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_ne!(
            caller, worker,
            "a FUSE/xattr lookup from draw must be dispatched to a thumbnail worker"
        );
        svc.block_until_idle(Duration::from_secs(2));
        assert!(matches!(
            svc.request_encoded(key, FileCategory::Image, || {
                panic!("the settled badge must not reload the xattr")
            }),
            ThumbState::Badge(FileCategory::Image)
        ));
    }
}
