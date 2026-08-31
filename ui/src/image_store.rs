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
        // A poisoned lock means some other thread already panicked while
        // holding it; there is nothing sane left to cache, so degrade to
        // "no image yet" rather than taking the caller down with it.
        let Ok(mut slots) = self.shared.slots.lock() else {
            return None;
        };
        if let Some(slot) = slots.get(url) {
            return match slot {
                Slot::Ready(img) => Some(img.clone()),
                _ => None,
            };
        }
        slots.insert(url.to_string(), Slot::Loading);
        drop(slots);
        self.shared.pending.fetch_add(1, Ordering::SeqCst);
        // If the workers are gone the send fails; undo the bump so `is_idle` doesn't wedge.
        if self.tx.send(url.to_string()).is_err() {
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
            .map(|slots| slots.contains_key(url))
            .unwrap_or(false)
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
/// Oversized sources are downscaled to 600px so texture memory stays bounded
/// — album art never needs more.
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
        let store = ImageStore::new(fetch);

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
        let store = ImageStore::new(fetch);
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
        let store = ImageStore::new(fetch);
        assert!(store.get("").is_none());
        assert!(store.is_idle(), "empty url schedules nothing");
    }
}
