//! Clip thumbnails, decoded by the shared `thumbnail` pipeline and cached per
//! source and time: one bin swatch per source, and a different frame per cell of
//! a timeline filmstrip, sampled across the clip's trimmed window.
//!
//! A request settles three ways and no fourth: a real frame, a generated
//! representative frame (no decoder, or the bytes not hydrated yet), or failure.
//! A failure is recorded and never retried, so an undecodable source settles
//! instead of pinning [`ThumbnailCache::has_pending`] true forever.

use std::collections::{BTreeMap, HashMap, HashSet};
#[cfg(feature = "decode")]
use std::path::PathBuf;
use std::sync::Arc;
#[cfg(any(feature = "decode", test))]
use thumbnail::ThumbSize;
use thumbnail::{ThumbKey, ThumbState, ThumbnailService};
use vello::peniko::{Blob, ImageData};

use crate::media::MediaSource;
use crate::model::MediaId;
use crate::preview::palette_rgb;

/// Decoded frames are fitted inside this box before they are cached, so a 4K
/// source costs what a 480p one does.
pub const THUMB_W: u32 = 160;
pub const THUMB_H: u32 = 90;
#[cfg(any(feature = "decode", test))]
const THUMB_SIZE: ThumbSize = ThumbSize::new(THUMB_W, THUMB_H);

/// Filmstrip resolution in source time: on a quarter-second grid, neighbouring
/// cells share a decode and a small zoom change reuses what is decoded.
pub const STRIP_BUCKET_SECS: f64 = 0.25;

const MAX_FRAMES_PER_SOURCE: usize = 96;

const MAX_WORKERS: usize = 2;

/// How far [`ThumbnailCache::frame_at`] reaches for a decoded neighbour while
/// the exact bucket is in flight: two seconds, which is the same shot.
const NEAR_BUCKETS: u32 = (2.0 / STRIP_BUCKET_SECS) as u32;

pub fn bucket_of(t: f64) -> u32 {
    (t.max(0.0) / STRIP_BUCKET_SECS)
        .floor()
        .min(f64::from(u32::MAX)) as u32
}

/// The centre of `bucket`, so every request for a bucket names one frame.
#[cfg(any(feature = "decode", test))]
fn bucket_time(bucket: u32) -> f64 {
    (f64::from(bucket) + 0.5) * STRIP_BUCKET_SECS
}

#[cfg(feature = "decode")]
fn millis(t: f64) -> u64 {
    (t.max(0.0) * 1000.0).round().min(u64::MAX as f64) as u64
}

#[derive(Clone, Debug)]
pub struct Thumb {
    pub width: u32,
    pub height: u32,
    /// Shared, so handing the frame to the scene each frame is a refcount bump.
    pub rgba: Arc<Vec<u8>>,
    /// Whether this is a decoded frame rather than a generated stand-in.
    pub real: bool,
    image: ImageData,
}

impl PartialEq for Thumb {
    /// The render handle is derived, so it is not part of the value.
    fn eq(&self, other: &Self) -> bool {
        self.width == other.width
            && self.height == other.height
            && self.real == other.real
            && self.rgba == other.rgba
    }
}

impl Thumb {
    pub fn new(width: u32, height: u32, rgba: Arc<Vec<u8>>, real: bool) -> Self {
        let data: Arc<dyn AsRef<[u8]> + Send + Sync> = rgba.clone();
        let image = ui::raster::rgba8(Blob::new(data), width, height);
        Self {
            width,
            height,
            rgba,
            real,
            image,
        }
    }

    /// Cloned by the renderer, never rebuilt: a clone keeps the same [`Blob`]
    /// identity, so the GPU atlas recognises pixels it has already uploaded. A
    /// fresh `Blob` at the draw call deep-copies the buffer and misses the atlas
    /// every frame.
    pub fn image(&self) -> &ImageData {
        &self.image
    }
}

/// A stand-in frame in a `palette` slot's colour family: a vertical gradient
/// with a horizon band and a corner vignette, so it reads as a frame.
#[expect(
    clippy::many_single_char_names,
    reason = "pixel maths: w/h and x/y are the frame and pixel, r/g/b the channels"
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
    for (y, row) in (0..h).zip(rgba.chunks_exact_mut(w as usize * 4)) {
        let yf = f64::from(y) / hf;
        let mut r = top[0] + (bottom[0] - top[0]) * yf;
        let mut g = top[1] + (bottom[1] - top[1]) * yf;
        let mut b = top[2] + (bottom[2] - top[2]) * yf;
        if (0.60..0.68).contains(&yf) {
            r *= 0.82;
            g *= 0.82;
            b *= 0.82;
        }
        for (x, px) in (0..w).zip(row.as_chunks_mut::<4>().0) {
            let xf = f64::from(x) / wf;
            let dx = (xf - 0.5) * 2.0;
            let dy = (yf - 0.5) * 2.0;
            let v = (1.0 - 0.28 * (dx * dx + dy * dy)).clamp(0.0, 1.0);
            *px = [
                ((r * v) * 255.0).round().clamp(0.0, 255.0) as u8,
                ((g * v) * 255.0).round().clamp(0.0, 255.0) as u8,
                ((b * v) * 255.0).round().clamp(0.0, 255.0) as u8,
                255,
            ];
        }
    }
    Thumb::new(w, h, Arc::new(rgba), false)
}

#[derive(Clone)]
enum Slot {
    /// Asked for under this key and not answered yet.
    #[cfg(any(feature = "decode", test))]
    Pending(ThumbKey),
    Ready(Arc<Thumb>),
    /// The decode produced nothing: never requested again, not pending.
    Failed,
}

impl Slot {
    fn pending_key(&self) -> Option<&ThumbKey> {
        match self {
            #[cfg(any(feature = "decode", test))]
            Self::Pending(key) => Some(key),
            _ => None,
        }
    }

    fn ready(&self) -> Option<&Arc<Thumb>> {
        if let Self::Ready(thumb) = self {
            Some(thumb)
        } else {
            None
        }
    }
}

#[derive(Default)]
struct SourceFrames {
    slots: BTreeMap<u32, Slot>,
    touched: HashMap<u32, u64>,
}

pub struct ThumbnailCache {
    swatch: HashMap<MediaId, Arc<Thumb>>,
    frames: HashMap<MediaId, SourceFrames>,
    swatch_pending: HashMap<MediaId, ThumbKey>,
    undecodable: HashSet<MediaId>,
    failures: HashMap<MediaId, u32>,
    #[cfg(feature = "decode")]
    sources: HashMap<MediaId, (PathBuf, ThumbKey)>,
    clock: u64,
    /// A capture or a past version decodes nothing, so what it draws does not
    /// depend on when a decode lands.
    decoding: bool,
    service: ThumbnailService,
}

impl Default for ThumbnailCache {
    fn default() -> Self {
        Self::new()
    }
}

/// One bad seek near a damaged GOP must not condemn a whole clip.
pub(crate) const FAILURES_BEFORE_GIVING_UP: u32 = 3;

impl ThumbnailCache {
    pub fn new() -> Self {
        // Tests stay hermetic; the app keeps filmstrips in the preview cache, so
        // a reopened project does not decode them again.
        let service = if cfg!(test) || !cfg!(feature = "decode") {
            ThumbnailService::ephemeral(MAX_WORKERS)
        } else {
            ThumbnailService::open_default(MAX_WORKERS)
                .unwrap_or_else(|_| ThumbnailService::ephemeral(MAX_WORKERS))
        };
        Self {
            swatch: HashMap::new(),
            frames: HashMap::new(),
            swatch_pending: HashMap::new(),
            undecodable: HashSet::new(),
            failures: HashMap::new(),
            #[cfg(feature = "decode")]
            sources: HashMap::new(),
            clock: 0,
            decoding: true,
            service,
        }
    }

    pub fn set_decoding(&mut self, decoding: bool) {
        self.decoding = decoding;
    }

    /// Called each sync. A request the pipeline no longer knows, dropped from its
    /// bounded queue, simply stops being pending and the next `ensure_strip` asks
    /// again.
    pub fn drain(&mut self) {
        let mut settled: Vec<(MediaId, u32, Option<ThumbState>)> = Vec::new();
        for (id, key) in &self.swatch_pending {
            match self.service.peek(key) {
                Some(state) if state.is_loading() => {}
                state => settled.push((*id, SWATCH_BUCKET, state)),
            }
        }
        for (id, f) in &self.frames {
            for (bucket, slot) in &f.slots {
                if let Some(key) = slot.pending_key() {
                    match self.service.peek(key) {
                        Some(state) if state.is_loading() => {}
                        state => settled.push((*id, *bucket, state)),
                    }
                }
            }
        }
        for (id, bucket, state) in settled {
            match state {
                None => self.forget_request(id, bucket),
                Some(state) => {
                    let thumb = state.ready().map(|frame| {
                        Thumb::new(
                            frame.width,
                            frame.height,
                            Arc::new(frame.rgba.clone()),
                            true,
                        )
                    });
                    self.settle(id, bucket, thumb);
                }
            }
        }
    }

    fn forget_request(&mut self, id: MediaId, bucket: u32) {
        if bucket == SWATCH_BUCKET {
            self.swatch_pending.remove(&id);
        } else if let Some(f) = self.frames.get_mut(&id) {
            f.slots.remove(&bucket);
        }
    }

    /// `None` is a terminal failure, not "try again later".
    fn settle(&mut self, id: MediaId, bucket: u32, thumb: Option<Thumb>) {
        if let Some(t) = thumb {
            self.failures.remove(&id);
            let t = Arc::new(t);
            if bucket == SWATCH_BUCKET {
                self.swatch_pending.remove(&id);
                self.swatch.insert(id, t);
            } else {
                self.frames
                    .entry(id)
                    .or_default()
                    .slots
                    .insert(bucket, Slot::Ready(t));
            }
        } else {
            // Terminal, never re-requested.
            self.swatch_pending.remove(&id);
            if bucket != SWATCH_BUCKET {
                self.frames
                    .entry(id)
                    .or_default()
                    .slots
                    .insert(bucket, Slot::Failed);
            }
            let n = self.failures.entry(id).or_insert(0);
            *n += 1;
            if *n >= FAILURES_BEFORE_GIVING_UP || bucket == SWATCH_BUCKET {
                self.write_off(id);
            }
        }
        self.evict(id);
    }

    /// Fail every outstanding request for `id` and stop asking, so the app goes
    /// quiet with the representative frame.
    fn write_off(&mut self, id: MediaId) {
        self.undecodable.insert(id);
        if let Some(key) = self.swatch_pending.remove(&id) {
            self.service.cancel(&key);
        }
        if let Some(f) = self.frames.get_mut(&id) {
            for slot in f.slots.values_mut() {
                if let Some(key) = slot.pending_key() {
                    self.service.cancel(key);
                    *slot = Slot::Failed;
                }
            }
        }
    }

    pub fn is_undecodable(&self, id: MediaId) -> bool {
        self.undecodable.contains(&id)
    }

    /// Generate the representative frame once, then ask for a real one at `at`
    /// to replace it when the bytes are hydrated.
    pub fn ensure(&mut self, id: MediaId, palette: u8, source: &MediaSource, at: f64) {
        self.swatch
            .entry(id)
            .or_insert_with(|| Arc::new(representative(palette, THUMB_W, THUMB_H)));
        #[cfg(feature = "decode")]
        {
            let already_real = self.swatch.get(&id).is_some_and(|t| t.real);
            if !already_real && !self.undecodable.contains(&id) {
                self.request(id, source, SWATCH_BUCKET, at);
            }
        }
        let _ = (source, at);
    }

    /// Ask for `cells` samples across the trimmed window `[in_point, out_point]`.
    /// Cheap to call every frame: a bucket ready, pending or failed is not asked
    /// for again.
    pub fn ensure_strip(
        &mut self,
        id: MediaId,
        palette: u8,
        source: &MediaSource,
        in_point: f64,
        out_point: f64,
        cells: usize,
    ) {
        // The stand-in until real frames land, even with no decoder.
        self.swatch
            .entry(id)
            .or_insert_with(|| Arc::new(representative(palette, THUMB_W, THUMB_H)));
        if cells == 0 || self.undecodable.contains(&id) {
            return;
        }
        for bucket in strip_buckets(in_point, out_point, cells) {
            self.touch(id, bucket);
            #[cfg(feature = "decode")]
            self.request(id, source, bucket, bucket_time(bucket));
        }
        let _ = source;
    }

    fn touch(&mut self, id: MediaId, bucket: u32) {
        self.clock += 1;
        let clock = self.clock;
        self.frames
            .entry(id)
            .or_default()
            .touched
            .insert(bucket, clock);
    }

    /// Its path, modification time and size, so an edited file gets new frames.
    /// Recomputed only when the path changes.
    #[cfg(feature = "decode")]
    fn source_key(&mut self, id: MediaId, source: &MediaSource) -> Option<ThumbKey> {
        if let Some((path, key)) = self.sources.get(&id)
            && path == &source.abs_path
        {
            return Some(*key);
        }
        let meta = crate::files::metadata(&source.abs_path).ok()?;
        let mtime_ms = meta
            .modified
            .duration_since(ui::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        let key = ThumbKey::of_path_meta(&source.abs_path, mtime_ms, meta.len, THUMB_SIZE);
        self.sources.insert(id, (source.abs_path.clone(), key));
        Some(key)
    }

    /// Ask unless the bucket is already known or in flight.
    #[cfg(feature = "decode")]
    fn request(&mut self, id: MediaId, source: &MediaSource, bucket: u32, at: f64) {
        if !self.decoding || self.undecodable.contains(&id) || !source.is_decodable() {
            return;
        }
        if bucket == SWATCH_BUCKET {
            if self.swatch_pending.contains_key(&id) {
                return;
            }
        } else if self
            .frames
            .get(&id)
            .is_some_and(|f| f.slots.contains_key(&bucket))
        {
            return;
        }
        let Some(base) = self.source_key(id, source) else {
            return;
        };
        let at_ms = millis(at);
        let key = base.at_frame(at_ms);
        let request = thumbnail::ThumbRequest::video_frame(key, source.abs_path.clone(), at_ms);
        if let ThumbState::Ready(frame) = self.service.request(request) {
            let thumb = Thumb::new(
                frame.width,
                frame.height,
                Arc::new(frame.rgba.clone()),
                true,
            );
            self.settle(id, bucket, Some(thumb));
            return;
        }
        if bucket == SWATCH_BUCKET {
            self.swatch_pending.insert(id, key);
        } else {
            self.frames
                .entry(id)
                .or_default()
                .slots
                .insert(bucket, Slot::Pending(key));
        }
    }

    pub fn get(&self, id: MediaId) -> Option<&Arc<Thumb>> {
        self.swatch.get(&id)
    }

    /// The exact bucket when decoded, else a decoded frame within
    /// `NEAR_BUCKETS`, else the representative. Never a frame from elsewhere in
    /// the clip or another source, and [`Thumb::real`] says which of the three it
    /// is.
    pub fn frame_at(&self, id: MediaId, t: f64) -> Option<&Arc<Thumb>> {
        let want = bucket_of(t);
        if let Some(f) = self.frames.get(&id) {
            if let Some(Slot::Ready(th)) = f.slots.get(&want) {
                return Some(th);
            }
            let lo = want.saturating_sub(NEAR_BUCKETS);
            let hi = want.saturating_add(NEAR_BUCKETS);
            let below = f
                .slots
                .range(lo..want)
                .rev()
                .find_map(|(b, s)| s.ready().map(|th| (want - b, th)));
            let above = f
                .slots
                .range(want..=hi)
                .find_map(|(b, s)| s.ready().map(|th| (b - want, th)));
            let nearest = match (below, above) {
                (Some((db, tb)), Some((da, ta))) => Some(if db <= da { tb } else { ta }),
                (Some((_, tb)), None) => Some(tb),
                (None, Some((_, ta))) => Some(ta),
                (None, None) => None,
            };
            if let Some(th) = nearest {
                return Some(th);
            }
        }
        self.swatch.get(&id)
    }

    /// Whether any decode is outstanding, so the app keeps syncing until it
    /// lands. False once every request has settled, a frame or a recorded
    /// failure: one that stays in flight keeps the app in an idle-sync loop.
    pub fn has_pending(&self) -> bool {
        !self.swatch_pending.is_empty()
            || self
                .frames
                .values()
                .any(|f| f.slots.values().any(|s| s.pending_key().is_some()))
    }

    fn evict(&mut self, id: MediaId) {
        let Some(f) = self.frames.get_mut(&id) else {
            return;
        };
        let mut settled: Vec<u32> = f
            .slots
            .iter()
            .filter(|(_, s)| s.pending_key().is_none())
            .map(|(b, _)| *b)
            .collect();
        if settled.len() > MAX_FRAMES_PER_SOURCE {
            let touched = &f.touched;
            settled.sort_by_key(|b| touched.get(b).copied().unwrap_or(0));
            let drop_n = settled.len() - MAX_FRAMES_PER_SOURCE;
            for b in settled.iter().take(drop_n) {
                f.slots.remove(b);
            }
        }
        if f.touched.len() > MAX_FRAMES_PER_SOURCE * 4 {
            let slots = &f.slots;
            f.touched.retain(|b, _| slots.contains_key(b));
        }
    }
}

/// Each cell at its centre, so cell 0 is not always frame 0.
fn strip_buckets(in_point: f64, out_point: f64, cells: usize) -> Vec<u32> {
    let span = (out_point - in_point).max(0.0);
    let mut buckets: Vec<u32> = (0..cells)
        .map(|i| bucket_of(in_point + ((i as f64 + 0.5) / cells as f64) * span))
        .collect();
    buckets.dedup();
    buckets
}

const SWATCH_BUCKET: u32 = u32::MAX;

#[cfg(test)]
mod tests {
    use super::*;

    fn projected(path: &str) -> MediaSource {
        MediaSource {
            abs_path: path.into(),
            hydration: crate::media::Hydration::Projected,
        }
    }

    fn pending(cache: &mut ThumbnailCache, id: MediaId, bucket: u32) {
        let key = ThumbKey::of_bytes(&bucket.to_le_bytes(), THUMB_SIZE);
        if bucket == SWATCH_BUCKET {
            cache.swatch_pending.insert(id, key);
        } else {
            cache
                .frames
                .entry(id)
                .or_default()
                .slots
                .insert(bucket, Slot::Pending(key));
        }
    }

    #[test]
    fn representative_thumbnails_are_generated_sized_and_stable() {
        {
            let t = representative(0, 32, 18); // a bluish palette slot
            assert_eq!(t.rgba.len(), 32 * 18 * 4);
            assert!(!t.real, "a generated frame is not a real decode");
            assert!(t.rgba.chunks(4).all(|px| px[3] == 255), "opaque");
            assert!(
                t.rgba
                    .chunks(4)
                    .any(|px| px[0] > 8 || px[1] > 8 || px[2] > 8)
            );
            let top_lum: u32 = t.rgba[0..4].iter().take(3).map(|&v| u32::from(v)).sum();
            let last = (17 * 32 * 4) as usize;
            let bot_lum: u32 = t.rgba[last..last + 3].iter().map(|&v| u32::from(v)).sum();
            assert!(
                top_lum > bot_lum,
                "gradient runs light→dark ({top_lum} vs {bot_lum})"
            );
        }
        {
            let mut cache = ThumbnailCache::new();
            let id = uuid::Uuid::new_v4();
            assert!(cache.get(id).is_none());
            cache.ensure(id, 2, &projected("/some/where.mp4"), 1.0);
            let t = cache.get(id).expect("a thumbnail was generated");
            assert_eq!((t.width, t.height), (THUMB_W, THUMB_H));
            assert!(
                !t.real,
                "an un-hydrated source caches a representative frame"
            );
            assert!(
                !cache.has_pending(),
                "nothing was decodable, so nothing is in flight"
            );
        }
        {
            // Every clone must carry the same blob id, or the atlas re-uploads
            // the pixels each time.
            let t = representative(1, 16, 9);
            let first = t.image().clone();
            let second = t.image().clone();
            assert_eq!(
                first.data.id(),
                second.data.id(),
                "cloning the cached handle keeps one identity"
            );
            assert_eq!(first.data.id(), t.image().data.id());
            // A fresh blob over the same bytes would miss the atlas.
            let fresh =
                Blob::new(Arc::new(t.rgba.as_ref().clone()) as Arc<dyn AsRef<[u8]> + Send + Sync>);
            assert_ne!(fresh.id(), first.data.id());
            // The handle points at the thumbnail's own pixels, not a copy.
            assert_eq!(first.data.as_ref(), t.rgba.as_slice());
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn thumbnail_decode_failures_settle_without_condemning_sources() {
        {
            // Mark the requests as `request` does, then deliver the pipeline's
            // empty answer.
            let mut cache = ThumbnailCache::new();
            let id = uuid::Uuid::new_v4();
            let src = projected("/not/decodable.mp4");
            cache.ensure(id, 0, &src, 0.5);
            pending(&mut cache, id, SWATCH_BUCKET);
            pending(&mut cache, id, 7);
            assert!(cache.has_pending(), "the requested frames are in flight");

            cache.settle(id, SWATCH_BUCKET, None);
            assert!(
                !cache.has_pending(),
                "a failed decode is a terminal state, not an endless request"
            );
            assert!(
                cache.is_undecodable(id),
                "the source is written off, not retried"
            );
            assert!(
                matches!(cache.frames[&id].slots[&7], Slot::Failed),
                "outstanding strip requests settle too, rather than hanging"
            );

            // The app's per-frame `ensure` must not resurrect the request.
            cache.ensure(id, 0, &src, 0.5);
            cache.ensure_strip(id, 0, &src, 0.0, 4.0, 12);
            assert!(
                !cache.has_pending(),
                "a written-off source is never re-requested"
            );

            let t = cache.get(id).expect("the representative frame survives");
            assert!(!t.real);
        }
        {
            // Per-bucket failures terminate too, or every failing cell keeps the
            // app awake one bucket at a time.
            let mut cache = ThumbnailCache::new();
            let id = uuid::Uuid::new_v4();
            cache.ensure(id, 0, &projected("/broken.mp4"), 0.5);
            for b in 0..FAILURES_BEFORE_GIVING_UP {
                pending(&mut cache, id, b);
                cache.settle(id, b, None);
            }
            assert!(
                cache.is_undecodable(id),
                "a source that never answers is given up on"
            );
            assert!(!cache.has_pending());
        }
        {
            let mut cache = ThumbnailCache::new();
            let id = uuid::Uuid::new_v4();
            cache.ensure(id, 0, &projected("/clip.mp4"), 0.5);
            cache.settle(id, 3, None);
            assert!(
                !cache.is_undecodable(id),
                "a single failed bucket is not a verdict"
            );
            // The bucket itself is terminal, though.
            assert!(matches!(cache.frames[&id].slots[&3], Slot::Failed));
            assert!(!cache.has_pending());
        }
        {
            // A request the filled queue dropped is unknown to the pipeline, so it
            // must not stay pending here.
            let mut cache = ThumbnailCache::new();
            let id = uuid::Uuid::new_v4();
            pending(&mut cache, id, 4);
            pending(&mut cache, id, SWATCH_BUCKET);
            cache.drain();
            assert!(
                !cache.has_pending(),
                "a dropped request leaves nothing pending"
            );
            assert!(
                !cache.frames[&id].slots.contains_key(&4),
                "and is asked for again, not written off"
            );
            assert!(!cache.is_undecodable(id));
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn thumbnail_strip_samples_real_times_within_bounded_caches() {
        {
            // Three source times give three different pictures.
            let mut cache = ThumbnailCache::new();
            let id = uuid::Uuid::new_v4();
            cache.ensure(id, 0, &projected("/clip.mp4"), 0.5);
            for (t, shade) in [(1.0f64, 10u8), (5.0, 120), (9.0, 240)] {
                let px = vec![shade; 4 * 4 * 4];
                cache.settle(id, bucket_of(t), Some(Thumb::new(4, 4, Arc::new(px), true)));
            }

            let a = cache.frame_at(id, 1.0).expect("frame near 1s").clone();
            let b = cache.frame_at(id, 5.0).expect("frame near 5s").clone();
            let c = cache.frame_at(id, 9.0).expect("frame near 9s").clone();
            assert!(a.real && b.real && c.real, "all three are real decodes");
            assert_ne!(a.rgba[0], b.rgba[0], "1s and 5s are different pictures");
            assert_ne!(b.rgba[0], c.rgba[0], "5s and 9s are different pictures");
            assert_ne!(a.image().data.id(), b.image().data.id());
        }
        {
            // A clip trimmed to [4, 8] asks for buckets inside that window only.
            let mut cache = ThumbnailCache::new();
            let id = uuid::Uuid::new_v4();
            cache.ensure_strip(id, 0, &projected("/clip.mp4"), 4.0, 8.0, 8);
            let touched: Vec<u32> = cache.frames[&id].touched.keys().copied().collect();
            assert!(
                touched.len() >= 4,
                "distinct time buckets, not one: {touched:?}"
            );
            for b in &touched {
                let t = bucket_time(*b);
                assert!(
                    (4.0..=8.5).contains(&t),
                    "bucket {b} at {t}s falls outside the trimmed window"
                );
            }
        }
        {
            let mut cache = ThumbnailCache::new();
            let id = uuid::Uuid::new_v4();
            cache.ensure(id, 3, &projected("/clip.mp4"), 0.5);
            let t = cache.frame_at(id, 12.0).expect("something to draw");
            assert!(
                !t.real,
                "an undecoded cell must not claim to be a real frame"
            );

            // A nearby time reuses the real frame rather than the gradient.
            cache.settle(
                id,
                bucket_of(12.0),
                Some(Thumb::new(2, 2, Arc::new(vec![9u8; 16]), true)),
            );
            let near = cache.frame_at(id, 12.1).expect("nearest real frame");
            assert!(near.real);
            assert_eq!(near.rgba[0], 9);

            // Far away the frame says nothing, so the strip falls back.
            let far = cache.frame_at(id, 90.0).expect("something to draw");
            assert!(
                !far.real,
                "a distant cell must not borrow an unrelated frame"
            );
        }
        {
            assert_eq!(strip_buckets(0.0, 0.1, 6), vec![0], "one bucket, once");
            let buckets = strip_buckets(0.0, 10.0, 8);
            assert_eq!(buckets.len(), 8);
            assert!(buckets.windows(2).all(|w| w[0] < w[1]));
        }
        {
            // Scrubbing visits far more buckets than are decoded; the bookkeeping
            // is pruned too.
            let mut cache = ThumbnailCache::new();
            let id = uuid::Uuid::new_v4();
            for b in 0..(MAX_FRAMES_PER_SOURCE as u32 * 6) {
                cache.touch(id, b);
            }
            cache.settle(id, 0, Some(Thumb::new(1, 1, Arc::new(vec![0u8; 4]), true)));
            assert!(
                cache.frames[&id].touched.len() <= MAX_FRAMES_PER_SOURCE * 4,
                "stamps for buckets holding nothing are forgotten"
            );
        }
        {
            let mut cache = ThumbnailCache::new();
            let id = uuid::Uuid::new_v4();
            for b in 0..(MAX_FRAMES_PER_SOURCE as u32 + 20) {
                cache.touch(id, b);
                cache.settle(id, b, Some(Thumb::new(1, 1, Arc::new(vec![0u8; 4]), true)));
            }
            let ready = cache.frames[&id]
                .slots
                .values()
                .filter(|s| matches!(s, Slot::Ready(_)))
                .count();
            assert!(
                ready <= MAX_FRAMES_PER_SOURCE,
                "a scrolled timeline must not grow without bound ({ready} kept)"
            );
            assert!(
                cache
                    .frame_at(id, bucket_time(MAX_FRAMES_PER_SOURCE as u32 + 19))
                    .is_some()
            );
        }
    }
}
