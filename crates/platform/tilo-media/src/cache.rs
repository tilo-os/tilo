//! A bounded frame cache, so stepping and short seeks do not re-decode.
//!
//! Scrubbing a timeline revisits the same handful of times constantly: nudge
//! the playhead one frame forward, then back, then forward again. Without a
//! cache each of those is a flushing seek that tears the decoder down and
//! refills it. With one, a step back is a hash lookup.
//!
//! The cache is keyed by `(source, time bucket)`. Times are bucketed to the
//! source's frame interval so that a playhead at 12.503s and one at 12.510s —
//! which resolve to the same picture in a 30fps clip — hit the same entry
//! rather than thrashing.
//!
//! # Bounding
//!
//! Video frames are large: a 1080p RGBA picture is 8 MB, and a dmabuf holds a
//! GPU allocation *and* a file descriptor. So the cache bounds itself two ways
//! — a maximum entry count and a maximum byte budget — and evicts
//! least-recently-used entries until both hold. Dropping an entry closes its
//! dmabuf fds, which is what returns the buffer to the decoder's pool, so an
//! unbounded cache here would starve the decoder rather than merely waste
//! memory.

use std::collections::HashMap;
use std::sync::Arc;

use crate::frame::{Frame, FramePayload};

/// Identifies the media a cached frame came from. A plain string so the caller
/// can key on a path, a URI, or its own media id without the cache caring.
pub type SourceKey = Arc<str>;

/// A cache key: which source, and which frame bucket within it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FrameKey {
    pub source: SourceKey,
    /// The bucket index — `time_ms / bucket_ms`, so nearby times that resolve
    /// to the same picture collapse onto one entry.
    pub bucket: u64,
}

/// How wide a time bucket is, derived from the source frame rate.
///
/// Rounded **down** so a bucket never spans two frames: at 30fps a 33ms bucket
/// would let 12.999s and 13.020s (different pictures) collide.
///
/// A degenerate frame rate — zero, negative, NaN, infinite — yields a 1 ms
/// bucket rather than a division by zero downstream in [`bucket_of`].
pub fn bucket_ms_for_fps(fps: f64) -> u64 {
    /// The largest integer `f64` represents without gaps, and it fits `u64`.
    /// 2^53 ms is 285 000 years; a frame rate small enough to reach it is not
    /// a frame rate.
    const MAX_EXACT_MS: f64 = 9_007_199_254_740_992.0; // 2^53

    if fps <= 0.0 || !fps.is_finite() {
        return 1;
    }
    // `fps` is finite and positive here. Clamp before converting to `u64`.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 1.0..=2^53 on the same line, which u64 holds exactly"
    )]
    let bucket = (1000.0 / fps).floor().clamp(1.0, MAX_EXACT_MS) as u64;
    bucket
}

/// Which bucket a time falls in.
///
/// A zero `bucket_ms` is read as 1 ms, so every millisecond is its own bucket
/// rather than the division trapping.
pub fn bucket_of(time_ms: u64, bucket_ms: u64) -> u64 {
    // `.max(1)` rules out the only divisor `checked_div` rejects, so the
    // fallback below never runs; it is here because the alternative is a
    // bare `/` that clippy cannot see is guarded.
    time_ms.checked_div(bucket_ms.max(1)).unwrap_or(time_ms)
}

/// Roughly how much memory a frame holds, for the byte budget.
///
/// A dmabuf's bytes live on the GPU rather than in this process, but they count
/// against the decoder's buffer pool, so charge the full picture size.
pub fn frame_cost_bytes(frame: &Frame) -> usize {
    match &frame.payload {
        FramePayload::Rgba(b) => b.len(),
        FramePayload::Planar(p) => p.planes.iter().map(|pl| pl.bytes.len()).sum(),
        // NV12-equivalent: one luma byte plus half a chroma byte per pixel.
        //
        // Saturating, not wrapping: the dimensions come out of a decoder for a
        // stream this crate did not author, and a wrapped product would charge
        // a huge picture *almost nothing*, so the cache would stop evicting and
        // starve the decoder's buffer pool — the exact failure the byte budget
        // exists to prevent.
        #[cfg(unix)]
        FramePayload::Dmabuf(d) => {
            (d.width as usize)
                .saturating_mul(d.height as usize)
                .saturating_mul(3)
                / 2
        }
    }
}

/// A bounded least-recently-used cache of decoded frames.
pub struct FrameCache {
    entries: HashMap<FrameKey, Entry>,
    /// Monotonic tick, incremented on every access; the smallest value is the
    /// least recently used.
    clock: u64,
    bytes: usize,
    max_entries: usize,
    max_bytes: usize,
    hits: u64,
    misses: u64,
}

struct Entry {
    frame: Arc<Frame>,
    used_at: u64,
    cost: usize,
}

impl FrameCache {
    /// A cache bounded by both an entry count and a byte budget. Whichever
    /// binds first drives eviction.
    pub fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            clock: 0,
            bytes: 0,
            max_entries: max_entries.max(1),
            max_bytes: max_bytes.max(1),
            hits: 0,
            misses: 0,
        }
    }

    /// A cache sized for interactive scrubbing of one 1080p source: enough
    /// frames to cover a second of stepping in either direction, capped at
    /// 256 MB so a 4K source shrinks the count instead of exhausting memory.
    pub fn for_preview() -> Self {
        Self::new(64, 256 * 1024 * 1024)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Bytes currently held.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Cache hits and misses since construction, for a diagnostic overlay.
    pub fn stats(&self) -> (u64, u64) {
        (self.hits, self.misses)
    }

    /// Look a frame up, marking it most-recently-used.
    pub fn get(&mut self, key: &FrameKey) -> Option<Arc<Frame>> {
        // Saturating rather than wrapping: at `u64::MAX` recency ordering goes
        // flat and eviction degrades to arbitrary, where a wrap would make the
        // newest entry look like the oldest and evict exactly what is in use.
        // Neither is reachable — one tick per cache access is 584 years at a
        // billion accesses a second — but the failure modes are not equal.
        self.clock = self.clock.saturating_add(1);
        let tick = self.clock;
        if let Some(e) = self.entries.get_mut(key) {
            e.used_at = tick;
            self.hits = self.hits.saturating_add(1);
            Some(e.frame.clone())
        } else {
            self.misses = self.misses.saturating_add(1);
            None
        }
    }

    /// Whether a key is present, without disturbing its recency. For a
    /// read-ahead planner deciding what still needs decoding.
    pub fn contains(&self, key: &FrameKey) -> bool {
        self.entries.contains_key(key)
    }

    /// Insert a frame, evicting least-recently-used entries until both bounds
    /// hold. Replacing an existing key keeps the newer frame.
    pub fn insert(&mut self, key: FrameKey, frame: Arc<Frame>) {
        self.clock = self.clock.saturating_add(1);
        let cost = frame_cost_bytes(&frame);
        if let Some(old) = self.entries.remove(&key) {
            self.bytes = self.bytes.saturating_sub(old.cost);
        }
        self.entries.insert(
            key,
            Entry {
                frame,
                used_at: self.clock,
                cost,
            },
        );
        // Saturating: a byte total that wrapped would read as *below* the
        // budget and stop eviction entirely, which for dmabuf entries means
        // holding decoder pool buffers and file descriptors forever. Saturated,
        // `evict_to_bounds` simply keeps evicting.
        self.bytes = self.bytes.saturating_add(cost);
        self.evict_to_bounds();
    }

    /// Drop everything belonging to one source — it closed, or its file
    /// changed underneath us.
    pub fn invalidate_source(&mut self, source: &str) {
        let doomed: Vec<FrameKey> = self
            .entries
            .keys()
            .filter(|k| &*k.source == source)
            .cloned()
            .collect();
        for k in doomed {
            if let Some(e) = self.entries.remove(&k) {
                self.bytes = self.bytes.saturating_sub(e.cost);
            }
        }
    }

    /// Drop everything.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }

    fn evict_to_bounds(&mut self) {
        while self.entries.len() > self.max_entries || self.bytes > self.max_bytes {
            let Some(victim) = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.used_at)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            if let Some(e) = self.entries.remove(&victim) {
                self.bytes = self.bytes.saturating_sub(e.cost);
            }
        }
    }
}

impl Default for FrameCache {
    fn default() -> Self {
        Self::for_preview()
    }
}

/// The times a read-ahead should decode next, given where the playhead is and
/// which way it is travelling.
///
/// Returns bucket times **not already cached**, nearest first, so a caller can
/// decode as many as its budget allows and stop. Playing forward reads ahead;
/// scrubbing backwards reads behind.
pub fn read_ahead_targets(
    cache: &FrameCache,
    source: &SourceKey,
    position_ms: u64,
    bucket_ms: u64,
    direction: i64,
    count: usize,
) -> Vec<u64> {
    let here = bucket_of(position_ms, bucket_ms);
    let backwards = direction < 0;
    let mut out = Vec::with_capacity(count);
    // Scan twice as far as asked, so cached buckets can be skipped without
    // cutting the read-ahead short.
    let scan = u64::try_from(count).unwrap_or(u64::MAX).saturating_mul(2);
    for offset in 1..=scan {
        if out.len() >= count {
            break;
        }
        // Stays in the unsigned bucket domain throughout. The previous form
        // went through `i64`, where a bucket past `i64::MAX` read as negative
        // and silently returned nothing to read ahead.
        let Some(bucket) = (if backwards {
            here.checked_sub(offset)
        } else {
            here.checked_add(offset)
        }) else {
            // Ran off the start of the stream, or off the end of the bucket
            // space. Either way there is nothing further in this direction.
            break;
        };
        let key = FrameKey {
            source: source.clone(),
            bucket,
        };
        if !cache.contains(&key) {
            let Some(time_ms) = bucket.checked_mul(bucket_ms) else {
                break;
            };
            out.push(time_ms);
        }
    }
    out
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    reason = "fixture arithmetic on literal frame sizes, bucket counts and \
              budgets — an overflow here would be a broken test, not a \
              hazard reaching a user's footage"
)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::dmabuf::{Colorimetry, DRM_FORMAT_NV12, DmabufImage};

    fn rgba_frame(w: u32, h: u32, pts_ms: u64) -> Arc<Frame> {
        Arc::new(Frame {
            width: w,
            height: h,
            pts_ms,
            payload: FramePayload::Rgba(Arc::new(vec![0u8; (w * h * 4) as usize])),
        })
    }

    fn key(source: &str, bucket: u64) -> FrameKey {
        FrameKey {
            source: Arc::from(source),
            bucket,
        }
    }

    #[test]
    fn bucket_space_and_read_ahead_stay_in_bounds() {
        // Phase `buckets_never_span_two_frames_at_common_rates`.
        {
            // 33ms at 30fps would let 12.999s and 13.020s — different pictures —
            // land in one bucket. The real invariant: consecutive frame times
            // differ by at least one bucket. Each frame interval is written out
            // in whole milliseconds rather than recomputed from the frame rate,
            // so the expected value is stated rather than derived from the code
            // under test.
            for (fps, frame_interval_ms) in [
                (24.0, 42u64),
                (25.0, 40),
                (30.0, 33),
                (50.0, 20),
                (60.0, 17),
            ] {
                let b = bucket_ms_for_fps(fps);
                assert!(b >= 1);
                assert!(
                    b <= frame_interval_ms,
                    "bucket {b}ms must not exceed the {frame_interval_ms}ms frame interval at {fps}fps"
                );
                // Two consecutive frames must fall in different buckets.
                let t0 = 12_000u64;
                let t1 = t0 + frame_interval_ms;
                assert_ne!(
                    bucket_of(t0, b),
                    bucket_of(t1, b),
                    "consecutive frames collide at {fps}fps with a {b}ms bucket"
                );
            }
        }
        // Phase `a_degenerate_frame_rate_does_not_produce_a_zero_bucket`.
        {
            // A zero bucket would divide by zero in `bucket_of`.
            assert_eq!(bucket_ms_for_fps(0.0), 1);
            assert_eq!(bucket_ms_for_fps(f64::NAN), 1);
            assert_eq!(bucket_ms_for_fps(-30.0), 1);
            assert_eq!(
                bucket_of(500, 0),
                500,
                "a zero bucket is clamped, not divided by"
            );
        }
        // Phase `read_ahead_skips_what_is_already_cached_and_follows_the_direction`.
        {
            let mut c = FrameCache::new(64, 1 << 30);
            let src: SourceKey = Arc::from("a");
            let bucket_ms = bucket_ms_for_fps(30.0);
            // Playhead at bucket 100; 101 already decoded.
            c.insert(
                FrameKey {
                    source: src.clone(),
                    bucket: 101,
                },
                rgba_frame(8, 8, 0),
            );

            let fwd = read_ahead_targets(&c, &src, 100 * bucket_ms, bucket_ms, 1, 3);
            assert_eq!(fwd.len(), 3);
            assert!(
                !fwd.contains(&(101 * bucket_ms)),
                "an already-cached time is not re-decoded: {fwd:?}"
            );
            assert_eq!(fwd[0], 102 * bucket_ms, "nearest uncached first");

            let back = read_ahead_targets(&c, &src, 100 * bucket_ms, bucket_ms, -1, 2);
            assert_eq!(
                back,
                vec![99 * bucket_ms, 98 * bucket_ms],
                "scrubbing back reads behind"
            );
        }
        // Phase `read_ahead_does_not_run_off_the_start_of_the_stream`.
        {
            let c = FrameCache::new(8, 1 << 30);
            let src: SourceKey = Arc::from("a");
            let back = read_ahead_targets(&c, &src, 0, 33, -1, 4);
            assert!(back.is_empty(), "nothing precedes t=0, got {back:?}");
        }
        // Phase `read_ahead_does_not_run_off_the_end_of_the_bucket_space`.
        {
            // A playhead at the top of the `u64` millisecond range — a "seek to
            // the end" sentinel, or a duration that was never resolved. The
            // read-ahead planner used to route the bucket index through `i64`
            // and multiply it back out into milliseconds; both steps break here.
            let c = FrameCache::new(8, 1 << 30);
            let src: SourceKey = Arc::from("a");

            // 1 ms buckets put the playhead in bucket `u64::MAX`, which as an
            // `i64` reads as -1 — so *forward* read-ahead came back with the
            // start of the stream, and the decoder would fetch t=0 while the
            // playhead sat at the end.
            let fwd = read_ahead_targets(&c, &src, u64::MAX, 1, 1, 4);
            assert!(
                fwd.iter().all(|t| *t >= u64::MAX.saturating_sub(4)),
                "forward read-ahead must stay ahead of the playhead, got {fwd:?}"
            );

            // At a realistic 33 ms bucket the same position overflows the
            // bucket→milliseconds multiplication outright.
            let fwd = read_ahead_targets(&c, &src, u64::MAX, 33, 1, 4);
            assert!(
                fwd.iter().all(|t| *t >= u64::MAX.saturating_sub(66)),
                "forward read-ahead must stay ahead of the playhead, got {fwd:?}"
            );
        }
    }

    #[test]
    fn cache_hits_evicts_and_accounts_bytes_by_recency() {
        // Phase `a_step_back_after_a_step_forward_is_a_hit_not_a_re_decode`.
        {
            // The exact scrubbing pattern the cache exists for.
            let mut c = FrameCache::new(8, 1 << 30);
            c.insert(key("a", 10), rgba_frame(16, 16, 333));
            c.insert(key("a", 11), rgba_frame(16, 16, 366));
            assert!(
                c.get(&key("a", 10)).is_some(),
                "stepping back hits the cache"
            );
            assert!(c.get(&key("a", 11)).is_some());
            let (hits, misses) = c.stats();
            assert_eq!((hits, misses), (2, 0));
            assert!(
                c.get(&key("a", 12)).is_none(),
                "an unvisited time is a miss"
            );
            assert_eq!(c.stats(), (2, 1));
        }
        // Phase `the_entry_bound_evicts_the_least_recently_used_not_the_oldest_inserted`.
        {
            let mut c = FrameCache::new(2, 1 << 30);
            c.insert(key("a", 1), rgba_frame(8, 8, 0));
            c.insert(key("a", 2), rgba_frame(8, 8, 1));
            // Touch 1 so 2 becomes the least recently used.
            assert!(c.get(&key("a", 1)).is_some());
            c.insert(key("a", 3), rgba_frame(8, 8, 2));
            assert_eq!(c.len(), 2);
            assert!(c.contains(&key("a", 1)), "recently used survives");
            assert!(c.contains(&key("a", 3)));
            assert!(!c.contains(&key("a", 2)), "least recently used was evicted");
        }
        // Phase `the_byte_budget_binds_before_the_entry_count_on_large_frames`.
        {
            // Three 1080p RGBA frames are ~24 MB; a 16 MB budget must hold two or
            // fewer even though the entry cap allows 64.
            let budget = 16 * 1024 * 1024;
            let mut c = FrameCache::new(64, budget);
            for i in 0..3u64 {
                c.insert(key("a", i), rgba_frame(1920, 1080, i));
            }
            assert!(
                c.bytes() <= budget,
                "byte budget respected: {} > {budget}",
                c.bytes()
            );
            assert!(c.len() < 3, "the byte budget evicted, not the entry cap");
            assert!(!c.is_empty(), "at least the newest frame is retained");
        }
        // Phase `replacing_a_key_does_not_double_count_its_bytes`.
        {
            let mut c = FrameCache::new(8, 1 << 30);
            c.insert(key("a", 1), rgba_frame(64, 64, 0));
            let once = c.bytes();
            c.insert(key("a", 1), rgba_frame(64, 64, 1));
            assert_eq!(c.bytes(), once, "the old entry's bytes were released");
            assert_eq!(c.len(), 1);
        }
        // Phase `invalidating_one_source_leaves_the_others_intact`.
        {
            let mut c = FrameCache::new(16, 1 << 30);
            c.insert(key("a", 1), rgba_frame(8, 8, 0));
            c.insert(key("a", 2), rgba_frame(8, 8, 1));
            c.insert(key("b", 1), rgba_frame(8, 8, 2));
            c.invalidate_source("a");
            assert!(!c.contains(&key("a", 1)));
            assert!(!c.contains(&key("a", 2)));
            assert!(c.contains(&key("b", 1)), "another source is untouched");
            assert_eq!(c.len(), 1);
            assert!(
                c.bytes() > 0,
                "the surviving entry still accounts for its bytes"
            );
        }
    }

    #[test]
    #[cfg(unix)]
    fn a_dmabuf_frame_is_charged_for_its_gpu_bytes_not_treated_as_free() {
        // Holding a dmabuf holds a decoder pool buffer and an fd. Charging it
        // zero would let the cache starve the decoder.
        let f = Arc::new(Frame {
            width: 1920,
            height: 1080,
            pts_ms: 0,
            payload: FramePayload::Dmabuf(Arc::new(DmabufImage {
                planes: vec![],
                fourcc: DRM_FORMAT_NV12,
                modifier: 0,
                width: 1920,
                height: 1080,
                colorimetry: Colorimetry::default(),
            })),
        });
        let cost = frame_cost_bytes(&f);
        assert_eq!(cost, 1920 * 1080 * 3 / 2, "charged its NV12 picture size");
        assert!(cost > 0, "never free");

        let mut c = FrameCache::new(64, 4 * 1024 * 1024);
        for i in 0..4u64 {
            c.insert(key("a", i), f.clone());
        }
        assert!(
            c.bytes() <= 4 * 1024 * 1024,
            "dmabufs are evicted under the budget too"
        );
    }
}
