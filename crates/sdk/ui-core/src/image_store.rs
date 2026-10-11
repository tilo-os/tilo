//! Async image loading, decode and cache — keyed by URL. Displaying remote
//! thumbnails on the UI thread would jank every frame, so loads run off the
//! UI thread — fetched through a caller-supplied [`Fetcher`] and decoded as
//! [`crate::background::Background`] jobs, or fetched by the host through a
//! [`HostImageLoader`] — and the decoded `peniko::ImageData` is cached and
//! handed back on a later frame; until then the caller paints a placeholder.
//!
//! No networking here — the toolkit stays HTTP-free, and a no-op fetcher
//! keeps tests and hermetic offline builds deterministic. Decoding uses the
//! `image` crate; cloning a returned `ImageData` only bumps an `Arc`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};

use crate::background::Background;
use crate::time::Duration;
use crate::wake::WakeHandle;

use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect};
use vello::peniko::{
    BlendMode, Blob, Fill, ImageAlphaType, ImageBrush, ImageData as PenikoImage,
    ImageFormat as Format,
};

/// Fetches the raw encoded bytes for a URL, or `None` on failure. Invoked as
/// background work, so it may block on I/O.
pub type Fetcher = Arc<dyn Fn(&str) -> Option<Vec<u8>> + Send + Sync>;

/// Cache state for one URL.
enum Slot {
    /// A fetch/decode is in flight.
    Loading(Arc<RequestIdentity>),
    /// Decoded and ready to paint.
    Ready(PenikoImage),
    /// Fetch or decode failed; won't retry.
    Failed,
}

struct SlotEntry {
    slot: Slot,
    last_used: u64,
}

#[derive(Default)]
struct CacheState {
    slots: HashMap<String, SlotEntry>,
    clock: u64,
}

impl CacheState {
    fn next_tick(&mut self) -> u64 {
        if self.clock == u64::MAX {
            self.rebase_lru_clock();
        }
        if let Some(next) = self.clock.checked_add(1) {
            self.clock = next;
            return next;
        }
        // A defensive retry keeps this total even if the cache bound changes
        // in the future: LRU eviction quality may reset, but timestamps never
        // wrap into a value belonging to a live entry.
        self.rebase_lru_clock();
        self.next_tick()
    }

    fn rebase_lru_clock(&mut self) {
        // This is a process-local LRU clock, not a persisted identity.
        // Rebase the (at most 64) live timestamps in their existing order
        // before issuing another tick, so no two cache entries become
        // indistinguishable through counter wraparound.
        let mut order: Vec<_> = self.slots.keys().cloned().collect();
        order.sort_by_key(|url| self.slots.get(url).map_or(0, |entry| entry.last_used));
        for (tick, url) in (1_u64..).zip(order) {
            if let Some(entry) = self.slots.get_mut(&url) {
                entry.last_used = tick;
            }
        }
        self.clock = u64::try_from(self.slots.len()).unwrap_or(u64::MAX);
    }
}

const DEFAULT_IMAGE_WORKERS: usize = 2;
const MAX_IMAGE_WORKERS: usize = 4;
const IMAGE_JOB_QUEUE_LIMIT: usize = 64;
const IMAGE_CACHE_ENTRY_LIMIT: usize = 64;
const IMAGE_CACHE_BYTE_LIMIT: u64 = 64 * 1024 * 1024;
const THUMBNAIL_MAX_DIMENSION: u32 = 600;
// A single RGBA image must fit within the existing 64 MiB cache ceiling.
const IMAGE_MAX_DIMENSION: u32 = 4096;

fn slot_bytes(slot: &Slot) -> u64 {
    match slot {
        Slot::Ready(image) => u64::from(image.width)
            .saturating_mul(u64::from(image.height))
            .saturating_mul(4),
        Slot::Loading(_) | Slot::Failed => 0,
    }
}

fn cache_bytes(cache: &CacheState) -> u64 {
    cache
        .slots
        .values()
        .map(|entry| slot_bytes(&entry.slot))
        .sum()
}

fn evict_one_settled(cache: &mut CacheState) -> bool {
    let victim = cache
        .slots
        .iter()
        .filter(|(_, entry)| !matches!(entry.slot, Slot::Loading(_)))
        .min_by_key(|(_, entry)| entry.last_used)
        .map(|(url, _)| url.clone());
    victim.is_some_and(|url| cache.slots.remove(&url).is_some())
}

fn insert_settled(cache: &mut CacheState, url: String, slot: Slot) {
    cache.slots.remove(&url);
    let incoming_bytes = slot_bytes(&slot);
    while (cache.slots.len() >= IMAGE_CACHE_ENTRY_LIMIT
        || cache_bytes(cache).saturating_add(incoming_bytes) > IMAGE_CACHE_BYTE_LIMIT)
        && evict_one_settled(cache)
    {}
    let last_used = cache.next_tick();
    cache.slots.insert(url, SlotEntry { slot, last_used });
}

/// Loads in flight across every store in the process, so a capture can wait
/// for the thumbnails a frame asked for before it is written
/// ([`loads_in_flight`]).
static LOADS_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// Loads in flight (queued or decoding) across every [`ImageStore`] in the
/// process. A screenshot capture waits, within a bound, until this is zero.
#[must_use]
pub fn loads_in_flight() -> usize {
    LOADS_IN_FLIGHT.load(Ordering::SeqCst)
}

/// One store's in-flight count, mirrored into [`LOADS_IN_FLIGHT`].
struct Pending(AtomicUsize);

impl Pending {
    fn add(&self, n: usize) {
        self.0.fetch_add(n, Ordering::SeqCst);
        LOADS_IN_FLIGHT.fetch_add(n, Ordering::SeqCst);
    }
    fn sub(&self, n: usize) {
        self.0.fetch_sub(n, Ordering::SeqCst);
        LOADS_IN_FLIGHT.fetch_sub(n, Ordering::SeqCst);
    }
    fn get(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Default)]
struct RequestIdentity {
    reserved: AtomicBool,
}

struct Shared {
    slots: Mutex<CacheState>,
    /// Count of in-flight (queued or decoding) loads, for `is_idle` /
    /// `block_until_idle` and to know when to keep animation frames flowing.
    pending: Pending,
    wake: WakeHandle,
}

impl Shared {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            slots: Mutex::new(CacheState::default()),
            pending: Pending(AtomicUsize::new(0)),
            wake: WakeHandle::new(),
        })
    }

    /// Exact request identity, rather than its URL, owns pending custody.
    fn settle(&self, url: &str, request: &Arc<RequestIdentity>, slot: Slot) -> bool {
        let mut cache = self
            .slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !cache.slots.get(url).is_some_and(
            |entry| matches!(&entry.slot, Slot::Loading(current) if Arc::ptr_eq(current, request)),
        ) {
            return false;
        }
        // Authorization belongs at this commit boundary, after fetch/decode and
        // validation. Cleanup of a closed owner removes custody without publishing.
        let authorized = self.wake.authorize_publication();
        if authorized {
            insert_settled(&mut cache, url.to_owned(), slot);
        } else {
            cache.slots.remove(url);
        }
        self.pending.sub(1);
        drop(cache);
        if authorized {
            self.wake.wake();
        }
        authorized
    }
}

/// A cheap-to-clone handle to a shared image cache whose loads run off the
/// UI thread — on the toolkit's [`Background`] for [`ImageStore::new`], or
/// in the host for [`ImageStore::host_driven`].
#[derive(Clone)]
pub struct ImageStore {
    shared: Arc<Shared>,
    tx: SyncSender<String>,
    /// Starts background loads for newly queued requests; `None` when a
    /// [`HostImageLoader`] owns loading.
    native: Option<Arc<NativeLoader>>,
    background: Background,
}

/// The loading half of a host-driven [`ImageStore`].
///
/// The host drains requested URLs with [`take_requests`](Self::take_requests),
/// fetches them however it can (asynchronously, from its own cache, over its
/// own transport) and hands the bytes back through
/// [`complete`](Self::complete), which decodes them under the same size and
/// cache limits as the native loader and wakes the component that owns the
/// store. Dropping the loader settles every unfinished request as failed.
pub struct HostImageLoader {
    shared: Arc<Shared>,
    requests: Option<Receiver<String>>,
    in_flight: HashMap<String, Arc<RequestIdentity>>,
    max_dimension: u32,
}

/// Opaque custody for one admitted asynchronous image decode.
///
/// A ticket identifies this loader's shared cache and this request generation;
/// it grants no publication permission. It is deliberately not cloneable.
/// Dropping it cancels unfinished work, including on a worker thread.
pub struct ImageCompletionTicket {
    shared: Arc<Shared>,
    request: Arc<RequestIdentity>,
    url: String,
    max_dimension: u32,
}

impl ImageCompletionTicket {
    /// The output dimension bound captured for this request.
    pub fn max_dimension(&self) -> u32 {
        self.max_dimension
    }
}

impl Drop for ImageCompletionTicket {
    fn drop(&mut self) {
        self.shared.settle(&self.url, &self.request, Slot::Failed);
    }
}

impl HostImageLoader {
    /// Reserve a request already handed out by this loader for off-thread decode.
    /// Unknown, settled, closed-owner or already reserved requests are rejected.
    pub fn reserve(&mut self, url: &str) -> Option<ImageCompletionTicket> {
        let request = self.in_flight.get(url)?.clone();
        let cache = self.shared.slots.lock().ok()?;
        if !cache.slots.get(url).is_some_and(
            |entry| matches!(&entry.slot, Slot::Loading(current) if Arc::ptr_eq(current, &request)),
        ) {
            return None;
        }
        drop(cache);
        if !self.shared.wake.authorize_publication() {
            self.settle(url, &request, Slot::Failed);
            return None;
        }
        if request.reserved.swap(true, Ordering::AcqRel) {
            return None;
        }
        Some(ImageCompletionTicket {
            shared: self.shared.clone(),
            request,
            url: url.to_owned(),
            max_dimension: self.max_dimension,
        })
    }

    /// Settle an exact reservation with decoded pixels, or `None` on failure.
    /// Validation enforces this ticket's output bound and exact RGBA length.
    /// Foreign, canceled, duplicate and old-generation replies are inert.
    /// A borrowed ticket leaves foreign-loader attempts unable to cancel its owner.
    pub fn complete_decoded(
        &mut self,
        ticket: &ImageCompletionTicket,
        decoded: Option<tilo_image_codec::DecodedImage>,
    ) -> bool {
        if !Arc::ptr_eq(&self.shared, &ticket.shared) {
            return false;
        }
        self.settle(
            &ticket.url,
            &ticket.request,
            validated_slot(decoded, ticket.max_dimension),
        )
    }

    /// Cancel one exact reservation. A late result or later ticket drop is inert.
    /// Closed-owner cleanup releases custody but returns `false` without publication.
    pub fn cancel(&mut self, ticket: &ImageCompletionTicket) -> bool {
        if !Arc::ptr_eq(&self.shared, &ticket.shared) {
            return false;
        }
        self.settle(&ticket.url, &ticket.request, Slot::Failed)
    }

    /// Every URL requested since the last call. Each must eventually be
    /// passed to [`complete`](Self::complete).
    pub fn take_requests(&mut self) -> Vec<String> {
        std::iter::from_fn(|| self.next_request()).collect()
    }

    /// The next requested URL, if any.
    pub fn next_request(&mut self) -> Option<String> {
        loop {
            let url = self.requests.as_ref()?.try_recv().ok()?;
            let slots = self.shared.slots.lock().ok()?;
            let Some(SlotEntry {
                slot: Slot::Loading(request),
                ..
            }) = slots.slots.get(&url)
            else {
                continue;
            };
            self.in_flight.retain(|url, request| {
                slots.slots.get(url).is_some_and(|entry| {
                    matches!(&entry.slot, Slot::Loading(current) if Arc::ptr_eq(current, request))
                })
            });
            self.in_flight.insert(url.clone(), request.clone());
            return Some(url);
        }
    }

    /// Settle a request taken from this loader with fetched bytes, or `None`
    /// on failure. Decoding runs on the calling thread. Reserved asynchronous
    /// requests must instead use [`complete_decoded`](Self::complete_decoded).
    pub fn complete(&mut self, url: &str, bytes: Option<&[u8]>) -> bool {
        let Some(request) = self.in_flight.get(url).cloned() else {
            return false;
        };
        if request.reserved.load(Ordering::Acquire) {
            return false;
        }
        let slot = decoded_slot(bytes, self.max_dimension);
        self.settle(url, &request, slot)
    }

    /// Reopen only a previously admitted failed slot; arbitrary URLs cannot enter here.
    pub fn retry(&mut self, url: &str) -> bool {
        if !self.shared.wake.authorize_publication() {
            return false;
        }
        let Ok(mut slots) = self.shared.slots.lock() else {
            return false;
        };
        let Some(entry) = slots.slots.get_mut(url) else {
            return false;
        };
        if !matches!(entry.slot, Slot::Failed) {
            return false;
        }
        let request = Arc::new(RequestIdentity::default());
        entry.slot = Slot::Loading(request.clone());
        self.in_flight.insert(url.into(), request);
        self.shared.pending.add(1);
        true
    }

    /// Whether `url` has settled with a decoded image.
    pub fn loaded(&self, url: &str) -> bool {
        self.shared.slots.lock().ok().is_some_and(|slots| {
            slots
                .slots
                .get(url)
                .is_some_and(|entry| matches!(entry.slot, Slot::Ready(_)))
        })
    }

    fn settle(&mut self, url: &str, request: &Arc<RequestIdentity>, slot: Slot) -> bool {
        let published = self.shared.settle(url, request, slot);
        if self
            .in_flight
            .get(url)
            .is_some_and(|current| Arc::ptr_eq(current, request))
        {
            self.in_flight.remove(url);
        }
        published
    }
}

impl Drop for HostImageLoader {
    fn drop(&mut self) {
        // Disconnect first: concurrent admission either belongs to the cache
        // sweep below, or observes disconnection and settles its own identity.
        self.requests.take();
        let unfinished: Vec<_> = {
            let slots = self
                .shared
                .slots
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            slots
                .slots
                .iter()
                .filter_map(|(url, entry)| {
                    if let Slot::Loading(request) = &entry.slot {
                        Some((url.clone(), request.clone()))
                    } else {
                        None
                    }
                })
                .collect()
        };
        for (url, request) in unfinished {
            self.shared.settle(&url, &request, Slot::Failed);
        }
    }
}

/// Pairs a [`HostImageLoader`] with a [`Fetcher`] on a [`Background`]: at
/// most `workers` loads run at once, one background job per URL.
struct NativeLoader {
    state: Mutex<NativeState>,
    fetch: Fetcher,
    background: Background,
    workers: usize,
}

struct NativeState {
    loader: HostImageLoader,
    running: usize,
}

impl NativeLoader {
    fn state(&self) -> std::sync::MutexGuard<'_, NativeState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Start a job for each queued request while a worker slot is free.
    fn start_queued(self: &Arc<Self>) {
        loop {
            let ticket = {
                let mut state = self.state();
                if state.running >= self.workers {
                    return;
                }
                let Some(url) = state.loader.next_request() else {
                    return;
                };
                let Some(ticket) = state.loader.reserve(&url) else {
                    continue;
                };
                state.running = state.running.saturating_add(1);
                ticket
            };
            let this = Arc::clone(self);
            let job_url = ticket.url.clone();
            let spawned = self.background.spawn("ui-image", move || {
                let decoded = (this.fetch)(&job_url).as_deref().and_then(|encoded| {
                    tilo_image_codec::DecodeImage {
                        encoded,
                        max_dimension: ticket.max_dimension(),
                    }
                    .decode_native()
                    .ok()
                });
                {
                    let mut state = this.state();
                    state.loader.complete_decoded(&ticket, decoded);
                    state.running = state.running.saturating_sub(1);
                }
                this.start_queued();
            });
            if spawned.is_err() {
                // This host runs no background work: settle instead of
                // leaving the request loading forever.
                let mut state = self.state();
                // Rejected jobs drop their ticket, which settles exact custody.
                state.running = state.running.saturating_sub(1);
            }
        }
    }
}

thread_local! {
    static HOST_FACTORY: std::cell::RefCell<Option<std::rc::Rc<dyn Fn(u32) -> ImageStore>>> = const { std::cell::RefCell::new(None) };
}

impl ImageStore {
    /// Construct app caches under a host's asynchronous service while retaining
    /// the requested decoded-image bound and component wake custody.
    pub fn with_host_factory_with_max_dimension<R>(
        factory: std::rc::Rc<dyn Fn(u32) -> Self>,
        construct: impl FnOnce() -> R,
    ) -> R {
        struct Restore(Option<std::rc::Rc<dyn Fn(u32) -> ImageStore>>);
        impl Drop for Restore {
            fn drop(&mut self) {
                HOST_FACTORY.with_borrow_mut(|slot| *slot = self.0.take());
            }
        }
        let _restore = Restore(HOST_FACTORY.with_borrow_mut(|slot| slot.replace(factory)));
        construct()
    }

    /// Build a store with the default worker count.
    pub fn new(fetch: &Fetcher) -> Self {
        if let Some(factory) = HOST_FACTORY.with_borrow(Clone::clone) {
            return factory(THUMBNAIL_MAX_DIMENSION);
        }
        Self::with_workers(fetch, DEFAULT_IMAGE_WORKERS)
    }

    /// Build a store that runs at most `workers` fetch/decode jobs at once,
    /// clamped to 1..=4 so one store cannot monopolise the background pool.
    pub fn with_workers(fetch: &Fetcher, workers: usize) -> Self {
        Self::with_decode_options(fetch, workers, THUMBNAIL_MAX_DIMENSION)
    }

    /// Build a store for document images instead of small thumbnails.
    ///
    /// Decoded dimensions are bounded to 1..=4096; the worker, queue and
    /// total cache memory limits remain the same as for the thumbnail store.
    pub fn with_max_dimension(fetch: &Fetcher, max_dimension: u32) -> Self {
        let max_dimension = max_dimension.clamp(1, IMAGE_MAX_DIMENSION);
        if let Some(factory) = HOST_FACTORY.with_borrow(Clone::clone) {
            return factory(max_dimension);
        }
        Self::with_decode_options(fetch, DEFAULT_IMAGE_WORKERS, max_dimension)
    }

    /// A thumbnail store whose loading the host drives through the returned
    /// [`HostImageLoader`]; no background work is spawned.
    pub fn host_driven() -> (Self, HostImageLoader) {
        Self::host_driven_with_max_dimension(THUMBNAIL_MAX_DIMENSION)
    }

    /// A host-driven store with a bounded document-image decode dimension
    /// (clamped to 1..=4096).
    pub fn host_driven_with_max_dimension(max_dimension: u32) -> (Self, HostImageLoader) {
        let (tx, requests) = std::sync::mpsc::sync_channel(IMAGE_JOB_QUEUE_LIMIT);
        let shared = Shared::new();
        let loader = HostImageLoader {
            shared: Arc::clone(&shared),
            requests: Some(requests),
            in_flight: HashMap::new(),
            max_dimension: max_dimension.clamp(1, IMAGE_MAX_DIMENSION),
        };
        let store = Self {
            shared,
            tx,
            native: None,
            background: Background::current(),
        };
        (store, loader)
    }

    fn with_decode_options(fetch: &Fetcher, workers: usize, max_dimension: u32) -> Self {
        let (mut store, loader) = Self::host_driven_with_max_dimension(max_dimension);
        store.native = Some(Arc::new(NativeLoader {
            state: Mutex::new(NativeState { loader, running: 0 }),
            fetch: fetch.clone(),
            background: store.background.clone(),
            workers: workers.clamp(1, MAX_IMAGE_WORKERS),
        }));
        store
    }

    /// Decoded image for `url`, kicking off an async load the first time a URL
    /// is seen. Returns `None` while loading or after a failure — the caller
    /// paints a placeholder until a later frame returns `Some`.
    pub fn get(&self, url: &str) -> Option<PenikoImage> {
        if url.is_empty() || !self.shared.wake.authorize_publication() {
            return None;
        }
        // A poisoned lock means some other thread already panicked while
        // holding it; there is nothing sane left to cache, so degrade to
        // "no image yet" rather than taking the caller down with it.
        let Ok(mut slots) = self.shared.slots.lock() else {
            return None;
        };
        let last_used = slots.next_tick();
        if let Some(entry) = slots.slots.get_mut(url) {
            entry.last_used = last_used;
            return match &entry.slot {
                Slot::Ready(img) => Some(img.clone()),
                _ => None,
            };
        }
        if slots.slots.len() >= IMAGE_CACHE_ENTRY_LIMIT && !evict_one_settled(&mut slots) {
            // Every slot is still loading. Avoid an unbounded work queue; a
            // later frame can retry after one of those requests settles.
            return None;
        }
        let request = Arc::new(RequestIdentity::default());
        slots.slots.insert(
            url.to_string(),
            SlotEntry {
                slot: Slot::Loading(request.clone()),
                last_used,
            },
        );
        self.shared.pending.add(1);
        drop(slots);
        // Never block the UI thread behind image I/O. A full queue removes the
        // provisional Loading slot so a later frame can retry; a loader that
        // is gone will never answer, so the request settles as failed.
        match self.tx.try_send(url.to_string()) {
            Ok(()) => {
                if let Some(native) = &self.native {
                    native.start_queued();
                }
            }
            Err(TrySendError::Full(_)) => {
                let mut cache = self
                    .shared
                    .slots
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if cache.slots.get(url).is_some_and(|entry| {
                    matches!(&entry.slot, Slot::Loading(current) if Arc::ptr_eq(current, &request))
                }) {
                    cache.slots.remove(url);
                    self.shared.pending.sub(1);
                }
            }
            Err(TrySendError::Disconnected(_)) => {
                self.shared.settle(url, &request, Slot::Failed);
            }
        }
        None
    }

    /// Forget every load that failed, so the next [`get`](Self::get) of its
    /// URL asks the fetcher again. A failure is cached (it would otherwise be
    /// re-fetched every frame), which is wrong for a source whose bytes were
    /// not available yet and now are: a caller that knows they changed calls
    /// this. Returns whether any failure was forgotten.
    pub fn forget_failed(&self) -> bool {
        let Ok(mut slots) = self.shared.slots.lock() else {
            return false;
        };
        let before = slots.slots.len();
        slots
            .slots
            .retain(|_, entry| !matches!(entry.slot, Slot::Failed));
        slots.slots.len() != before
    }

    /// Whether `url` has been requested yet (Ready/Loading/Failed all count).
    pub fn is_known(&self, url: &str) -> bool {
        // Same poisoned-lock degrade as `get`: unknown is the safe answer.
        self.shared
            .slots
            .lock()
            .is_ok_and(|slots| slots.slots.contains_key(url))
    }

    /// Number of in-flight loads (queued or decoding).
    pub fn pending(&self) -> usize {
        self.shared.pending.get()
    }

    /// Whether every requested load has settled.
    pub fn is_idle(&self) -> bool {
        self.pending() == 0
    }

    /// Block until all in-flight loads settle or `timeout` elapses on the
    /// store's executor clock. For the synchronous `--shots` pre-warm and
    /// tests only — never call this on a live app's UI thread. A host-driven
    /// store settles only when its host completes requests.
    pub fn block_until_idle(&self, timeout: Duration) {
        let start = self.background.now();
        let deadline = start.checked_add(timeout).unwrap_or(start);
        loop {
            let now = self.background.now();
            if self.is_idle() || now >= deadline {
                return;
            }
            // Loads also settle outside the executor's view (a host loader
            // on another thread), so re-check at least this often.
            let slice = now
                .checked_add(Duration::from_millis(10))
                .unwrap_or(deadline);
            self.background.park(slice.min(deadline));
        }
    }
}

fn validated_slot(decoded: Option<tilo_image_codec::DecodedImage>, max_dimension: u32) -> Slot {
    let Some(decoded) = decoded else {
        return Slot::Failed;
    };
    let limits = tilo_image_codec::AdmissionLimits {
        max_dimension,
        max_rgba_bytes: IMAGE_CACHE_BYTE_LIMIT,
        ..tilo_image_codec::AdmissionLimits::default()
    };
    if limits.validate(&decoded).is_err() {
        return Slot::Failed;
    }
    Slot::Ready(PenikoImage {
        data: Blob::new(Arc::new(decoded.rgba)),
        format: Format::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width: decoded.width,
        height: decoded.height,
    })
}

fn decoded_slot(bytes: Option<&[u8]>, max_dimension: u32) -> Slot {
    validated_slot(
        bytes.and_then(|encoded| {
            tilo_image_codec::DecodeImage {
                encoded,
                max_dimension,
            }
            .decode_native()
            .ok()
        }),
        max_dimension,
    )
}

/// Decode encoded image bytes (JPEG/PNG/WebP) into 8-bit RGBA + dimensions.
/// Oversized sources are downscaled after native decoding to the store's output
/// dimension limit; resizing does not bound source decoder allocations.
#[cfg(test)]
fn decode_rgba(bytes: &[u8], max_dimension: u32) -> Option<(Vec<u8>, u32, u32)> {
    let decoded = tilo_image_codec::DecodeImage {
        encoded: bytes,
        max_dimension,
    }
    .decode_native()
    .ok()?;
    Some((decoded.rgba, decoded.width, decoded.height))
}

/// Paint `img` into `rect` with a `cover` fit (fill the rect, preserve aspect,
/// crop the overflow) clipped to a rounded rectangle, matching the shape a
/// procedural placeholder occupied.
pub fn paint_image_cover(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    img: &PenikoImage,
    radius: f64,
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let iw = f64::from(img.width);
    let ih = f64::from(img.height);
    if iw <= 0.0 || ih <= 0.0 {
        return;
    }
    let rw = rect.width();
    let rh = rect.height();
    let scale = (rw / iw).max(rh / ih);
    let dw = iw * scale;
    let dh = ih * scale;
    let dx = rect.x0 + (rw - dw) * 0.5;
    let dy = rect.y0 + (rh - dh) * 0.5;
    // `Affine`'s `Mul` composes two transforms; it has no failure mode, but
    // clippy can't see that for a non-primitive operator overload.
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "Affine's Mul composes transform matrices and cannot overflow/panic"
    )]
    let brush_transform = Affine::translate((dx, dy)) * Affine::scale(scale);

    let clip = RoundedRect::from_rect(rect, radius);
    scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &clip);
    let brush = ImageBrush::new(img.clone());
    scene.fill(
        Fill::NonZero,
        transform,
        &brush,
        Some(brush_transform),
        &rect,
    );
    scene.pop_layer();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forgetting_failures_lets_an_image_whose_bytes_arrived_load() {
        use std::sync::{Arc, RwLock};
        let bytes = Arc::new(RwLock::new(None::<Vec<u8>>));
        let source = Arc::clone(&bytes);
        let fetch: Fetcher = Arc::new(move |_| source.read().ok()?.clone());
        let store = ImageStore::new(&fetch);
        store.get("asset:late");
        store.block_until_idle(Duration::from_secs(2));
        assert!(store.get("asset:late").is_none(), "no bytes yet: failed");
        *bytes.write().unwrap() = Some(png_bytes(4, 3, [9, 9, 9]));
        store.block_until_idle(Duration::from_secs(2));
        assert!(store.get("asset:late").is_none(), "a failure is cached");
        assert!(store.forget_failed());
        assert!(!store.forget_failed(), "nothing left to forget");
        store.get("asset:late");
        store.block_until_idle(Duration::from_secs(2));
        assert!(store.get("asset:late").is_some());
    }

    #[test]
    fn failed_host_image_can_retry_only_its_admitted_request_and_decode_real_bytes() {
        let (store, mut host) = ImageStore::host_driven();
        store.get("retained");
        host.take_requests();
        assert!(host.complete("retained", None));
        assert!(store.get("retained").is_none());
        assert!(!host.retry("unknown"));
        assert!(host.retry("retained"));
        assert!(!host.retry("retained"));
        assert!(host.complete("retained", Some(&png_bytes(9, 7, [1, 2, 3]))));
        assert!(host.loaded("retained"));
        let image = store.get("retained").unwrap();
        assert_eq!((image.width, image.height), (9, 7));
        assert_eq!(store.pending(), 0);
        assert!(!host.retry("retained"));
    }

    fn logical_image(width: u32, height: u32) -> PenikoImage {
        PenikoImage {
            data: Blob::new(Arc::new(vec![0, 0, 0, 0])),
            format: Format::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width,
            height,
        }
    }

    /// Encode a solid `w`×`h` RGBA PNG in-memory (no network) so the store's
    /// fetch+decode+cache path is exercised deterministically.
    fn png_bytes(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
        let pixels = w.saturating_mul(h);
        let mut rgba = Vec::with_capacity(pixels.saturating_mul(4) as usize);
        for _ in 0..pixels {
            rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
        crate::png::encode_rgba8(w, h, &rgba).unwrap()
    }

    #[test]
    fn store_schedules_one_load_per_url_and_settles_ready_or_failed() {
        // Case `get_returns_none_then_ready_after_async_decode`.
        {
            let bytes = png_bytes(12, 8, [10, 200, 30]);
            // Asserting `!is_idle()` right after the miss is a race: the worker
            // may finish this tiny decode first. Counting fetches makes the
            // real claim regardless of who wins that race.
            let fetches = Arc::new(AtomicUsize::new(0));
            let seen = Arc::clone(&fetches);
            let fetch: Fetcher = Arc::new(move |_url: &str| {
                seen.fetch_add(1, Ordering::SeqCst);
                Some(bytes.clone())
            });
            let store = ImageStore::new(&fetch);

            // First look: not cached → placeholder (None) + a load is scheduled.
            assert!(store.get("http://example.test/cover.png").is_none());

            store.block_until_idle(tilo_test_env::hang_bound());
            assert!(store.is_idle(), "load settled");
            assert_eq!(
                fetches.load(Ordering::SeqCst),
                1,
                "the cache miss scheduled exactly one load"
            );

            let img = store
                .get("http://example.test/cover.png")
                .expect("decoded image is cached after the async load");
            assert_eq!((img.width, img.height), (12, 8));

            // Painting the cached image must not panic.
            let mut scene = Scene::new();
            paint_image_cover(
                &mut scene,
                Affine::IDENTITY,
                Rect::new(0.0, 0.0, 40.0, 40.0),
                &img,
                8.0,
            );
        }

        // Case `failed_fetch_settles_as_failed_not_pending`.
        {
            let fetch: Fetcher = Arc::new(|_url: &str| None);
            let store = ImageStore::new(&fetch);
            assert!(store.get("http://example.test/missing.jpg").is_none());
            store.block_until_idle(tilo_test_env::hang_bound());
            assert!(store.is_idle(), "a failed fetch still settles");
            // Still None, and no repeated scheduling (already known/Failed).
            assert!(store.get("http://example.test/missing.jpg").is_none());
            assert!(store.is_known("http://example.test/missing.jpg"));
        }

        // Case `empty_url_is_ignored`.
        {
            let fetch: Fetcher = Arc::new(|_url: &str| Some(png_bytes(2, 2, [0, 0, 0])));
            let store = ImageStore::new(&fetch);
            assert!(store.get("").is_none());
            assert!(store.is_idle(), "empty url schedules nothing");
        }
    }

    #[test]
    fn host_and_background_loaders_settle_only_what_they_run() {
        // Case `host_driven_store_loads_only_what_the_host_completes`.
        {
            let (store, mut loader) = ImageStore::host_driven();
            assert!(store.get("a.png").is_none());
            assert!(store.get("b.png").is_none());
            assert_eq!(store.pending(), 2);
            assert_eq!(loader.take_requests(), ["a.png", "b.png"]);
            assert!(loader.take_requests().is_empty());
            assert!(loader.complete("a.png", Some(&png_bytes(3, 2, [1, 2, 3]))));
            assert!(!loader.complete("a.png", None), "a request settles once");
            assert!(!loader.complete("never-requested.png", None));
            let image = store.get("a.png").expect("completed image is cached");
            assert_eq!((image.width, image.height), (3, 2));
            assert_eq!(store.pending(), 1);
            drop(loader);
            assert!(store.is_idle(), "dropping the loader settles what it left");
            assert!(store.get("b.png").is_none());
            assert!(store.is_known("b.png"));
            assert!(store.get("c.png").is_none());
            assert!(
                store.is_idle(),
                "a store without a loader fails new requests"
            );
        }

        // Case `pumped_background_runs_native_loads_one_job_per_image`.
        {
            let pumped = crate::background::Pumped::new();
            let bytes = png_bytes(4, 4, [9, 9, 9]);
            let fetch: Fetcher = Arc::new(move |_| Some(bytes.clone()));
            let store = {
                let _background = pumped.background().enter();
                ImageStore::with_workers(&fetch, 1)
            };
            let urls = ["one.png", "two.png", "three.png"];
            for url in urls {
                assert!(store.get(url).is_none());
            }
            assert_eq!(store.pending(), 3);
            assert_eq!(pumped.run_pending(), 3);
            assert!(store.is_idle());
            for url in urls {
                assert!(store.get(url).is_some(), "{url} decoded");
            }
            assert!(store.get("four.png").is_none());
            store.block_until_idle(tilo_test_env::hang_bound());
            assert!(store.get("four.png").is_some(), "parking pumps the queue");
        }

        // Case `a_host_without_background_work_settles_loads_as_failed`.
        {
            let fetch: Fetcher = Arc::new(|_| Some(png_bytes(2, 2, [0, 0, 0])));
            let store = {
                let _background = Background::new(crate::background::NoBackground).enter();
                ImageStore::new(&fetch)
            };
            assert!(store.get("x.png").is_none());
            assert!(store.is_idle());
            assert!(store.is_known("x.png"));
        }
    }

    #[test]
    fn decoded_cache_enforces_entry_byte_and_lru_limits() {
        // Case `decoded_cache_has_strict_entry_and_byte_limits`.
        {
            let mut slots = CacheState::default();
            for index in 0..IMAGE_CACHE_ENTRY_LIMIT.saturating_add(5) {
                insert_settled(
                    &mut slots,
                    format!("small-{index}"),
                    Slot::Ready(logical_image(1, 1)),
                );
            }
            assert_eq!(slots.slots.len(), IMAGE_CACHE_ENTRY_LIMIT);

            let mut large_slots = CacheState::default();
            for index in 0..IMAGE_CACHE_ENTRY_LIMIT {
                insert_settled(
                    &mut large_slots,
                    format!("large-{index}"),
                    Slot::Ready(logical_image(600, 600)),
                );
                assert!(cache_bytes(&large_slots) <= IMAGE_CACHE_BYTE_LIMIT);
            }
            assert!(large_slots.slots.len() < IMAGE_CACHE_ENTRY_LIMIT);
        }

        // Case `decoded_cache_evicts_the_least_recently_used_settled_entry`.
        {
            let mut cache = CacheState::default();
            for index in 0..IMAGE_CACHE_ENTRY_LIMIT {
                insert_settled(
                    &mut cache,
                    format!("image-{index}"),
                    Slot::Ready(logical_image(1, 1)),
                );
            }
            let hot_tick = cache.next_tick();
            cache
                .slots
                .get_mut("image-0")
                .expect("seeded entry")
                .last_used = hot_tick;
            insert_settled(
                &mut cache,
                "new-image".into(),
                Slot::Ready(logical_image(1, 1)),
            );
            assert!(cache.slots.contains_key("image-0"));
            assert!(!cache.slots.contains_key("image-1"));
            assert!(cache.slots.contains_key("new-image"));
        }

        // Case `lru_clock_rebases_before_exhaustion_without_aliasing_live_entries`.
        {
            let mut cache = CacheState::default();
            cache.slots.insert(
                "old".into(),
                SlotEntry {
                    slot: Slot::Failed,
                    last_used: 7,
                },
            );
            cache.slots.insert(
                "new".into(),
                SlotEntry {
                    slot: Slot::Failed,
                    last_used: u64::MAX,
                },
            );
            cache.clock = u64::MAX;
            let tick = cache.next_tick();
            let old = cache.slots["old"].last_used;
            let new = cache.slots["new"].last_used;
            assert!(old < new && new < tick);
            assert_ne!(tick, 0);
        }
    }

    #[test]
    fn decode_dimension_limits_fit_thumbnails_documents_and_the_cache_budget() {
        // Case `decode_downscales_oversized_sources_before_caching`.
        {
            let bytes = png_bytes(700, 2, [20, 40, 60]);
            let (_, width, height) =
                decode_rgba(&bytes, THUMBNAIL_MAX_DIMENSION).expect("valid PNG decodes");
            assert!(width <= 600 && height <= 600);
        }

        // Case `document_decode_limit_cannot_exceed_the_cache_budget`.
        {
            let bytes = png_bytes(5000, 2, [20, 40, 60]);
            let (_, width, height) = decode_rgba(&bytes, u32::MAX).unwrap();
            assert_eq!(width, IMAGE_MAX_DIMENSION);
            assert!(u64::from(width) * u64::from(height) * 4 <= IMAGE_CACHE_BYTE_LIMIT);
        }

        // Case `document_store_preserves_scan_resolution_without_changing_thumbnail_defaults`.
        {
            let bytes = png_bytes(1200, 760, [20, 40, 60]);
            let fetch: Fetcher = Arc::new(move |_| Some(bytes.clone()));
            for (store, expected) in [
                (ImageStore::new(&fetch), (600, 380)),
                (ImageStore::with_max_dimension(&fetch, 4096), (1200, 760)),
            ] {
                assert!(store.get("scan.png").is_none());
                store.block_until_idle(tilo_test_env::hang_bound());
                let image = store.get("scan.png").expect("scan must finish decoding");
                assert_eq!((image.width, image.height), expected);
                assert!(cache_bytes(&store.shared.slots.lock().unwrap()) <= IMAGE_CACHE_BYTE_LIMIT);
            }
        }
    }

    #[test]
    fn document_images_can_remain_resident_together_without_redecoding() {
        let bytes = png_bytes(2100, 2100, [20, 40, 60]);
        let fetches = Arc::new(AtomicUsize::new(0));
        let counter = fetches.clone();
        let fetch: Fetcher = Arc::new(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            Some(bytes.clone())
        });
        let store = ImageStore::with_max_dimension(&fetch, 2048);
        assert!(store.get("first-scan.png").is_none());
        assert!(store.get("second-scan.png").is_none());
        store.block_until_idle(tilo_test_env::hang_bound());
        for _ in 0..3 {
            for url in ["first-scan.png", "second-scan.png"] {
                let image = store.get(url).expect("both scans stay resident");
                assert_eq!((image.width, image.height), (2048, 2048));
            }
        }
        assert!(store.is_idle());
        assert_eq!(fetches.load(Ordering::SeqCst), 2);
        assert!(cache_bytes(&store.shared.slots.lock().unwrap()) <= IMAGE_CACHE_BYTE_LIMIT);
    }
}

#[cfg(test)]
mod decoded_completion_tests;
