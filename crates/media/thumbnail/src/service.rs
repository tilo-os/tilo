//! The off-thread thumbnail service: request a preview, get a placeholder now
//! and the generated image on a later frame.
//!
//! Every preview in the system goes through here: a file-manager tile, a photo
//! in a library grid, a camera roll peek, a video filmstrip cell. One
//! **priority queue** feeds one worker pool, fronted by the content-addressed
//! [`ThumbCache`], so work is never repeated across frames, folders, sessions
//! or peers:
//!
//! - A request is keyed by a [`ThumbKey`]. A key already queued, in flight or
//!   settled is never scheduled twice; asking again for a queued key at a
//!   higher [`Priority`] moves it up instead.
//! - Workers always take the highest-priority job, newest first within a
//!   priority, so what just scrolled into view beats what scrolled past.
//! - The queue is bounded: past [`DEFAULT_MAX_QUEUED`] the lowest-priority,
//!   oldest request is dropped (and forgotten, so asking again re-queues it).
//! - Settled results kept in memory are bounded too; an evicted preview is
//!   still in the cache and settles again on the next request.
//! - Decoders are the shared ones: images and EPUB covers in pure Rust, PDFs
//!   through the sandboxed PDF helper, video through the `GStreamer`
//!   `MediaEngine`. A worker keeps its video pipeline open and prefers jobs
//!   for that source, so a filmstrip decodes through one pipeline.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use crate::cache::{ThumbCache, ThumbKey};
use crate::category::FileCategory;
use crate::generate::{self, DEFAULT_EDGE, RgbaThumb, Source, ThumbSize};

/// Requests waiting for a worker before the lowest-priority one is dropped.
pub const DEFAULT_MAX_QUEUED: usize = 512;
/// Settled results kept in memory; older ones are re-read from the cache.
pub const DEFAULT_MAX_SETTLED: usize = 2048;
/// How long a worker holding an open video pipeline waits for more work before
/// closing it — long enough for a scrub, short enough not to pin a decoder.
#[cfg_attr(
    not(feature = "video"),
    allow(dead_code, reason = "only a worker with a video pipeline retires it")
)]
const PIPELINE_IDLE: Duration = Duration::from_secs(20);
/// How deep into its priority tier a worker with an open pipeline looks for a
/// job on that same source before taking the head of the queue.
#[cfg_attr(
    not(feature = "video"),
    allow(
        dead_code,
        reason = "only a worker with a video pipeline has a preference"
    )
)]
const AFFINITY_SCAN: usize = 32;

/// How urgently a preview is wanted. Workers always take the highest first.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Priority {
    /// Work nobody is looking at yet (warming a library).
    Background,
    /// Just outside the viewport: likely next.
    Prefetch,
    /// On screen now.
    #[default]
    Visible,
}

/// What a client should paint for a given request right now.
#[derive(Clone)]
pub enum ThumbState {
    /// Generation is queued or in flight — paint a placeholder.
    Loading,
    /// A rendered preview is ready.
    Ready(Arc<RgbaThumb>),
    /// No rendered preview: an unsupported type, or a decode that failed. The
    /// client draws a type badge for `category`.
    Badge(FileCategory),
}

impl ThumbState {
    pub fn ready(&self) -> Option<&Arc<RgbaThumb>> {
        match self {
            ThumbState::Ready(img) => Some(img),
            _ => None,
        }
    }

    pub fn is_loading(&self) -> bool {
        matches!(self, ThumbState::Loading)
    }
}

/// A loader run on a worker, never on the requesting thread: reading an
/// xattr or a file may cross a FUSE boundary and wait on a peer. Only a
/// host with no threads at all (a browser page, which has no FUSE either)
/// runs it on the requesting thread.
type Loader = Box<dyn FnOnce() -> Option<Vec<u8>> + Send + 'static>;

enum Work {
    /// Generate from a file on disk, by category.
    File(PathBuf),
    /// Generate from source bytes a loader produces (a content-store read).
    Load(Loader),
    /// Adopt an already-encoded PNG (the FUSE `user.tilo.thumb` xattr).
    Encoded(Loader),
    /// Prefer a host-provided PNG, else generate from an already-local file.
    EncodedOrFile { load: Loader, path: PathBuf },
    /// The frame at `at_ms` of a video.
    VideoFrame { path: PathBuf, at_ms: u64 },
}

impl Work {
    fn video_path(&self) -> Option<&PathBuf> {
        match self {
            Work::VideoFrame { path, .. } => Some(path),
            _ => None,
        }
    }
}

/// One preview to produce. Build with a constructor, adjust with
/// [`ThumbRequest::priority`], hand to [`ThumbnailService::request`].
pub struct ThumbRequest {
    key: ThumbKey,
    category: FileCategory,
    priority: Priority,
    transient: bool,
    work: Work,
}

impl ThumbRequest {
    /// Generate from a file on disk (image, PDF, ebook or video by `category`).
    pub fn file(key: ThumbKey, category: FileCategory, path: PathBuf) -> Self {
        Self::new(key, category, Work::File(path))
    }

    /// Generate from the source bytes `load` returns — typically a read of
    /// the content store, run on a worker.
    pub fn load(
        key: ThumbKey,
        category: FileCategory,
        load: impl FnOnce() -> Option<Vec<u8>> + Send + 'static,
    ) -> Self {
        Self::new(key, category, Work::Load(Box::new(load)))
    }

    /// Adopt an already-encoded PNG the host provided. `png` runs only on a
    /// cache miss, on a worker.
    pub fn encoded(
        key: ThumbKey,
        category: FileCategory,
        png: impl FnOnce() -> Option<Vec<u8>> + Send + 'static,
    ) -> Self {
        Self::new(key, category, Work::Encoded(Box::new(png)))
    }

    /// Adopt a host-provided PNG, falling back to generating a preview from a
    /// file whose content is already local.
    pub fn encoded_or_file(
        key: ThumbKey,
        category: FileCategory,
        path: PathBuf,
        png: impl FnOnce() -> Option<Vec<u8>> + Send + 'static,
    ) -> Self {
        Self::new(
            key,
            category,
            Work::EncodedOrFile {
                load: Box::new(png),
                path,
            },
        )
    }

    /// The frame at `at_ms` of the video at `path`, fitted to `key.size`. Key
    /// it with [`ThumbKey::at_frame`] so each time has its own entry.
    pub fn video_frame(key: ThumbKey, path: PathBuf, at_ms: u64) -> Self {
        Self::new(key, FileCategory::Video, Work::VideoFrame { path, at_ms })
    }

    fn new(key: ThumbKey, category: FileCategory, work: Work) -> Self {
        Self {
            key,
            category,
            priority: Priority::default(),
            transient: false,
            work,
        }
    }

    #[must_use]
    pub fn priority(mut self, priority: Priority) -> Self {
        self.priority = priority;
        self
    }

    /// Keep the result in memory only, never in the persistent cache: for a
    /// source with no stable content identity (a legacy path whose file may be
    /// rewritten in place), where a stored preview could outlive its pixels.
    #[must_use]
    pub fn transient(mut self) -> Self {
        self.transient = true;
        self
    }

    /// The key this request settles under.
    pub fn key(&self) -> ThumbKey {
        self.key
    }
}

#[derive(Clone)]
enum Slot {
    Loading,
    Ready(Arc<RgbaThumb>),
    Badge(FileCategory),
}

impl Slot {
    fn state(&self) -> ThumbState {
        match self {
            Slot::Loading => ThumbState::Loading,
            Slot::Ready(img) => ThumbState::Ready(img.clone()),
            Slot::Badge(cat) => ThumbState::Badge(*cat),
        }
    }
}

/// Queue order: priority first, then request sequence (newest last), so the
/// map's last entry is always the job to run next.
type Order = (Priority, u64);

struct Queued {
    order: Order,
    job: Job,
}

struct Job {
    key: ThumbKey,
    category: FileCategory,
    transient: bool,
    work: Work,
}

#[derive(Default)]
struct Inner {
    slots: HashMap<ThumbKey, Slot>,
    queue: BTreeMap<Order, ThumbKey>,
    queued: HashMap<ThumbKey, Queued>,
    seq: u64,
    in_flight: usize,
    /// Settled keys, oldest first, for bounding `slots`. May name keys that
    /// were since forgotten; eviction skips those.
    settled: VecDeque<ThumbKey>,
    shutdown: bool,
}

impl Inner {
    fn pending(&self) -> usize {
        self.queued.len().saturating_add(self.in_flight)
    }

    fn next_order(&mut self, priority: Priority) -> Order {
        self.seq = self.seq.wrapping_add(1);
        (priority, self.seq)
    }

    /// Take the next job: the head of the queue, unless a job for the video
    /// source this worker already has open sits in the same priority tier.
    fn take(&mut self, open_video: Option<&PathBuf>) -> Option<Job> {
        let (&head, _) = self.queue.last_key_value()?;
        let mut pick = head;
        if let Some(open) = open_video {
            let same_source = self
                .queue
                .iter()
                .rev()
                .take(AFFINITY_SCAN)
                .take_while(|(order, _)| order.0 == head.0)
                .find(|(_, key)| {
                    self.queued
                        .get(key)
                        .and_then(|q| q.job.work.video_path())
                        .is_some_and(|path| path == open)
                });
            if let Some((&order, _)) = same_source {
                pick = order;
            }
        }
        let key = self.queue.remove(&pick)?;
        let queued = self.queued.remove(&key)?;
        self.in_flight = self.in_flight.saturating_add(1);
        Some(queued.job)
    }
}

struct Shared {
    inner: Mutex<Inner>,
    /// Signalled when work is queued or the service shuts down.
    work: Condvar,
    /// Signalled when a job settles.
    settled: Condvar,
    cache: ThumbCache,
    /// Where transient results go instead: nowhere that outlives the slot.
    transient: ThumbCache,
    max_queued: usize,
    max_settled: usize,
    wake: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Stops the workers when the last service handle goes away.
struct Shutdown(Arc<Shared>);

impl Drop for Shutdown {
    fn drop(&mut self) {
        let mut inner = self.0.lock();
        inner.shutdown = true;
        inner.queue.clear();
        inner.queued.clear();
        drop(inner);
        self.0.work.notify_all();
        self.0.settled.notify_all();
    }
}

/// A cheap-to-clone handle to the shared background thumbnail pool + cache.
#[derive(Clone)]
pub struct ThumbnailService {
    shared: Arc<Shared>,
    _shutdown: Arc<Shutdown>,
    edge: u32,
    /// No worker could start (a browser page has no threads): each request
    /// is generated on the requesting thread instead.
    inline: bool,
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

    /// Open a service over the per-user preview cache (`tilo/thumbnails` in
    /// the cache directory), or, with no cache directory, a temporary one.
    ///
    /// # Errors
    ///
    /// Returns an error if neither location can be created.
    pub fn open_default(workers: usize) -> std::io::Result<Self> {
        // No cache directory and no file system (the page): previews live
        // in memory. `temp_dir` panics there rather than failing.
        if tilo_paths::availability(tilo_paths::Feature::NativePaths)
            != tilo_paths::Availability::Available
        {
            return Ok(Self::ephemeral(workers));
        }
        let dir = xdg_paths::cache_dir().map_or_else(
            || std::env::temp_dir().join("tilo-thumbnails"),
            |cache| cache.join("tilo").join("thumbnails"),
        );
        Self::open(dir, workers)
    }

    /// A service whose cache lives in memory only — hermetic samples and tests,
    /// which must not write the user's cache.
    pub fn ephemeral(workers: usize) -> Self {
        Self::new(ThumbCache::ephemeral(256), workers, DEFAULT_EDGE)
    }

    /// Build a service over an existing cache with an explicit worker count and
    /// default target edge.
    pub fn new(cache: ThumbCache, workers: usize, edge: u32) -> Self {
        Self::with_bounds(
            cache,
            workers,
            edge,
            DEFAULT_MAX_QUEUED,
            DEFAULT_MAX_SETTLED,
        )
    }

    /// [`ThumbnailService::new`] with explicit queue and settled-result bounds.
    pub fn with_bounds(
        cache: ThumbCache,
        workers: usize,
        edge: u32,
        max_queued: usize,
        max_settled: usize,
    ) -> Self {
        let shared = Arc::new(Shared {
            inner: Mutex::new(Inner::default()),
            work: Condvar::new(),
            settled: Condvar::new(),
            cache,
            transient: ThumbCache::ephemeral(0),
            max_queued: max_queued.max(1),
            max_settled: max_settled.max(1),
            wake: Mutex::new(None),
        });
        let mut started = 0_usize;
        for i in 0..workers.max(1) {
            let shared = shared.clone();
            if std::thread::Builder::new()
                .name(format!("tilo-thumb-{i}"))
                .spawn(move || worker_loop(&shared))
                .is_ok()
            {
                started = started.saturating_add(1);
            }
        }
        Self {
            _shutdown: Arc::new(Shutdown(shared.clone())),
            shared,
            edge,
            inline: started == 0,
        }
    }

    /// Call `wake` (from a worker thread) whenever a preview settles, so an
    /// app that sleeps between frames repaints when one lands.
    pub fn set_wake(&self, wake: impl Fn() + Send + Sync + 'static) {
        *self
            .shared
            .wake
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::new(wake));
    }

    /// The configured default longest edge for generated thumbnails.
    pub fn edge(&self) -> u32 {
        self.edge
    }

    /// The cache backing this service (for direct content-hash lookups, e.g.
    /// from the FUSE host path).
    pub fn cache(&self) -> &ThumbCache {
        &self.shared.cache
    }

    /// Ask for a preview. A badge-only category settles immediately with no
    /// work. Otherwise the first call queues generation and returns
    /// [`ThumbState::Loading`]; a later call returns [`ThumbState::Ready`] (or
    /// `Badge` if it could not be decoded). Asking again for a queued key at a
    /// higher priority moves it up the queue.
    pub fn request(&self, request: ThumbRequest) -> ThumbState {
        let is_frame = matches!(request.work, Work::VideoFrame { .. });
        if !is_frame && !request.category.has_preview() {
            return ThumbState::Badge(request.category);
        }
        let mut inner = self.shared.lock();
        if inner.shutdown {
            return ThumbState::Badge(request.category);
        }
        if let Some(slot) = inner.slots.get(&request.key) {
            let state = slot.state();
            if let Some(queued) = inner.queued.get(&request.key)
                && request.priority > queued.order.0
            {
                let old = queued.order;
                let order = inner.next_order(request.priority);
                inner.queue.remove(&old);
                inner.queue.insert(order, request.key);
                if let Some(queued) = inner.queued.get_mut(&request.key) {
                    queued.order = order;
                }
            }
            return state;
        }
        let order = inner.next_order(request.priority);
        inner.slots.insert(request.key, Slot::Loading);
        inner.queue.insert(order, request.key);
        inner.queued.insert(
            request.key,
            Queued {
                order,
                job: Job {
                    key: request.key,
                    category: request.category,
                    transient: request.transient,
                    work: request.work,
                },
            },
        );
        // Bounded: drop the least urgent, oldest request and forget it, so a
        // client that still wants it simply asks again.
        while inner.queue.len() > self.shared.max_queued {
            let Some((_, dropped)) = inner.queue.pop_first() else {
                break;
            };
            inner.queued.remove(&dropped);
            inner.slots.remove(&dropped);
        }
        drop(inner);
        self.shared.work.notify_one();
        if self.inline {
            self.run_queued();
            return self.peek(&request.key).unwrap_or(ThumbState::Loading);
        }
        ThumbState::Loading
    }

    /// Generate every queued preview on this thread, for a service with no
    /// workers.
    fn run_queued(&self) {
        let mut video = VideoPipeline::default();
        loop {
            let mut inner = self.shared.lock();
            if inner.shutdown {
                return;
            }
            let Some(job) = inner.take(None) else {
                return;
            };
            drop(inner);
            let key = job.key;
            let cache = if job.transient {
                &self.shared.transient
            } else {
                &self.shared.cache
            };
            let slot = run_job(cache, job, &mut video);
            finish(&self.shared, key, slot);
        }
    }

    /// Request a preview for a file on disk at [`Priority::Visible`].
    pub fn request_file(&self, key: ThumbKey, category: FileCategory, path: PathBuf) -> ThumbState {
        self.request(ThumbRequest::file(key, category, path))
    }

    /// Adopt an already-encoded PNG thumbnail the host provided (e.g. read from
    /// the `user.tilo.thumb` xattr), at [`Priority::Visible`].
    pub fn request_encoded(
        &self,
        key: ThumbKey,
        category: FileCategory,
        png: impl FnOnce() -> Option<Vec<u8>> + Send + 'static,
    ) -> ThumbState {
        self.request(ThumbRequest::encoded(key, category, png))
    }

    /// Adopt a host-provided PNG, falling back to generating a preview from a
    /// file whose content is already local, at [`Priority::Visible`].
    pub fn request_encoded_or_file(
        &self,
        key: ThumbKey,
        category: FileCategory,
        path: PathBuf,
        png: impl FnOnce() -> Option<Vec<u8>> + Send + 'static,
    ) -> ThumbState {
        self.request(ThumbRequest::encoded_or_file(key, category, path, png))
    }

    /// What `key` would paint right now, without scheduling anything. `None`
    /// when it was never requested (or has been forgotten).
    pub fn peek(&self, key: &ThumbKey) -> Option<ThumbState> {
        self.shared.lock().slots.get(key).map(Slot::state)
    }

    /// Withdraw a request that is still queued (it scrolled away). An in-flight
    /// or settled preview is left alone. Returns whether a job was withdrawn.
    pub fn cancel(&self, key: &ThumbKey) -> bool {
        let mut inner = self.shared.lock();
        let Some(queued) = inner.queued.remove(key) else {
            return false;
        };
        inner.queue.remove(&queued.order);
        inner.slots.remove(key);
        drop(inner);
        self.shared.settled.notify_all();
        true
    }

    /// Forget a settled result so the next request asks the cache (or the
    /// decoder) again. Queued and in-flight work is left alone.
    pub fn forget(&self, key: &ThumbKey) {
        let mut inner = self.shared.lock();
        if !inner.queued.contains_key(key)
            && inner
                .slots
                .get(key)
                .is_some_and(|s| !matches!(s, Slot::Loading))
        {
            inner.slots.remove(key);
        }
    }

    /// Number of queued and in-flight generations.
    pub fn pending(&self) -> usize {
        self.shared.lock().pending()
    }

    /// Whether every scheduled generation has settled.
    pub fn is_idle(&self) -> bool {
        self.pending() == 0
    }

    /// Block until all scheduled generations settle or `timeout` elapses. For
    /// the synchronous `--shots` pre-warm and tests only — never on a live UI
    /// thread.
    pub fn block_until_idle(&self, timeout: Duration) {
        drop(
            self.shared
                .settled
                .wait_timeout_while(self.shared.lock(), timeout, |inner| {
                    inner.pending() > 0 && !inner.shutdown
                })
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
    }
}

/// A worker's open video pipeline, reused across requests for one source.
#[derive(Default)]
struct VideoPipeline {
    #[cfg(feature = "video")]
    session: Option<crate::video::FrameSession>,
    /// Without a decoder nothing is ever open; the path stays `None`.
    #[cfg(not(feature = "video"))]
    session: Option<PathBuf>,
}

impl VideoPipeline {
    fn open_path(&self) -> Option<PathBuf> {
        #[cfg(feature = "video")]
        {
            self.session.as_ref().map(|s| s.path().to_path_buf())
        }
        #[cfg(not(feature = "video"))]
        {
            self.session.clone()
        }
    }

    fn close(&mut self) {
        self.session = None;
    }

    /// The frame at `at_ms` of `path`, reopening only when the source changed.
    #[cfg(feature = "video")]
    fn frame(&mut self, path: &std::path::Path, at_ms: u64, size: ThumbSize) -> Option<RgbaThumb> {
        if self.session.as_ref().is_none_or(|s| s.path() != path) {
            self.session = crate::video::FrameSession::open(path);
        }
        let frame = self.session.as_mut()?.frame_at(at_ms, size);
        if frame.is_none() {
            // A source that answers nothing must not keep a stale pipeline.
            self.close();
        }
        frame
    }

    /// Without a decoder no frame is ever produced — never a synthesised one.
    #[cfg(not(feature = "video"))]
    fn frame(&mut self, path: &std::path::Path, at_ms: u64, size: ThumbSize) -> Option<RgbaThumb> {
        let _ = (path, at_ms, size);
        self.close();
        None
    }
}

fn worker_loop(shared: &Arc<Shared>) {
    let mut video = VideoPipeline::default();
    loop {
        let job = {
            let mut inner = shared.lock();
            loop {
                if inner.shutdown {
                    return;
                }
                let open = video.open_path();
                if let Some(job) = inner.take(open.as_ref()) {
                    break job;
                }
                if open.is_some() {
                    let (next, res) = shared
                        .work
                        .wait_timeout(inner, PIPELINE_IDLE)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    inner = next;
                    if res.timed_out() {
                        video.close();
                    }
                } else {
                    inner = shared
                        .work
                        .wait(inner)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                }
            }
        };
        let key = job.key;
        let cache = if job.transient {
            &shared.transient
        } else {
            &shared.cache
        };
        let slot = run_job(cache, job, &mut video);
        finish(shared, key, slot);
    }
}

/// Record a generated preview, bound the settled results and tell the app.
fn finish(shared: &Shared, key: ThumbKey, slot: Slot) {
    let mut inner = shared.lock();
    inner.in_flight = inner.in_flight.saturating_sub(1);
    inner.slots.insert(key, slot);
    inner.settled.push_back(key);
    while inner.settled.len() > shared.max_settled {
        let Some(old) = inner.settled.pop_front() else {
            break;
        };
        if inner.settled.contains(&old) || inner.queued.contains_key(&old) {
            continue;
        }
        if inner
            .slots
            .get(&old)
            .is_some_and(|s| !matches!(s, Slot::Loading))
        {
            inner.slots.remove(&old);
        }
    }
    drop(inner);
    shared.settled.notify_all();
    let wake = shared
        .wake
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    if let Some(wake) = wake {
        wake();
    }
}

fn settle(
    cache: &ThumbCache,
    key: &ThumbKey,
    thumb: Option<RgbaThumb>,
    badge: FileCategory,
) -> Slot {
    match thumb {
        Some(thumb) => {
            if let Some(png) = generate::encode_png(&thumb) {
                cache.put(key, png);
            }
            Slot::Ready(Arc::new(thumb))
        }
        None => Slot::Badge(badge),
    }
}

fn adopt(cache: &ThumbCache, key: &ThumbKey, png: Vec<u8>) -> Option<Slot> {
    let thumb = generate::decode_png(&png)?;
    cache.put(key, png);
    Some(Slot::Ready(Arc::new(thumb)))
}

fn run_job(cache: &ThumbCache, job: Job, video: &mut VideoPipeline) -> Slot {
    // A prior session (or a peer, via content-addressing) may already hold it.
    if let Some(png) = cache.get(&job.key)
        && let Some(thumb) = generate::decode_png(&png)
    {
        return Slot::Ready(Arc::new(thumb));
    }
    let (key, size, category) = (job.key, job.key.size, job.category);
    match job.work {
        Work::File(path) => settle(
            cache,
            &key,
            generate::generate(&Source::Path(&path), category, size),
            category,
        ),
        Work::Load(load) => {
            let thumb = load().and_then(|b| generate::generate(&Source::Bytes(&b), category, size));
            settle(cache, &key, thumb, category)
        }
        Work::Encoded(load) => load()
            .and_then(|png| adopt(cache, &key, png))
            .unwrap_or(Slot::Badge(category)),
        Work::EncodedOrFile { load, path } => load()
            .and_then(|png| adopt(cache, &key, png))
            .unwrap_or_else(|| {
                settle(
                    cache,
                    &key,
                    generate::generate(&Source::Path(&path), category, size),
                    category,
                )
            }),
        Work::VideoFrame { path, at_ms } => {
            let thumb = video.frame(&path, at_ms, size);
            settle(cache, &key, thumb, category)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// How long a test waits for work to settle. Waits end as soon as it has,
    /// so this only bounds a genuine hang; it is generous because the workers
    /// share a loaded build machine with everything else.
    const SETTLE: Duration = Duration::from_secs(120);

    fn service() -> (tempfile::TempDir, ThumbnailService) {
        let dir = tempfile::tempdir().unwrap();
        let cache = ThumbCache::open(dir.path()).unwrap();
        (dir, ThumbnailService::new(cache, 3, 128))
    }

    fn png(rgb: [u8; 3]) -> Vec<u8> {
        crate::testutil::solid_png(40, 40, rgb)
    }

    /// A service whose single worker is parked inside a loader until the test
    /// releases it, so the queue behind it can be inspected deterministically.
    fn blocked_service() -> (tempfile::TempDir, ThumbnailService, mpsc::Sender<()>) {
        let dir = tempfile::tempdir().unwrap();
        let cache = ThumbCache::open(dir.path()).unwrap();
        let svc = ThumbnailService::with_bounds(cache, 1, 64, 4, 1024);
        let (release, gate) = mpsc::channel::<()>();
        let (started_tx, started) = mpsc::channel::<()>();
        svc.request(ThumbRequest::load(
            ThumbKey::of_bytes(b"gate", 64),
            FileCategory::Image,
            move || {
                started_tx.send(()).unwrap();
                gate.recv().ok();
                None
            },
        ));
        started.recv_timeout(SETTLE).unwrap();
        (dir, svc, release)
    }

    fn order_probe(
        svc: &ThumbnailService,
        name: &str,
        priority: Priority,
        log: &mpsc::Sender<String>,
    ) -> ThumbKey {
        let key = ThumbKey::of_bytes(name.as_bytes(), 64);
        let log = log.clone();
        let name = name.to_owned();
        svc.request(
            ThumbRequest::load(key, FileCategory::Image, move || {
                log.send(name).unwrap();
                Some(png([1, 2, 3]))
            })
            .priority(priority),
        );
        key
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn thumbnail_requests_settle_off_thread_as_ready_or_badge() {
        // Phase `image_file_loads_then_ready_and_second_request_hits`.
        {
            let (_dir, svc) = service();
            let src = tempfile::NamedTempFile::with_suffix(".png").unwrap();
            std::fs::write(
                src.path(),
                crate::testutil::solid_png(300, 200, [40, 90, 200]),
            )
            .unwrap();
            let key = ThumbKey::of_bytes(b"img-key", 128);

            assert!(
                svc.request_file(key, FileCategory::Image, src.path().to_path_buf())
                    .is_loading()
            );
            svc.block_until_idle(SETTLE);
            assert!(svc.is_idle(), "load settled");

            let state = svc.request_file(key, FileCategory::Image, src.path().to_path_buf());
            let img = state.ready().expect("ready after the async load");
            assert!(img.is_valid());
            assert_eq!(img.max_edge(), 128, "downscaled to the target edge");
            assert!(svc.cache().contains(&key), "generation populated the cache");
        }
        // Phase `badge_category_settles_immediately_without_work`.
        {
            let (_dir, svc) = service();
            let key = ThumbKey::of_bytes(b"archive", 128);
            assert!(matches!(
                svc.request_file(key, FileCategory::Archive, PathBuf::from("/x/a.zip")),
                ThumbState::Badge(FileCategory::Archive)
            ));
            assert!(svc.is_idle(), "a badge schedules no background work");
        }
        // Phase `a_broken_image_settles_as_a_badge`.
        {
            let (_dir, svc) = service();
            let src = tempfile::NamedTempFile::with_suffix(".png").unwrap();
            std::fs::write(src.path(), b"not really a png").unwrap();
            let key = ThumbKey::of_bytes(b"broken", 128);
            svc.request_file(key, FileCategory::Image, src.path().to_path_buf());
            svc.block_until_idle(SETTLE);
            assert!(matches!(
                svc.request_file(key, FileCategory::Image, src.path().to_path_buf()),
                ThumbState::Badge(FileCategory::Image)
            ));
        }
        // Phase `content_bytes_are_generated_on_a_worker_and_cached_by_key`.
        {
            let (_dir, svc) = service();
            let bytes = png([200, 10, 10]);
            let address = tilo_content::hash_bytes(&bytes);
            let key = ThumbKey::of_address(&address, 32).unwrap();
            svc.request(ThumbRequest::load(key, FileCategory::Image, move || {
                Some(bytes)
            }));
            svc.block_until_idle(SETTLE);
            let state = svc.peek(&key).expect("requested");
            assert_eq!(state.ready().expect("ready").max_edge(), 32);
            assert!(svc.cache().contains(&key));
        }
        // Phase `encoded_host_thumbnail_is_adopted_without_touching_source`.
        {
            let (_dir, svc) = service();
            let png = crate::generate::encode_png(
                &crate::generate::image_thumb(&png([10, 220, 60]), 128).unwrap(),
            )
            .unwrap();
            let key = ThumbKey::of_hash([7u8; 32], 128);
            svc.request_encoded(key, FileCategory::Image, move || Some(png));
            svc.block_until_idle(SETTLE);
            let state = svc.request_encoded(key, FileCategory::Image, || {
                panic!("must not re-read a warm slot")
            });
            assert!(
                state.ready().is_some(),
                "host-provided thumbnail is adopted"
            );
        }
        // Phase `encoded_thumbnail_loader_never_runs_on_the_requesting_thread`.
        {
            let (_dir, svc) = service();
            let key = ThumbKey::of_hash([8u8; 32], 128);
            let caller = std::thread::current().id();
            let (tx, rx) = mpsc::channel();

            assert!(
                svc.request_encoded(key, FileCategory::Image, move || {
                    tx.send(std::thread::current().id()).unwrap();
                    None
                })
                .is_loading()
            );
            let worker = rx.recv_timeout(SETTLE).unwrap();
            assert_ne!(
                caller, worker,
                "a FUSE/xattr lookup from draw must be dispatched to a thumbnail worker"
            );
            svc.block_until_idle(SETTLE);
            assert!(matches!(
                svc.request_encoded(key, FileCategory::Image, || {
                    panic!("the settled badge must not reload the xattr")
                }),
                ThumbState::Badge(FileCategory::Image)
            ));
        }
    }

    /// Visible work runs before prefetch before background, and within one
    /// priority the newest request runs first.
    #[test]
    fn the_queue_runs_the_most_urgent_newest_request_first() {
        let (_dir, svc, release) = blocked_service();
        let (log, order) = mpsc::channel();
        order_probe(&svc, "background", Priority::Background, &log);
        order_probe(&svc, "visible-old", Priority::Visible, &log);
        order_probe(&svc, "prefetch", Priority::Prefetch, &log);
        order_probe(&svc, "visible-new", Priority::Visible, &log);
        release.send(()).unwrap();
        svc.block_until_idle(SETTLE);
        let ran: Vec<String> = order.try_iter().collect();
        assert_eq!(
            ran,
            ["visible-new", "visible-old", "prefetch", "background"],
            "priority first, newest first within a priority"
        );
    }

    /// Asking again for a queued key never schedules it twice, and asking at a
    /// higher priority moves it ahead of the work it was behind.
    #[test]
    fn a_repeated_request_is_deduplicated_and_can_move_up() {
        let (_dir, svc, release) = blocked_service();
        let (log, order) = mpsc::channel();
        let late = order_probe(&svc, "scrolled-into-view", Priority::Background, &log);
        order_probe(&svc, "prefetch", Priority::Prefetch, &log);
        // Same key, now on screen: the loader passed here must never run.
        assert!(
            svc.request(
                ThumbRequest::load(late, FileCategory::Image, || panic!("deduplicated"))
                    .priority(Priority::Visible)
            )
            .is_loading()
        );
        assert_eq!(svc.pending(), 3, "gate + two queued, not three queued");
        release.send(()).unwrap();
        svc.block_until_idle(SETTLE);
        let ran: Vec<String> = order.try_iter().collect();
        assert_eq!(ran, ["scrolled-into-view", "prefetch"]);
    }

    /// Past the bound the least urgent, oldest request is dropped and
    /// forgotten, so asking for it again queues it afresh.
    #[test]
    fn the_queue_is_bounded_by_dropping_the_least_urgent_request() {
        let (_dir, svc, release) = blocked_service();
        let (log, order) = mpsc::channel();
        let oldest = order_probe(&svc, "oldest-background", Priority::Background, &log);
        for i in 0..4 {
            order_probe(&svc, &format!("visible-{i}"), Priority::Visible, &log);
        }
        assert!(svc.peek(&oldest).is_none(), "dropped and forgotten");
        release.send(()).unwrap();
        svc.block_until_idle(SETTLE);
        let ran: Vec<String> = order.try_iter().collect();
        assert_eq!(ran.len(), 4);
        assert!(!ran.iter().any(|name| name == "oldest-background"));
    }

    #[test]
    fn a_cancelled_request_never_runs() {
        let (_dir, svc, release) = blocked_service();
        let (log, order) = mpsc::channel();
        let gone = order_probe(&svc, "scrolled-away", Priority::Visible, &log);
        assert!(svc.cancel(&gone));
        assert!(svc.peek(&gone).is_none());
        release.send(()).unwrap();
        svc.block_until_idle(SETTLE);
        assert_eq!(order.try_iter().count(), 0);
    }

    /// Settled results in memory are bounded; an evicted one settles again
    /// from the cache without running its generator.
    #[test]
    fn evicted_results_come_back_from_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let svc =
            ThumbnailService::with_bounds(ThumbCache::open(dir.path()).unwrap(), 1, 64, 64, 2);
        let keys: Vec<ThumbKey> = (0..4u8)
            .map(|i| {
                let key = ThumbKey::of_bytes(&[i], 32);
                svc.request(ThumbRequest::load(key, FileCategory::Image, move || {
                    Some(png([i, i, i]))
                }));
                // One at a time, so settle order is request order.
                svc.block_until_idle(SETTLE);
                key
            })
            .collect();
        assert!(svc.peek(&keys[0]).is_none(), "oldest result evicted");
        assert!(svc.peek(&keys[3]).is_some(), "newest result kept");
        svc.request(ThumbRequest::load(keys[0], FileCategory::Image, || {
            panic!("the cache answers, not the generator")
        }));
        svc.block_until_idle(SETTLE);
        assert!(svc.peek(&keys[0]).unwrap().ready().is_some());
    }

    #[test]
    fn a_transient_preview_is_never_persisted() {
        let (_dir, svc) = service();
        let key = ThumbKey::of_bytes(b"/legacy/path.png", 32);
        svc.request(
            ThumbRequest::load(key, FileCategory::Image, || Some(png([9, 9, 9]))).transient(),
        );
        svc.block_until_idle(SETTLE);
        assert!(svc.peek(&key).unwrap().ready().is_some());
        assert!(!svc.cache().contains(&key), "kept out of the preview cache");
    }

    #[test]
    fn the_wake_hook_fires_when_a_preview_settles() {
        let (_dir, svc) = service();
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        svc.set_wake(move || {
            let _ = tx.lock().unwrap().send(());
        });
        svc.request(ThumbRequest::load(
            ThumbKey::of_bytes(b"wake", 32),
            FileCategory::Image,
            || Some(png([5, 5, 5])),
        ));
        rx.recv_timeout(SETTLE).unwrap();
    }

    /// Without a real decoder a video frame settles as a badge — never a
    /// synthesised picture passed off as a frame.
    #[cfg(not(feature = "video"))]
    #[test]
    fn a_video_frame_without_a_decoder_settles_as_a_badge() {
        let (_dir, svc) = service();
        let key = ThumbKey::of_bytes(b"clip", ThumbSize::new(160, 90)).at_frame(500);
        svc.request(ThumbRequest::video_frame(
            key,
            PathBuf::from("/x/clip.mp4"),
            500,
        ));
        svc.block_until_idle(SETTLE);
        assert!(matches!(
            svc.peek(&key),
            Some(ThumbState::Badge(FileCategory::Video))
        ));
    }
}
