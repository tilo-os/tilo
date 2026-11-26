//! The off-thread thumbnail service: request a preview, get a placeholder now and the
//! generated image on a later frame.
//!
//! Every preview goes through here. One priority queue feeds one worker pool, behind the
//! content-addressed [`ThumbCache`], so work is never repeated across frames, folders,
//! sessions or peers.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use crate::cache::{ThumbCache, ThumbKey};
use crate::category::FileCategory;
use crate::generate::{self, DEFAULT_EDGE, RgbaThumb, Source, ThumbSize};
use tilo_lock::MutexRecover as _;

pub const DEFAULT_MAX_QUEUED: usize = 512;
pub const DEFAULT_MAX_SETTLED: usize = 2048;
const PIPELINE_IDLE: Duration = Duration::from_secs(20);
const AFFINITY_SCAN: usize = 32;

/// How urgently a preview is wanted; workers take the highest first.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Priority {
    Background,
    Prefetch,
    #[default]
    Visible,
}

#[derive(Clone)]
pub enum ThumbState {
    Loading,
    Ready(Arc<RgbaThumb>),
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

/// A loader run on a worker, never on the requesting thread: reading an xattr or a file
/// may cross a FUSE boundary and wait on a peer.
type Loader = Box<dyn FnOnce() -> Option<Vec<u8>> + Send + 'static>;

enum Work {
    File(PathBuf),
    /// Generate from source bytes a loader produces (a content-store read).
    Load(Loader),
    Encoded(Loader),
    EncodedOrFile {
        load: Loader,
        path: PathBuf,
    },
    VideoFrame {
        path: PathBuf,
        at_ms: u64,
    },
}

impl Work {
    fn video_path(&self) -> Option<&PathBuf> {
        match self {
            Work::VideoFrame { path, .. } => Some(path),
            _ => None,
        }
    }
}

pub struct ThumbRequest {
    key: ThumbKey,
    category: FileCategory,
    priority: Priority,
    transient: bool,
    work: Work,
}

impl ThumbRequest {
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
    work: Condvar,
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
        self.inner.lock_recover()
    }
}

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

    pub fn open_default(workers: usize) -> std::io::Result<Self> {
        // No cache directory and no file system (the page): previews live
        // in memory. `temp_dir` panics there rather than failing.
        if !tilo_paths::available(tilo_paths::Feature::NativePaths) {
            return Ok(Self::ephemeral(workers));
        }
        let dir = tilo_paths::cache_dir().map_or_else(
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
        *self.shared.wake.lock_recover() = Some(Arc::new(wake));
    }

    pub fn edge(&self) -> u32 {
        self.edge
    }

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

    pub fn request_file(&self, key: ThumbKey, category: FileCategory, path: PathBuf) -> ThumbState {
        self.request(ThumbRequest::file(key, category, path))
    }

    pub fn request_encoded(
        &self,
        key: ThumbKey,
        category: FileCategory,
        png: impl FnOnce() -> Option<Vec<u8>> + Send + 'static,
    ) -> ThumbState {
        self.request(ThumbRequest::encoded(key, category, png))
    }

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

    pub fn pending(&self) -> usize {
        self.shared.lock().pending()
    }

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

#[derive(Default)]
struct VideoPipeline {
    session: Option<Box<dyn crate::generate::VideoFrames>>,
}

impl VideoPipeline {
    fn open_path(&self) -> Option<PathBuf> {
        self.session.as_ref().map(|s| s.path().to_path_buf())
    }

    fn close(&mut self) {
        self.session = None;
    }

    /// Without a decoder no frame is ever produced — never a synthesised one.
    fn frame(&mut self, path: &std::path::Path, at_ms: u64, size: ThumbSize) -> Option<RgbaThumb> {
        if self.session.as_ref().is_none_or(|s| s.path() != path) {
            self.session = crate::generate::open_video(path);
        }
        let frame = self.session.as_mut()?.frame_at(at_ms, size);
        if frame.is_none() {
            // A source that answers nothing must not keep a stale pipeline.
            self.close();
        }
        frame
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
    let wake = shared.wake.lock_recover().clone();
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
