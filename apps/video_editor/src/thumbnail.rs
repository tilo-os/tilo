//! Clip thumbnails: a small representative frame per source, cached.
//!
//! Every visual source gets a thumbnail so the bin and timeline show a picture,
//! not an abstract swatch. Two producers, mirroring the preview:
//!
//!   * **Representative frame (default / `--shots`)** — generated from the
//!     source's palette with the same colour family the preview uses, so it's
//!     deterministic and needs no decoder. Cheap enough to generate on first
//!     request and cache.
//!   * **Real frame (`--features decode`)** — a true decoded frame pulled off
//!     the UI thread from the source at a representative time and cached in
//!     place of the generated one. Falls back to the representative frame while
//!     the bytes aren't hydrated or the pipeline hasn't prerolled.
//!
//! [`ThumbnailCache`] holds one [`Thumb`] per source; the widget blits it.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use crate::media::MediaSource;
use crate::model::MediaId;
use crate::preview::palette_rgb;

/// Default thumbnail resolution (16:9). Small — it's an icon, and the renderer
/// scales it to fit the bin swatch / timeline cell.
pub const THUMB_W: u32 = 160;
pub const THUMB_H: u32 = 90;

/// A decoded/generated thumbnail: tightly packed `width*height*4` RGBA.
#[derive(Clone, Debug, PartialEq)]
pub struct Thumb {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// `true` when this is a real decoded frame (vs. a generated representative).
    pub real: bool,
}

/// Generate a representative thumbnail frame for a `palette` slot: a soft
/// vertical gradient with a horizon band and a gentle corner vignette, in the
/// palette's colour family — reads as a frame, not a flat swatch.
#[allow(
    clippy::many_single_char_names,
    reason = "w/h/r/g/b/x/y are the conventional names for pixel dimensions, colour channels \
              and coordinates in this per-pixel gradient math"
)]
pub fn representative(palette: u8, w: u32, h: u32) -> Thumb {
    let w = w.max(1);
    let h = h.max(1);
    let base = palette_rgb(palette);
    let top = [
        (base.r * 0.55 + 0.45).min(1.0),
        (base.g * 0.55 + 0.45).min(1.0),
        (base.b * 0.55 + 0.45).min(1.0),
    ];
    let bottom = [base.r * 0.32, base.g * 0.32, base.b * 0.32];
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    let (wf, hf) = (f64::from(w), f64::from(h));
    for y in 0..h {
        let yf = f64::from(y) / hf;
        // Vertical gradient top→bottom.
        let mut r = top[0] + (bottom[0] - top[0]) * yf;
        let mut g = top[1] + (bottom[1] - top[1]) * yf;
        let mut b = top[2] + (bottom[2] - top[2]) * yf;
        // A subtle horizon band around 62% height.
        if (0.60..0.68).contains(&yf) {
            r *= 0.82;
            g *= 0.82;
            b *= 0.82;
        }
        for x in 0..w {
            let xf = f64::from(x) / wf;
            // Gentle corner vignette.
            let dx = (xf - 0.5) * 2.0;
            let dy = (yf - 0.5) * 2.0;
            let v = (1.0 - 0.28 * (dx * dx + dy * dy)).clamp(0.0, 1.0);
            let i = ((y * w + x) * 4) as usize;
            if let Some(px) = rgba.get_mut(i..).and_then(<[u8]>::first_chunk_mut::<4>) {
                *px = [
                    ((r * v) * 255.0).round().clamp(0.0, 255.0) as u8,
                    ((g * v) * 255.0).round().clamp(0.0, 255.0) as u8,
                    ((b * v) * 255.0).round().clamp(0.0, 255.0) as u8,
                    255,
                ];
            }
        }
    }
    Thumb {
        width: w,
        height: h,
        rgba,
        real: false,
    }
}

/// Per-source thumbnail cache. A representative frame is generated synchronously
/// on first request (cheap, cached); under `--features decode` a real frame is
/// then extracted off-thread and replaces it.
pub struct ThumbnailCache {
    map: HashMap<MediaId, Arc<Thumb>>,
    /// Sources with a real-frame extraction in flight, so it isn't spawned twice.
    real_requested: std::collections::HashSet<MediaId>,
    /// Delivers real decoded frames from the worker thread; only fed under the
    /// decode feature, but kept unconditionally so `drain` compiles uniformly.
    #[cfg_attr(not(feature = "decode"), allow(dead_code))]
    tx: Sender<(MediaId, Thumb)>,
    rx: Receiver<(MediaId, Thumb)>,
}

impl Default for ThumbnailCache {
    fn default() -> Self {
        Self::new()
    }
}

impl ThumbnailCache {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Self {
            map: HashMap::new(),
            real_requested: std::collections::HashSet::new(),
            tx,
            rx,
        }
    }

    /// Pull completed real-frame extractions into the cache. Call each `sync`.
    pub fn drain(&mut self) {
        while let Ok((id, thumb)) = self.rx.try_recv() {
            self.map.insert(id, Arc::new(thumb));
        }
    }

    /// Ensure a thumbnail exists for `id`: generate the representative frame now
    /// (once), and — under the decode feature, when the bytes are hydrated —
    /// kick off a real-frame extraction to replace it. `at` is the source time
    /// (seconds) to sample a real frame from.
    pub fn ensure(&mut self, id: MediaId, palette: u8, source: &MediaSource, at: f64) {
        self.map
            .entry(id)
            .or_insert_with(|| Arc::new(representative(palette, THUMB_W, THUMB_H)));
        #[cfg(feature = "decode")]
        {
            let already_real = self.map.get(&id).is_some_and(|t| t.real);
            if !already_real && !self.real_requested.contains(&id) && source.is_decodable() {
                self.real_requested.insert(id);
                let tx = self.tx.clone();
                let abs = source.abs_path.clone();
                std::thread::spawn(move || {
                    if let Some(t) = decode_real_frame(&abs, at) {
                        let _ = tx.send((id, t));
                    }
                });
            }
        }
        // Without the decode feature these are inert; silence the unused vars.
        let _ = (source, at);
    }

    /// The thumbnail for `id`, if one has been generated.
    pub fn get(&self, id: MediaId) -> Option<&Arc<Thumb>> {
        self.map.get(&id)
    }

    /// Whether a real-frame extraction is still in flight (so the app keeps
    /// syncing until it lands).
    pub fn has_pending(&self) -> bool {
        // A request is pending until its real frame replaces the representative.
        self.real_requested
            .iter()
            .any(|id| !self.map.get(id).is_some_and(|t| t.real))
    }
}

/// Extract a real decoded frame from `path` at source time `at` (seconds), off
/// the UI thread, through the shared media engine. Only compiled under the
/// decode feature.
#[cfg(feature = "decode")]
fn decode_real_frame(path: &std::path::Path, at: f64) -> Option<Thumb> {
    let mut engine = ::media::MediaEngine::new();
    if !engine.is_real_decode() {
        return None;
    }
    engine.open(crate::media::engine_source(path));
    engine.seek_ms((at * 1000.0).max(0.0) as u64);
    engine.play();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
    while std::time::Instant::now() < deadline {
        engine.poll();
        if let Some(f) = engine.latest_frame() {
            let frame = f.clone();
            engine.pause();
            return Some(Thumb {
                width: frame.width,
                height: frame.height,
                rgba: frame.rgba.as_ref().clone(),
                real: true,
            });
        }
        std::thread::sleep(std::time::Duration::from_millis(40));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn representative_is_a_sized_opaque_frame_in_the_palette_family() {
        let t = representative(0, 32, 18); // a bluish palette slot
        assert_eq!(t.rgba.len(), 32 * 18 * 4);
        assert!(!t.real, "a generated frame is not a real decode");
        assert!(t.rgba.chunks(4).all(|px| px[3] == 255), "opaque");
        // Not a flat black frame — the palette contributes visible colour.
        assert!(
            t.rgba
                .chunks(4)
                .any(|px| px[0] > 8 || px[1] > 8 || px[2] > 8)
        );
        // The top row is lighter than the bottom row (the vertical gradient).
        let top_lum: u32 = t.rgba[0..4].iter().take(3).map(|&v| u32::from(v)).sum();
        let last = (17 * 32 * 4) as usize;
        let bot_lum: u32 = t.rgba[last..last + 3].iter().map(|&v| u32::from(v)).sum();
        assert!(
            top_lum > bot_lum,
            "gradient runs light→dark ({top_lum} vs {bot_lum})"
        );
    }

    #[test]
    fn cache_generates_a_representative_thumb_on_first_request() {
        let mut cache = ThumbnailCache::new();
        let id = uuid::Uuid::new_v4();
        assert!(cache.get(id).is_none());
        let source = MediaSource {
            abs_path: "/some/where.mp4".into(),
            hydration: crate::media::Hydration::Projected,
        };
        cache.ensure(id, 2, &source, 1.0);
        let t = cache.get(id).expect("a thumbnail was generated");
        assert_eq!((t.width, t.height), (THUMB_W, THUMB_H));
        assert!(!t.real, "the default build caches a representative frame");
        // Without the decode feature nothing is pending.
        #[cfg(not(feature = "decode"))]
        assert!(!cache.has_pending());
    }
}
