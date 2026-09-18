//! Async image loading, decode and cache — keyed by URL. Displaying remote
//! thumbnails on the UI thread would jank every frame, so this store owns a
//! background thread pool that fetches (via a caller-supplied [`Fetcher`])
//! and decodes off-thread, caching the decoded [`peniko::ImageData`] and
//! handing it back on a later frame; until then the caller paints a
//! placeholder.
//!
//! No networking here — the toolkit stays HTTP-free, and a no-op fetcher
//! keeps tests and hermetic offline builds deterministic. Decoding uses the
//! `image` crate; cloning a returned `ImageData` only bumps an `Arc`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect};
use vello::peniko::{
    BlendMode, Blob, Fill, ImageAlphaType, ImageBrush, ImageData as PenikoImage,
    ImageFormat as Format,
};

/// Fetches the raw encoded bytes for a URL, or `None` on failure. Invoked on a
/// background worker thread, so it may block on I/O.
pub type Fetcher = Arc<dyn Fn(&str) -> Option<Vec<u8>> + Send + Sync>;

/// Cache state for one URL.
enum Slot {
    /// A fetch/decode is in flight.
    Loading,
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

fn slot_bytes(slot: &Slot) -> u64 {
    match slot {
        Slot::Ready(image) => u64::from(image.width)
            .saturating_mul(u64::from(image.height))
            .saturating_mul(4),
        Slot::Loading | Slot::Failed => 0,
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
        .filter(|(_, entry)| !matches!(entry.slot, Slot::Loading))
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

struct Shared {
    slots: Mutex<CacheState>,
    /// Count of in-flight (queued or decoding) loads, for `is_idle` /
    /// `block_until_idle` and to know when to keep animation frames flowing.
    pending: AtomicUsize,
}

/// A cheap-to-clone handle to a shared, background image loader/cache.
#[derive(Clone)]
pub struct ImageStore {
    shared: Arc<Shared>,
    tx: SyncSender<String>,
}

impl ImageStore {
    /// Build a store with the default worker count.
    pub fn new(fetch: &Fetcher) -> Self {
        Self::with_workers(fetch, DEFAULT_IMAGE_WORKERS)
    }

    /// Build a store with `workers` background fetch/decode threads, clamped
    /// to 1..=4 so each store has a predictable stack/scheduler footprint.
    pub fn with_workers(fetch: &Fetcher, workers: usize) -> Self {
        let (tx, rx) = std::sync::mpsc::sync_channel::<String>(IMAGE_JOB_QUEUE_LIMIT);
        let shared = Arc::new(Shared {
            slots: Mutex::new(CacheState::default()),
            pending: AtomicUsize::new(0),
        });
        let rx: Arc<Mutex<Receiver<String>>> = Arc::new(Mutex::new(rx));
        for i in 0..workers.clamp(1, MAX_IMAGE_WORKERS) {
            let rx = rx.clone();
            let shared = shared.clone();
            let fetch = fetch.clone();
            let _ = std::thread::Builder::new()
                .name(format!("ui-image-{i}"))
                .spawn(move || worker_loop(&rx, &shared, &fetch));
        }
        Self { shared, tx }
    }

    /// Decoded image for `url`, kicking off an async load the first time a URL
    /// is seen. Returns `None` while loading or after a failure — the caller
    /// paints a placeholder until a later frame returns `Some`.
    pub fn get(&self, url: &str) -> Option<PenikoImage> {
        if url.is_empty() {
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
        slots.slots.insert(
            url.to_string(),
            SlotEntry {
                slot: Slot::Loading,
                last_used,
            },
        );
        drop(slots);
        self.shared.pending.fetch_add(1, Ordering::SeqCst);
        // Never block the UI thread behind image I/O. A full queue removes the
        // provisional Loading slot so a later frame can retry.
        if let Err(error) = self.tx.try_send(url.to_string()) {
            let failed_url = match error {
                TrySendError::Full(url) | TrySendError::Disconnected(url) => url,
            };
            if let Ok(mut slots) = self.shared.slots.lock() {
                slots.slots.remove(&failed_url);
            }
            self.shared.pending.fetch_sub(1, Ordering::SeqCst);
        }
        None
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
        self.shared.pending.load(Ordering::SeqCst)
    }

    /// Whether every requested load has settled.
    pub fn is_idle(&self) -> bool {
        self.pending() == 0
    }

    /// Block until all in-flight loads settle or `timeout` elapses. For the
    /// synchronous `--shots` pre-warm only — never call this on a live app's UI
    /// thread.
    pub fn block_until_idle(&self, timeout: Duration) {
        let start = Instant::now();
        while self.pending() > 0 && start.elapsed() < timeout {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn worker_loop(rx: &Arc<Mutex<Receiver<String>>>, shared: &Arc<Shared>, fetch: &Fetcher) {
    loop {
        // Hold the receiver lock only long enough to pull one job.
        let url = {
            let Ok(guard) = rx.lock() else { break };
            match guard.recv() {
                Ok(u) => u,
                Err(_) => break, // sender dropped: store gone
            }
        };
        let decoded = fetch(&url).and_then(|bytes| decode_rgba(&bytes));
        let slot = match decoded {
            Some((rgba, w, h)) => {
                let blob = Blob::new(Arc::new(rgba));
                Slot::Ready(PenikoImage {
                    data: blob,
                    format: Format::Rgba8,
                    alpha_type: ImageAlphaType::Alpha,
                    width: w,
                    height: h,
                })
            }
            None => Slot::Failed,
        };
        if let Ok(mut slots) = shared.slots.lock() {
            // Replace this request's Loading marker, then reclaim settled
            // entries until both the count and decoded-pixel ceilings fit.
            insert_settled(&mut slots, url, slot);
        }
        shared.pending.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Decode encoded image bytes (JPEG/PNG/WebP) into 8-bit RGBA + dimensions.
/// Oversized sources are downscaled to 600px so texture memory stays bounded
/// — album art never needs more.
fn decode_rgba(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    const MAX: u32 = 600;
    let img = image::load_from_memory(bytes).ok()?;
    let img = if img.width().max(img.height()) > MAX {
        img.resize(MAX, MAX, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    Some((rgba.into_raw(), w, h))
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
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().unwrap();
            writer.write_image_data(&rgba).unwrap();
        }
        out
    }

    #[test]
    fn get_returns_none_then_ready_after_async_decode() {
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

        store.block_until_idle(Duration::from_secs(5));
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

    #[test]
    fn failed_fetch_settles_as_failed_not_pending() {
        let fetch: Fetcher = Arc::new(|_url: &str| None);
        let store = ImageStore::new(&fetch);
        assert!(store.get("http://example.test/missing.jpg").is_none());
        store.block_until_idle(Duration::from_secs(5));
        assert!(store.is_idle(), "a failed fetch still settles");
        // Still None, and no repeated scheduling (already known/Failed).
        assert!(store.get("http://example.test/missing.jpg").is_none());
        assert!(store.is_known("http://example.test/missing.jpg"));
    }

    #[test]
    fn empty_url_is_ignored() {
        let fetch: Fetcher = Arc::new(|_url: &str| Some(png_bytes(2, 2, [0, 0, 0])));
        let store = ImageStore::new(&fetch);
        assert!(store.get("").is_none());
        assert!(store.is_idle(), "empty url schedules nothing");
    }

    #[test]
    fn decoded_cache_has_strict_entry_and_byte_limits() {
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

    #[test]
    fn decoded_cache_evicts_the_least_recently_used_settled_entry() {
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

    #[test]
    fn lru_clock_rebases_before_exhaustion_without_aliasing_live_entries() {
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

    #[test]
    fn decode_downscales_oversized_sources_before_caching() {
        let bytes = png_bytes(700, 2, [20, 40, 60]);
        let (_, width, height) = decode_rgba(&bytes).expect("valid PNG decodes");
        assert!(width <= 600 && height <= 600);
    }
}
