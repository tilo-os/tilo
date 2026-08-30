//! Clip thumbnails: real frames from the source, cached per (source, time).
//!
//! Two things need pictures, and they need different ones:
//!
//!   * The **bin swatch** — one representative frame per source, sampled near
//!     the start. [`ThumbnailCache::ensure`] serves it.
//!   * The **timeline filmstrip** — a *different* frame per cell, walking the
//!     clip's trimmed `[in_point, out_point]` window so the strip actually
//!     shows the shot changing. [`ThumbnailCache::ensure_strip`] requests
//!     them and [`ThumbnailCache::frame_at`] looks one up per cell. Tiling a
//!     single image across every cell is not a filmstrip; it is one picture
//!     repeated.
//!
//! Frames come from a **bounded pool** of decode workers ([`MAX_WORKERS`]),
//! each owning one pipeline that is reused across every request for the same
//! source. Standing up a whole engine per thumbnail — one GStreamer pipeline
//! per cell — is what made a scrolled timeline unusable.
//!
//! Every request has three terminal outcomes and no fourth: a real frame, a
//! generated representative frame (no decoder compiled, or the bytes are not
//! hydrated yet), or **failure**. Failure is recorded and never retried, so a
//! source that cannot be decoded settles instead of being re-requested every
//! frame — a request with no failure state kept [`ThumbnailCache::has_pending`]
//! true forever and pinned the app in a wake loop.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use vello::peniko::{Blob, ImageAlphaType, ImageData, ImageFormat};

use crate::media::MediaSource;
use crate::model::MediaId;
use crate::preview::palette_rgb;

/// Default thumbnail resolution (16:9). Small — it's an icon, and the renderer
/// scales it to fit the bin swatch / timeline cell. Decoded frames are scaled
/// down to fit inside this box before being cached, so a 4K source costs the
/// same as a 480p one.
pub const THUMB_W: u32 = 160;
pub const THUMB_H: u32 = 90;

/// Filmstrip resolution in source time.
///
/// A clip body asks for one frame per cell; quantising those times onto a
/// quarter-second grid means neighbouring cells share a decode and a small
/// zoom change reuses what is already decoded rather than restarting the
/// pipeline at slightly different timestamps.
pub const STRIP_BUCKET_SECS: f64 = 0.25;

/// Cached frames kept per source. A long clip scrolled end to end would
/// otherwise grow without bound; the least-recently-looked-up bucket goes first.
const MAX_FRAMES_PER_SOURCE: usize = 96;

/// Decode workers across the whole app — and therefore the cap on live
/// pipelines. Each worker keeps one engine and reuses it for consecutive
/// requests against the same source.
#[cfg_attr(not(feature = "decode"), allow(dead_code))]
const MAX_WORKERS: usize = 2;

/// Requested buckets waiting for a worker. Beyond this the oldest request is
/// dropped: it is off-screen by now, and the renderer will ask again if not.
#[cfg_attr(not(feature = "decode"), allow(dead_code))]
const MAX_QUEUED: usize = 64;

/// How long a worker waits for a frame at the seeked position before giving up
/// on that request.
#[cfg_attr(not(feature = "decode"), allow(dead_code))]
const FRAME_TIMEOUT: Duration = Duration::from_secs(6);

/// How long to let a freshly-opened pipeline preroll before seeking it.
#[cfg_attr(not(feature = "decode"), allow(dead_code))]
const PREROLL_TIMEOUT: Duration = Duration::from_secs(5);

/// How far past the requested time a frame may sit and still count as the
/// answer to that seek — a little over one frame at any sane rate.
#[cfg_attr(not(feature = "decode"), allow(dead_code))]
const SEEK_SLOP_MS: u64 = 100;

/// How far from the requested time a frame may sit and still be worth showing
/// when the pipeline could not land on it (a keyframe-only source). Matches
/// the window `frame_at` will reach across for a decoded neighbour.
#[cfg_attr(not(feature = "decode"), allow(dead_code))]
const FALLBACK_SLOP_MS: u64 = 2_000;

/// How long an idle worker holds its pipeline open before retiring. Keeps a
/// scrub responsive without pinning a decoder for a timeline nobody is using.
#[cfg_attr(not(feature = "decode"), allow(dead_code))]
const WORKER_IDLE: Duration = Duration::from_secs(20);

/// How far [`ThumbnailCache::frame_at`] will reach for an already-decoded
/// neighbour while the exact bucket is still in flight. Two seconds of source
/// time: close enough that the stand-in is the same shot, near enough in time
/// that the strip fills in smoothly instead of jumping to a frame from
/// somewhere else in the clip.
const NEAR_BUCKETS: u32 = (2.0 / STRIP_BUCKET_SECS) as u32;

/// The source-time bucket `t` (seconds) falls in.
pub fn bucket_of(t: f64) -> u32 {
    (t.max(0.0) / STRIP_BUCKET_SECS)
        .floor()
        .min(u32::MAX as f64) as u32
}

/// The centre time (seconds) of `bucket`.
#[cfg_attr(not(test), allow(dead_code))]
fn bucket_time(bucket: u32) -> f64 {
    (bucket as f64 + 0.5) * STRIP_BUCKET_SECS
}

/// A decoded/generated thumbnail: tightly packed `width*height*4` RGBA.
#[derive(Clone, Debug)]
pub struct Thumb {
    pub width: u32,
    pub height: u32,
    /// The pixels, shared. `Arc` so handing the same frame to the scene every
    /// frame costs a refcount bump, not a copy of the buffer.
    pub rgba: Arc<Vec<u8>>,
    /// `true` when this is a real decoded frame (vs. a generated representative).
    pub real: bool,
    /// Render handle for `rgba`, built once — see [`Thumb::image`].
    image: ImageData,
}

impl PartialEq for Thumb {
    /// Two thumbnails are equal when their pixels are; the render handle is a
    /// derived cache, not part of the value.
    fn eq(&self, other: &Self) -> bool {
        self.width == other.width
            && self.height == other.height
            && self.real == other.real
            && self.rgba == other.rgba
    }
}

impl Thumb {
    /// Wrap owned pixels, building the render handle once.
    pub fn new(width: u32, height: u32, rgba: Arc<Vec<u8>>, real: bool) -> Self {
        let data: Arc<dyn AsRef<[u8]> + Send + Sync> = rgba.clone();
        let image = ImageData {
            data: Blob::new(data),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width,
            height,
        };
        Self {
            width,
            height,
            rgba,
            real,
            image,
        }
    }

    /// The stable render handle for these pixels.
    ///
    /// Built once at construction and **cloned** by the renderer: a clone
    /// shares the same [`Blob`] identity, so the GPU atlas recognises pixels it
    /// has already uploaded. Constructing a fresh `Blob` at the draw call — as
    /// `Blob::new(Arc::new(thumb.rgba.clone()))` did — both deep-copies the
    /// buffer and mints a new id, so every filmstrip cell missed the atlas and
    /// re-uploaded, every frame.
    pub fn image(&self) -> &ImageData {
        &self.image
    }
}

/// Generate a representative thumbnail frame for a `palette` slot: a soft
/// vertical gradient with a horizon band and a gentle corner vignette, in the
/// palette's colour family — reads as a frame, not a flat swatch.
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
            rgba[i] = ((r * v) * 255.0).round().clamp(0.0, 255.0) as u8;
            rgba[i + 1] = ((g * v) * 255.0).round().clamp(0.0, 255.0) as u8;
            rgba[i + 2] = ((b * v) * 255.0).round().clamp(0.0, 255.0) as u8;
            rgba[i + 3] = 255;
        }
    }
    Thumb::new(w, h, Arc::new(rgba), false)
}

/// Area-average downscale of tightly-packed RGBA into a box no larger than
/// `max_w` x `max_h`, preserving aspect. Caching frames at source resolution
/// would put megabytes per cell in RAM for a 4K clip; the strip only ever
/// draws them a few dozen pixels wide.
#[cfg_attr(not(feature = "decode"), allow(dead_code))]
fn downscale(src: &[u8], sw: u32, sh: u32, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
    if sw == 0 || sh == 0 || src.len() < (sw as usize * sh as usize * 4) {
        return (sw, sh, src.to_vec());
    }
    if sw <= max_w && sh <= max_h {
        return (sw, sh, src.to_vec());
    }
    let scale = (max_w as f64 / sw as f64).min(max_h as f64 / sh as f64);
    let dw = ((sw as f64 * scale).round() as u32).max(1);
    let dh = ((sh as f64 * scale).round() as u32).max(1);
    let mut out = vec![0u8; dw as usize * dh as usize * 4];
    for y in 0..dh {
        let y0 = (y as u64 * sh as u64 / dh as u64) as u32;
        let y1 = (((y + 1) as u64 * sh as u64 / dh as u64) as u32)
            .max(y0 + 1)
            .min(sh);
        for x in 0..dw {
            let x0 = (x as u64 * sw as u64 / dw as u64) as u32;
            let x1 = (((x + 1) as u64 * sw as u64 / dw as u64) as u32)
                .max(x0 + 1)
                .min(sw);
            let (mut r, mut g, mut b, mut a) = (0u32, 0u32, 0u32, 0u32);
            let mut n = 0u32;
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let i = ((sy as usize * sw as usize) + sx as usize) * 4;
                    r += src[i] as u32;
                    g += src[i + 1] as u32;
                    b += src[i + 2] as u32;
                    a += src[i + 3] as u32;
                    n += 1;
                }
            }
            let n = n.max(1);
            let o = ((y as usize * dw as usize) + x as usize) * 4;
            out[o] = (r / n) as u8;
            out[o + 1] = (g / n) as u8;
            out[o + 2] = (b / n) as u8;
            out[o + 3] = (a / n) as u8;
        }
    }
    (dw, dh, out)
}

// ---- the cache ------------------------------------------------------------

/// What we know about one (source, time-bucket) frame.
#[derive(Clone)]
#[cfg_attr(not(feature = "decode"), allow(dead_code))]
enum Slot {
    /// A worker has been asked for it and has not answered yet.
    Pending,
    /// A real decoded frame.
    Ready(Arc<Thumb>),
    /// The decode was attempted and produced nothing. **Terminal** — never
    /// requested again, and not pending.
    Failed,
}

/// One source's frames, keyed by time bucket, plus the LRU stamps used to
/// bound how many are kept.
#[derive(Default)]
struct SourceFrames {
    slots: BTreeMap<u32, Slot>,
    touched: HashMap<u32, u64>,
}

/// A decode request handed to the pool.
#[cfg_attr(not(feature = "decode"), allow(dead_code))]
struct Job {
    id: MediaId,
    path: PathBuf,
    bucket: u32,
    /// Source time to seek to, in seconds.
    at: f64,
}

/// The completed answer to a [`Job`]. `thumb: None` is a real, terminal
/// failure — not "try again later".
struct Outcome {
    id: MediaId,
    bucket: u32,
    thumb: Option<Thumb>,
}

#[derive(Default)]
#[cfg_attr(not(feature = "decode"), allow(dead_code))]
struct Queue {
    jobs: VecDeque<Job>,
    live_workers: usize,
    shutdown: bool,
}

struct Pool {
    queue: Mutex<Queue>,
    wake: Condvar,
}

/// Per-source thumbnail cache.
///
/// A representative frame is generated synchronously on first request (cheap,
/// cached); real frames are decoded by the bounded worker pool and replace it
/// per time bucket.
pub struct ThumbnailCache {
    /// The bin swatch: one frame per source (representative, then real).
    swatch: HashMap<MediaId, Arc<Thumb>>,
    /// Filmstrip frames, per source, per time bucket.
    frames: HashMap<MediaId, SourceFrames>,
    /// Sources with a bin-swatch decode in flight.
    swatch_pending: HashSet<MediaId>,
    /// Sources whose decode has failed outright. Nothing is ever requested for
    /// these again — this is what lets `has_pending` go quiet.
    undecodable: HashSet<MediaId>,
    /// Consecutive failures per source, before it is written off entirely.
    failures: HashMap<MediaId, u32>,
    /// Monotonic stamp for LRU eviction.
    clock: u64,
    pool: Arc<Pool>,
    #[cfg_attr(not(any(feature = "decode", test)), allow(dead_code))]
    tx: Sender<Outcome>,
    rx: Receiver<Outcome>,
}

impl Default for ThumbnailCache {
    fn default() -> Self {
        Self::new()
    }
}

/// A source is written off after this many consecutive failed decodes. One
/// bad seek near a damaged GOP should not condemn a whole clip; a file that
/// answers nothing anywhere is not decodable.
const FAILURES_BEFORE_GIVING_UP: u32 = 3;

impl ThumbnailCache {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Self {
            swatch: HashMap::new(),
            frames: HashMap::new(),
            swatch_pending: HashSet::new(),
            undecodable: HashSet::new(),
            failures: HashMap::new(),
            clock: 0,
            pool: Arc::new(Pool {
                queue: Mutex::new(Queue::default()),
                wake: Condvar::new(),
            }),
            tx,
            rx,
        }
    }

    /// Pull completed decodes into the cache. Call each `sync`.
    pub fn drain(&mut self) {
        while let Ok(Outcome { id, bucket, thumb }) = self.rx.try_recv() {
            match thumb {
                Some(t) => {
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
                }
                None => {
                    // Terminal. Record it so the slot stops being pending and
                    // is never re-requested.
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
            }
            self.evict(id);
        }
    }

    /// Give up on `id`: mark every outstanding request for it failed and stop
    /// asking. The representative frame stays, permanently, and the app goes
    /// quiet instead of re-requesting a decode that will never succeed.
    fn write_off(&mut self, id: MediaId) {
        self.undecodable.insert(id);
        self.swatch_pending.remove(&id);
        if let Some(f) = self.frames.get_mut(&id) {
            for slot in f.slots.values_mut() {
                if matches!(slot, Slot::Pending) {
                    *slot = Slot::Failed;
                }
            }
        }
        self.pool.queue.lock().unwrap().jobs.retain(|j| j.id != id);
    }

    /// Whether `id` has been written off as undecodable — the renderer can say
    /// so honestly instead of waiting for a frame that is not coming.
    pub fn is_undecodable(&self, id: MediaId) -> bool {
        self.undecodable.contains(&id)
    }

    /// Ensure a bin swatch exists for `id`: generate the representative frame
    /// now (once), and — under the decode feature, when the bytes are hydrated
    /// — request a real frame at source time `at` to replace it.
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

    /// Request the frames a clip body needs: `cells` evenly spaced samples
    /// across the clip's trimmed source window `[in_point, out_point]`.
    ///
    /// Idempotent and cheap to call every frame — a bucket already ready,
    /// pending or failed is not re-requested.
    pub fn ensure_strip(
        &mut self,
        id: MediaId,
        palette: u8,
        source: &MediaSource,
        in_point: f64,
        out_point: f64,
        cells: usize,
    ) {
        // The representative frame is the strip's stand-in until real frames
        // land, so make sure it exists even on a build with no decoder.
        self.swatch
            .entry(id)
            .or_insert_with(|| Arc::new(representative(palette, THUMB_W, THUMB_H)));
        if cells == 0 || self.undecodable.contains(&id) {
            return;
        }
        let span = (out_point - in_point).max(0.0);
        for i in 0..cells {
            // Sample each cell at its centre, so cell 0 is not always frame 0.
            let f = (i as f64 + 0.5) / cells as f64;
            let t = in_point + f * span;
            let bucket = bucket_of(t);
            self.touch(id, bucket);
            #[cfg(feature = "decode")]
            self.request(id, source, bucket, t);
        }
        let _ = source;
    }

    /// Note that `bucket` is being looked at, for LRU eviction.
    fn touch(&mut self, id: MediaId, bucket: u32) {
        self.clock += 1;
        let clock = self.clock;
        self.frames
            .entry(id)
            .or_default()
            .touched
            .insert(bucket, clock);
    }

    /// Queue a decode for `(id, bucket)` unless it is already known or in
    /// flight. Only reached under the decode feature.
    #[cfg(feature = "decode")]
    fn request(&mut self, id: MediaId, source: &MediaSource, bucket: u32, at: f64) {
        if self.undecodable.contains(&id) || !source.is_decodable() {
            return;
        }
        // Already answered, already failed, or already in flight.
        if bucket == SWATCH_BUCKET {
            if self.swatch_pending.contains(&id) {
                return;
            }
        } else if self
            .frames
            .get(&id)
            .is_some_and(|f| f.slots.contains_key(&bucket))
        {
            return;
        }

        // Enqueue *first*. A backlogged queue means this request is simply not
        // taken this frame; the slot must not be left marked pending for a job
        // that was never queued, or it would never reach a terminal state and
        // `has_pending` would stay true forever — the very loop this cache
        // exists to avoid. Other queued work keeps the app syncing, so the
        // renderer's next `ensure_strip` asks again.
        {
            let mut q = self.pool.queue.lock().unwrap();
            if q.jobs.len() >= MAX_QUEUED {
                return;
            }
            q.jobs.push_back(Job {
                id,
                path: source.abs_path.clone(),
                bucket,
                at,
            });
            if q.live_workers < MAX_WORKERS {
                q.live_workers += 1;
                let pool = self.pool.clone();
                let tx = self.tx.clone();
                std::thread::spawn(move || run_worker(pool, tx));
            }
        }
        self.pool.wake.notify_one();

        if bucket == SWATCH_BUCKET {
            self.swatch_pending.insert(id);
        } else {
            self.frames
                .entry(id)
                .or_default()
                .slots
                .insert(bucket, Slot::Pending);
        }
    }

    /// Decode `id`'s frames **synchronously**: the bin swatch at `at`, and —
    /// when `cells` is non-zero — the filmstrip across `[in_point, out_point]`,
    /// all through one reused pipeline.
    ///
    /// The live app never blocks on a decoder; this exists for the
    /// deterministic `--shots` capture and for tests, mirroring
    /// [`crate::waveform::WaveformCache::warm_blocking`]. A one-shot render
    /// has no frames to spare for the pool to finish in, and racing it means
    /// the capture shows whichever of the two won — which is how a shot ends
    /// up with the representative gradient where the real frame belongs.
    ///
    /// Failure is recorded exactly as the pool records it, so a source that
    /// cannot be decoded is written off here too rather than retried.
    pub fn warm_blocking(
        &mut self,
        id: MediaId,
        palette: u8,
        source: &MediaSource,
        at: f64,
        in_point: f64,
        out_point: f64,
        cells: usize,
    ) {
        self.swatch
            .entry(id)
            .or_insert_with(|| Arc::new(representative(palette, THUMB_W, THUMB_H)));
        if !source.is_decodable() || self.undecodable.contains(&id) {
            return;
        }
        let want_swatch = !self.swatch.get(&id).map(|t| t.real).unwrap_or(false);
        // Which filmstrip buckets are still unanswered.
        let mut wanted: Vec<(u32, f64)> = Vec::new();
        let span = (out_point - in_point).max(0.0);
        for i in 0..cells {
            let t = in_point + ((i as f64 + 0.5) / cells as f64) * span;
            let bucket = bucket_of(t);
            self.touch(id, bucket);
            let known = self
                .frames
                .get(&id)
                .and_then(|f| f.slots.get(&bucket))
                .is_some_and(|s| !matches!(s, Slot::Pending));
            if !known && !wanted.iter().any(|(b, _)| *b == bucket) {
                wanted.push((bucket, t));
            }
        }
        if !want_swatch && wanted.is_empty() {
            return;
        }
        let Some(mut session) = Session::open(&source.abs_path) else {
            self.write_off(id);
            return;
        };
        if want_swatch {
            match session.frame_at(at) {
                Some(t) => {
                    self.swatch_pending.remove(&id);
                    self.swatch.insert(id, Arc::new(t));
                }
                None => {
                    self.write_off(id);
                    return;
                }
            }
        }
        for (bucket, t) in wanted {
            let slot = match session.frame_at(t) {
                Some(th) => Slot::Ready(Arc::new(th)),
                None => Slot::Failed,
            };
            self.frames
                .entry(id)
                .or_default()
                .slots
                .insert(bucket, slot);
        }
        self.evict(id);
    }

    /// The bin swatch for `id`, if one has been generated.
    pub fn get(&self, id: MediaId) -> Option<&Arc<Thumb>> {
        self.swatch.get(&id)
    }

    /// The filmstrip frame to draw for source time `t` (seconds).
    ///
    /// Exact bucket when it has been decoded; otherwise a decoded frame from
    /// the same source within [`NEAR_BUCKETS`] (a real frame a moment away,
    /// while the exact one is still decoding); otherwise the generated
    /// representative. Never a frame from elsewhere in the clip, never one
    /// from a different source, and never invented pixels dressed as a decode
    /// — [`Thumb::real`] always tells the truth about which of the three it is.
    pub fn frame_at(&self, id: MediaId, t: f64) -> Option<&Arc<Thumb>> {
        let want = bucket_of(t);
        if let Some(f) = self.frames.get(&id) {
            if let Some(Slot::Ready(th)) = f.slots.get(&want) {
                return Some(th);
            }
            let lo = want.saturating_sub(NEAR_BUCKETS);
            let hi = want.saturating_add(NEAR_BUCKETS);
            let below = f.slots.range(lo..want).rev().find_map(|(b, s)| match s {
                Slot::Ready(th) => Some((want - b, th)),
                _ => None,
            });
            let above = f.slots.range(want..=hi).find_map(|(b, s)| match s {
                Slot::Ready(th) => Some((b - want, th)),
                _ => None,
            });
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

    /// Whether any decode is still outstanding, so the app keeps syncing until
    /// it lands.
    ///
    /// Goes **false** once every request has reached a terminal state — a
    /// frame or a recorded failure. A failed decode that left its request
    /// permanently in flight is what kept this true forever and burned CPU in
    /// an endless idle-sync loop.
    pub fn has_pending(&self) -> bool {
        !self.swatch_pending.is_empty()
            || self
                .frames
                .values()
                .any(|f| f.slots.values().any(|s| matches!(s, Slot::Pending)))
    }

    /// Bound what one source keeps: drop the least-recently-looked-at settled
    /// frames past the cap, and forget LRU stamps for buckets that hold
    /// nothing. Scrubbing a long clip visits unboundedly many buckets, so both
    /// halves have to be pruned, not just the pixels.
    fn evict(&mut self, id: MediaId) {
        let Some(f) = self.frames.get_mut(&id) else {
            return;
        };
        // A pending slot is in flight and must survive; anything settled is
        // fair game.
        let mut settled: Vec<u32> = f
            .slots
            .iter()
            .filter(|(_, s)| !matches!(s, Slot::Pending))
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

/// Sentinel bucket for the single bin-swatch frame, which is not part of the
/// filmstrip grid.
const SWATCH_BUCKET: u32 = u32::MAX;

impl Drop for ThumbnailCache {
    fn drop(&mut self) {
        let mut q = self.pool.queue.lock().unwrap();
        q.shutdown = true;
        q.jobs.clear();
        drop(q);
        self.pool.wake.notify_all();
    }
}

/// Take the next job, preferring one for `preferred` so a worker keeps reusing
/// the pipeline it already has open.
#[cfg_attr(not(feature = "decode"), allow(dead_code))]
fn take_job(q: &mut Queue, preferred: Option<&Path>) -> Option<Job> {
    if let Some(p) = preferred {
        if let Some(i) = q.jobs.iter().position(|j| j.path == p) {
            return q.jobs.remove(i);
        }
    }
    q.jobs.pop_front()
}

/// One worker: owns at most one engine, reuses it across consecutive requests
/// for the same source, and retires after [`WORKER_IDLE`] with nothing to do.
#[cfg_attr(not(feature = "decode"), allow(dead_code))]
fn run_worker(pool: Arc<Pool>, tx: Sender<Outcome>) {
    let mut session: Option<Session> = None;
    loop {
        let job = {
            let mut guard = pool.queue.lock().unwrap();
            if guard.shutdown {
                guard.live_workers = guard.live_workers.saturating_sub(1);
                return;
            }
            let want = session.as_ref().map(|s| s.path.clone());
            let mut picked = take_job(&mut guard, want.as_deref());
            if picked.is_none() {
                let (mut g, res) = pool.wake.wait_timeout(guard, WORKER_IDLE).unwrap();
                if g.shutdown {
                    g.live_workers = g.live_workers.saturating_sub(1);
                    return;
                }
                picked = take_job(&mut g, want.as_deref());
                if picked.is_none() {
                    if res.timed_out() {
                        g.live_workers = g.live_workers.saturating_sub(1);
                        return;
                    }
                    continue; // spurious wake
                }
            }
            match picked {
                Some(j) => j,
                None => continue,
            }
        };

        // Reuse the open pipeline when the source has not changed.
        let reopen = session.as_ref().map(|s| s.path != job.path).unwrap_or(true);
        if reopen {
            session = Session::open(&job.path);
        }
        let thumb = match session.as_mut() {
            Some(s) => s.frame_at(job.at),
            None => None,
        };
        if thumb.is_none() {
            // A source that cannot even be opened must not keep a stale engine.
            session = None;
        }
        if tx
            .send(Outcome {
                id: job.id,
                bucket: job.bucket,
                thumb,
            })
            .is_err()
        {
            return; // the cache is gone
        }
    }
}

/// One open source: the engine plus the last frame timestamp it handed back,
/// so a fresh seek can tell a new picture from the one already on screen.
#[cfg_attr(not(feature = "decode"), allow(dead_code))]
struct Session {
    path: PathBuf,
    engine: ::media::MediaEngine,
    last_pts: Option<u64>,
}

#[cfg_attr(not(feature = "decode"), allow(dead_code))]
impl Session {
    fn open(path: &Path) -> Option<Session> {
        let mut engine = ::media::MediaEngine::new();
        if !engine.is_real_decode() {
            // No real decoder at runtime. Say so once, terminally, rather than
            // letting every request time out.
            return None;
        }
        engine.open(crate::media::engine_source(path));
        // Grabbing a frame drives the pipeline with `play`, and the shared
        // engine always builds a real audio sink — so an unmuted thumbnail
        // worker plays the clip out loud and holds an output device while it
        // seeks. A thumbnail is a picture; it must never make a sound.
        engine.set_muted(true);
        engine.set_volume(0.0);
        let mut session = Session {
            path: path.to_path_buf(),
            engine,
            last_pts: None,
        };
        session.await_preroll();
        Some(session)
    }

    /// Let the pipeline finish coming up before anything is asked of it.
    ///
    /// `open` leaves the pipeline prerolling, and a seek issued into a
    /// pipeline that has not prerolled is dropped — the sink then still holds
    /// the preroll picture at position 0, so the first frame each worker asked
    /// for came back as the start of the clip whatever time it wanted. The
    /// preroll timestamp is remembered so that picture can never be mistaken
    /// for the answer to a later seek.
    fn await_preroll(&mut self) {
        let deadline = Instant::now() + PREROLL_TIMEOUT;
        while Instant::now() < deadline {
            self.engine.poll();
            if let Some(f) = self.engine.latest_frame() {
                self.last_pts = Some(f.pts_ms);
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Seek to `at` seconds and return the frame there, scaled to thumbnail
    /// size. `None` only when the pipeline produced no picture near that time.
    fn frame_at(&mut self, at: f64) -> Option<Thumb> {
        let target_ms = (at * 1000.0).max(0.0) as u64;
        self.engine.seek_ms(target_ms);
        self.engine.play();
        let deadline = Instant::now() + FRAME_TIMEOUT;
        let mut fallback: Option<(u32, u32, Arc<Vec<u8>>, u64)> = None;
        while Instant::now() < deadline {
            self.engine.poll();
            if let Some(f) = self.engine.latest_frame() {
                let got = (f.width, f.height, f.rgba.clone(), f.pts_ms);
                // A frame answers *this* seek only when its own timestamp says
                // so. Accepting whatever picture happened to be on the sink
                // hands back the preroll frame at position 0 on a
                // freshly-opened source — every worker's first cell would show
                // the start of the clip no matter which time it asked for.
                let answers_seek = got.3 + SEEK_SLOP_MS >= target_ms;
                let fresh = Some(got.3) != self.last_pts;
                if answers_seek && fresh {
                    self.engine.pause();
                    self.last_pts = Some(got.3);
                    return Some(scaled_thumb(&got.2, got.0, got.1));
                }
                // Keep the closest thing seen, in case the pipeline can only
                // land before the requested time.
                let better = fallback
                    .as_ref()
                    .map(|(_, _, _, p)| got.3.abs_diff(target_ms) < p.abs_diff(target_ms))
                    .unwrap_or(true);
                if better {
                    fallback = Some(got);
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        self.engine.pause();
        // The pipeline never landed on the requested time. A frame close by is
        // still a real frame of this shot — a source that can only seek to
        // keyframes looks exactly like this — but one from somewhere else
        // entirely is not an answer, so report nothing and let the cell show
        // the representative instead of the wrong picture.
        let near = fallback.filter(|(_, _, _, pts)| pts.abs_diff(target_ms) <= FALLBACK_SLOP_MS);
        near.map(|(w, h, rgba, pts)| {
            self.last_pts = Some(pts);
            scaled_thumb(&rgba, w, h)
        })
    }
}

/// Turn decoded source-resolution pixels into a cached thumbnail.
#[cfg_attr(not(feature = "decode"), allow(dead_code))]
fn scaled_thumb(rgba: &[u8], w: u32, h: u32) -> Thumb {
    let (tw, th, px) = downscale(rgba, w, h, THUMB_W, THUMB_H);
    Thumb::new(tw, th, Arc::new(px), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn projected(path: &str) -> MediaSource {
        MediaSource {
            abs_path: path.into(),
            hydration: crate::media::Hydration::Projected,
        }
    }

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

    #[test]
    fn a_thumbnails_render_handle_is_built_once_and_is_stable() {
        // The renderer clones this per cell per frame. Every clone must carry
        // the *same* blob id, or the GPU atlas re-uploads the pixels each time.
        let t = representative(1, 16, 9);
        let first = t.image().clone();
        let second = t.image().clone();
        assert_eq!(
            first.data.id(),
            second.data.id(),
            "cloning the cached handle keeps one identity"
        );
        assert_eq!(first.data.id(), t.image().data.id());
        // …and a *fresh* blob over the same bytes would not, which is exactly
        // the miss the old draw path caused.
        let fresh =
            Blob::new(Arc::new(t.rgba.as_ref().clone()) as Arc<dyn AsRef<[u8]> + Send + Sync>);
        assert_ne!(fresh.id(), first.data.id());
        // The handle points at the thumbnail's own pixels, not a copy.
        assert_eq!(first.data.as_ref(), t.rgba.as_slice());
    }

    #[test]
    fn a_failed_decode_is_terminal_and_stops_the_wake_loop() {
        // The reported bug: `ensure` asks for the bin swatch, the decode
        // yields nothing, and the old code both dropped the result and left
        // the id in the in-flight set — so `has_pending` stayed true forever
        // and the app never went idle. Drive exactly that: mark the requests
        // as `request` does, then deliver the worker's empty answer.
        let mut cache = ThumbnailCache::new();
        let id = uuid::Uuid::new_v4();
        let src = projected("/not/decodable.mp4");
        cache.ensure(id, 0, &src, 0.5);
        cache.swatch_pending.insert(id);
        cache
            .frames
            .entry(id)
            .or_default()
            .slots
            .insert(7, Slot::Pending);
        assert!(cache.has_pending(), "the requested frames are in flight");

        cache
            .tx
            .send(Outcome {
                id,
                bucket: SWATCH_BUCKET,
                thumb: None,
            })
            .unwrap();
        cache.drain();
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

        // Re-running the app's per-frame `ensure`/`ensure_strip` must not
        // resurrect the request — that is the loop that burned CPU.
        cache.ensure(id, 0, &src, 0.5);
        cache.ensure_strip(id, 0, &src, 0.0, 4.0, 12);
        assert!(
            !cache.has_pending(),
            "a written-off source is never re-requested"
        );

        // And the bin still shows the representative frame, permanently.
        let t = cache.get(id).expect("the representative frame survives");
        assert!(!t.real);
    }

    #[test]
    fn a_source_that_answers_nothing_anywhere_is_given_up_on() {
        // Repeated per-bucket failures also have to terminate, or a clip whose
        // every cell fails would keep the app awake one bucket at a time.
        let mut cache = ThumbnailCache::new();
        let id = uuid::Uuid::new_v4();
        cache.ensure(id, 0, &projected("/broken.mp4"), 0.5);
        for b in 0..FAILURES_BEFORE_GIVING_UP {
            cache
                .frames
                .entry(id)
                .or_default()
                .slots
                .insert(b, Slot::Pending);
            cache
                .tx
                .send(Outcome {
                    id,
                    bucket: b,
                    thumb: None,
                })
                .unwrap();
            cache.drain();
        }
        assert!(
            cache.is_undecodable(id),
            "a source that never answers is given up on"
        );
        assert!(!cache.has_pending());
    }

    #[test]
    fn a_blocking_warm_on_an_unhydrated_source_leaves_the_representative() {
        // The synchronous path must be as honest as the pool: no bytes means
        // the representative frame stays and nothing is left outstanding.
        let mut cache = ThumbnailCache::new();
        let id = uuid::Uuid::new_v4();
        cache.warm_blocking(id, 2, &projected("/nope.mp4"), 1.0, 0.0, 4.0, 6);
        let t = cache.get(id).expect("a representative frame");
        assert!(
            !t.real,
            "nothing was decoded, and it does not pretend otherwise"
        );
        assert!(
            !cache.has_pending(),
            "a blocking warm leaves nothing in flight"
        );
        assert!(
            !cache.is_undecodable(id),
            "un-hydrated is not the same as undecodable — the bytes may arrive"
        );
    }

    #[test]
    fn one_bad_seek_does_not_condemn_a_source() {
        let mut cache = ThumbnailCache::new();
        let id = uuid::Uuid::new_v4();
        cache.ensure(id, 0, &projected("/clip.mp4"), 0.5);
        cache
            .tx
            .send(Outcome {
                id,
                bucket: 3,
                thumb: None,
            })
            .unwrap();
        cache.drain();
        assert!(
            !cache.is_undecodable(id),
            "a single failed bucket is not a verdict"
        );
        // The bucket itself is terminal, though — never asked for again.
        assert!(matches!(cache.frames[&id].slots[&3], Slot::Failed));
        assert!(!cache.has_pending());
    }

    #[test]
    fn the_strip_shows_successive_source_times_not_one_repeated_frame() {
        // Feed real frames for three different source times, as the worker
        // would, and check the strip lookup returns a *different* picture per
        // cell across a clip's trimmed window.
        let mut cache = ThumbnailCache::new();
        let id = uuid::Uuid::new_v4();
        cache.ensure(id, 0, &projected("/clip.mp4"), 0.5);
        for (t, shade) in [(1.0f64, 10u8), (5.0, 120), (9.0, 240)] {
            let px = vec![shade; 4 * 4 * 4];
            cache
                .tx
                .send(Outcome {
                    id,
                    bucket: bucket_of(t),
                    thumb: Some(Thumb::new(4, 4, Arc::new(px), true)),
                })
                .unwrap();
        }
        cache.drain();

        let a = cache.frame_at(id, 1.0).expect("frame near 1s").clone();
        let b = cache.frame_at(id, 5.0).expect("frame near 5s").clone();
        let c = cache.frame_at(id, 9.0).expect("frame near 9s").clone();
        assert!(a.real && b.real && c.real, "all three are real decodes");
        assert_ne!(a.rgba[0], b.rgba[0], "1s and 5s are different pictures");
        assert_ne!(b.rgba[0], c.rgba[0], "5s and 9s are different pictures");
        // Distinct handles too, so the atlas keys them separately.
        assert_ne!(a.image().data.id(), b.image().data.id());
    }

    #[test]
    fn strip_cells_sample_across_the_trimmed_window() {
        // A clip trimmed to [4, 8] must ask for buckets inside that window and
        // nowhere else — the strip shows the part of the source in use.
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

    #[test]
    fn a_time_with_no_frame_yet_falls_back_without_inventing_one() {
        let mut cache = ThumbnailCache::new();
        let id = uuid::Uuid::new_v4();
        cache.ensure(id, 3, &projected("/clip.mp4"), 0.5);
        // Nothing decoded: the strip shows the representative, marked as such.
        let t = cache.frame_at(id, 12.0).expect("something to draw");
        assert!(
            !t.real,
            "an undecoded cell must not claim to be a real frame"
        );

        // With one real frame present, a nearby time reuses it (still real,
        // still this source) rather than jumping back to the gradient.
        cache
            .tx
            .send(Outcome {
                id,
                bucket: bucket_of(12.0),
                thumb: Some(Thumb::new(2, 2, Arc::new(vec![9u8; 16]), true)),
            })
            .unwrap();
        cache.drain();
        let near = cache.frame_at(id, 12.1).expect("nearest real frame");
        assert!(near.real);
        assert_eq!(near.rgba[0], 9);

        // Far away in the clip, though, that frame says nothing — the strip
        // falls back to the representative rather than showing the wrong shot.
        let far = cache.frame_at(id, 90.0).expect("something to draw");
        assert!(
            !far.real,
            "a distant cell must not borrow an unrelated frame"
        );
    }

    #[test]
    fn a_request_that_cannot_be_queued_is_not_left_pending() {
        // Fill the pool queue, then ask for one more frame. The refused
        // request must leave no pending slot behind — a slot marked pending
        // for a job nobody holds never reaches a terminal state, and
        // `has_pending` would stay true for the rest of the session.
        let cache = ThumbnailCache::new();
        let id = uuid::Uuid::new_v4();
        {
            let mut q = cache.pool.queue.lock().unwrap();
            for _ in 0..MAX_QUEUED {
                q.jobs.push_back(Job {
                    id: uuid::Uuid::new_v4(),
                    path: "/other.mp4".into(),
                    bucket: 0,
                    at: 0.0,
                });
            }
        }
        let mut cache = cache;
        let local = MediaSource {
            abs_path: std::env::temp_dir(),
            hydration: crate::media::Hydration::Local,
        };
        cache.ensure_strip(id, 0, &local, 0.0, 10.0, 8);
        let stuck = cache
            .frames
            .get(&id)
            .map(|f| {
                f.slots
                    .values()
                    .filter(|s| matches!(s, Slot::Pending))
                    .count()
            })
            .unwrap_or(0);
        assert_eq!(stuck, 0, "a refused request leaves nothing pending");
        // Draining the backlog and asking again does queue the work. One cell
        // only: each request stands up a real pipeline, and this test is about
        // the bookkeeping, not the decode.
        cache.pool.queue.lock().unwrap().jobs.clear();
        cache.ensure_strip(id, 0, &local, 0.0, 10.0, 1);
        #[cfg(feature = "decode")]
        assert!(
            cache.has_pending(),
            "with room in the queue the work is taken"
        );
    }

    #[test]
    fn lru_stamps_do_not_grow_without_bound() {
        // Scrubbing visits far more buckets than are ever decoded; the
        // bookkeeping for them must be pruned too, not just the pixels.
        let mut cache = ThumbnailCache::new();
        let id = uuid::Uuid::new_v4();
        for b in 0..(MAX_FRAMES_PER_SOURCE as u32 * 6) {
            cache.touch(id, b);
        }
        cache
            .tx
            .send(Outcome {
                id,
                bucket: 0,
                thumb: Some(Thumb::new(1, 1, Arc::new(vec![0u8; 4]), true)),
            })
            .unwrap();
        cache.drain();
        assert!(
            cache.frames[&id].touched.len() <= MAX_FRAMES_PER_SOURCE * 4,
            "stamps for buckets holding nothing are forgotten"
        );
    }

    #[test]
    fn cached_frames_are_capped_per_source() {
        let mut cache = ThumbnailCache::new();
        let id = uuid::Uuid::new_v4();
        for b in 0..(MAX_FRAMES_PER_SOURCE as u32 + 20) {
            cache.touch(id, b);
            cache
                .tx
                .send(Outcome {
                    id,
                    bucket: b,
                    thumb: Some(Thumb::new(1, 1, Arc::new(vec![0u8; 4]), true)),
                })
                .unwrap();
            cache.drain();
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
        // The most recent buckets are the ones kept.
        assert!(
            cache
                .frame_at(id, bucket_time(MAX_FRAMES_PER_SOURCE as u32 + 19))
                .is_some()
        );
    }

    #[test]
    fn downscale_shrinks_to_the_thumbnail_box_and_keeps_aspect() {
        let (w, h) = (1920u32, 1080u32);
        let src = vec![200u8; (w * h * 4) as usize];
        let (dw, dh, out) = downscale(&src, w, h, THUMB_W, THUMB_H);
        assert!(dw <= THUMB_W && dh <= THUMB_H, "{dw}x{dh} fits the box");
        assert_eq!(out.len(), (dw * dh * 4) as usize);
        assert!(
            (dw as f64 / dh as f64 - 16.0 / 9.0).abs() < 0.05,
            "aspect kept"
        );
        // A flat source averages to the same flat value — no smearing.
        assert!(out.iter().all(|&v| v == 200));
        // Already-small frames pass through untouched.
        let small = vec![7u8; 4 * 4 * 4];
        let (sw2, sh2, out2) = downscale(&small, 4, 4, THUMB_W, THUMB_H);
        assert_eq!((sw2, sh2, out2.len()), (4, 4, small.len()));
    }
}
