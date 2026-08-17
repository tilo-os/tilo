//! Async image loading, decode and cache — keyed by URL.
//!
//! System apps (e.g. Music's album/artist artwork) need to display remote
//! images: real JPEG/WebP/PNG thumbnails fetched from a CDN, decoded to GPU
//! textures and painted via Vello. Doing that on the UI thread would jank every
//! frame, so this store owns a small background thread pool that **fetches**
//! (via a caller-supplied transport) and **decodes** off-thread, caches the
//! decoded [`peniko::ImageData`] keyed by URL, and hands it back on the next
//! frame. Until then the caller paints a placeholder.
//!
//! The store deliberately does **no** networking itself — the toolkit stays
//! HTTP-free. The consumer injects a [`Fetcher`] closure (Music uses a blocking
//! `reqwest` client). A store built with a no-op fetcher never touches the
//! network, so tests and the hermetic offline builds stay deterministic.
//!
//! Decoding uses the `image` crate (JPEG/PNG/WebP), covering the formats
//! YouTube's image CDN serves. Cloning a returned [`peniko::ImageData`] only
//! bumps an `Arc`, so caching + per-frame reuse is cheap.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
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

/// The public, per-URL load state a caller can query to render an honest
/// placeholder: a *loading* skeleton while the fetch/decode is in flight
/// (it resolves to the real image on a later frame), or a *failed* fallback
/// (e.g. a letter monogram) once a fetch/decode has definitively failed.
/// Distinguishing the two is the difference between a skeleton that resolves
/// and a permanent grey placeholder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadState {
    /// Fetch/decode in flight; a real image may still arrive.
    Loading,
    /// Decoded and ready to paint.
    Ready,
    /// Fetch or decode failed; it will not be retried.
    Failed,
}

struct Shared {
    slots: Mutex<HashMap<String, Slot>>,
    /// Count of in-flight (queued or decoding) loads, for `is_idle` /
    /// `block_until_idle` and to know when to keep animation frames flowing.
    pending: AtomicUsize,
}

/// A cheap-to-clone handle to a shared, background image loader/cache.
#[derive(Clone)]
pub struct ImageStore {
    shared: Arc<Shared>,
    tx: Sender<String>,
}

impl ImageStore {
    /// Build a store with the default worker count.
    pub fn new(fetch: Fetcher) -> Self {
        Self::with_workers(fetch, 6)
    }

    /// Build a store with `workers` background fetch/decode threads (min 1).
    pub fn with_workers(fetch: Fetcher, workers: usize) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let shared = Arc::new(Shared {
            slots: Mutex::new(HashMap::new()),
            pending: AtomicUsize::new(0),
        });
        let rx: Arc<Mutex<Receiver<String>>> = Arc::new(Mutex::new(rx));
        for i in 0..workers.max(1) {
            let rx = rx.clone();
            let shared = shared.clone();
            let fetch = fetch.clone();
            let _ = std::thread::Builder::new()
                .name(format!("ui-image-{i}"))
                .spawn(move || worker_loop(rx, shared, fetch));
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
        let mut slots = self.shared.slots.lock().unwrap();
        if let Some(slot) = slots.get(url) {
            return match slot {
                Slot::Ready(img) => Some(img.clone()),
                _ => None,
            };
        }
        slots.insert(url.to_string(), Slot::Loading);
        drop(slots);
        self.shared.pending.fetch_add(1, Ordering::SeqCst);
        // If the workers are gone the send fails; undo the pending bump so
        // `is_idle` doesn't wedge.
        if self.tx.send(url.to_string()).is_err() {
            self.shared.pending.fetch_sub(1, Ordering::SeqCst);
        }
        None
    }

    /// Whether `url` has been requested yet (Ready/Loading/Failed all count).
    pub fn is_known(&self, url: &str) -> bool {
        self.shared.slots.lock().unwrap().contains_key(url)
    }

    /// The current [`LoadState`] for `url`, or `None` if it has never been
    /// requested. Lets a caller paint an honest *loading* skeleton (which
    /// resolves to the real image on a later frame) versus a *failed*
    /// fallback, without a permanent grey placeholder. Unlike [`Self::get`]
    /// this does **not** schedule a load — call `get` first (or in the same
    /// frame) to kick one off.
    pub fn load_state(&self, url: &str) -> Option<LoadState> {
        self.shared.slots.lock().unwrap().get(url).map(|slot| match slot {
            Slot::Loading => LoadState::Loading,
            Slot::Ready(_) => LoadState::Ready,
            Slot::Failed => LoadState::Failed,
        })
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

fn worker_loop(rx: Arc<Mutex<Receiver<String>>>, shared: Arc<Shared>, fetch: Fetcher) {
    loop {
        // Hold the receiver lock only long enough to pull one job.
        let url = {
            let guard = match rx.lock() {
                Ok(g) => g,
                Err(_) => break,
            };
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
            slots.insert(url, slot);
        }
        shared.pending.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Decode encoded image bytes (JPEG/PNG/WebP) into 8-bit RGBA + dimensions.
/// Oversized source images are downscaled so texture memory + GPU upload stay
/// bounded — album art never needs more than a few hundred pixels on screen.
fn decode_rgba(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let img = image::load_from_memory(bytes).ok()?;
    const MAX: u32 = 600;
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
/// crop the overflow) clipped to a rounded rectangle. Mirrors the album-art
/// tile shape system apps draw, so a real cover drops straight into the slot a
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
    let iw = img.width as f64;
    let ih = img.height as f64;
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
    let brush_transform = Affine::translate((dx, dy)) * Affine::scale(scale);

    let clip = RoundedRect::from_rect(rect, radius);
    scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &clip);
    let brush = ImageBrush::new(img.clone());
    scene.fill(Fill::NonZero, transform, &brush, Some(brush_transform), &rect);
    scene.pop_layer();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a solid `w`×`h` RGBA PNG in-memory (no network) so the store's
    /// fetch+decode+cache path is exercised deterministically.
    fn png_bytes(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..(w * h) {
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
        let fetch: Fetcher = Arc::new(move |_url: &str| Some(bytes.clone()));
        let store = ImageStore::new(fetch);

        // First look: not cached → placeholder (None) + a load is scheduled.
        assert!(store.get("http://example.test/cover.png").is_none());
        assert!(!store.is_idle(), "a load is in flight");

        store.block_until_idle(Duration::from_secs(5));
        assert!(store.is_idle(), "load settled");

        let img = store
            .get("http://example.test/cover.png")
            .expect("decoded image is cached after the async load");
        assert_eq!((img.width, img.height), (12, 8));

        // Painting the cached image must not panic.
        let mut scene = Scene::new();
        paint_image_cover(&mut scene, Affine::IDENTITY, Rect::new(0.0, 0.0, 40.0, 40.0), &img, 8.0);
    }

    #[test]
    fn failed_fetch_settles_as_failed_not_pending() {
        let fetch: Fetcher = Arc::new(|_url: &str| None);
        let store = ImageStore::new(fetch);
        assert!(store.get("http://example.test/missing.jpg").is_none());
        store.block_until_idle(Duration::from_secs(5));
        assert!(store.is_idle(), "a failed fetch still settles");
        // Still None, and no repeated scheduling (already known/Failed).
        assert!(store.get("http://example.test/missing.jpg").is_none());
        assert!(store.is_known("http://example.test/missing.jpg"));
    }

    #[test]
    fn load_state_reports_loading_then_ready() {
        let bytes = png_bytes(6, 6, [80, 90, 100]);
        let fetch: Fetcher = Arc::new(move |_url: &str| Some(bytes.clone()));
        let store = ImageStore::new(fetch);
        let url = "http://example.test/icon.png";
        // Unknown until first requested.
        assert_eq!(store.load_state(url), None);
        // get() schedules the load and the slot becomes Loading immediately.
        assert!(store.get(url).is_none());
        assert_eq!(store.load_state(url), Some(LoadState::Loading));
        // Once settled it is Ready and paintable.
        store.block_until_idle(Duration::from_secs(5));
        assert_eq!(store.load_state(url), Some(LoadState::Ready));
        assert!(store.get(url).is_some());
    }

    #[test]
    fn load_state_reports_failed_for_bad_fetch() {
        let fetch: Fetcher = Arc::new(|_url: &str| None);
        let store = ImageStore::new(fetch);
        let url = "http://example.test/broken.jpg";
        assert!(store.get(url).is_none());
        store.block_until_idle(Duration::from_secs(5));
        assert_eq!(store.load_state(url), Some(LoadState::Failed));
    }

    #[test]
    fn empty_url_is_ignored() {
        let fetch: Fetcher = Arc::new(|_url: &str| Some(png_bytes(2, 2, [0, 0, 0])));
        let store = ImageStore::new(fetch);
        assert!(store.get("").is_none());
        assert!(store.is_idle(), "empty url schedules nothing");
    }
}
