//! One decode pipeline per source, shared by everything that needs it.
//!
//! Building a `MediaEngine` sets up a `GStreamer` pipeline with element
//! discovery, a decoder, a demuxer, an audio chain, and, on the hardware path,
//! a VA display or CUDA context. Creating one per thumbnail would duplicate
//! setup work and prevent reuse of the decoder held by the preview.
//!
//! A [`SessionPool`] keeps one [`EngineSession`] per source. The preview and
//! thumbnail requests for that source share it, and each session carries its
//! own [`FrameCache`].
//!
//! # Threading
//!
//! Sessions are handed out as `Arc<Mutex<EngineSession>>`. A `GStreamer`
//! pipeline is not `Sync`, and the mutex is what makes sharing one legal; it
//! also serialises the seeks, which is what you want — two callers scrubbing
//! the same decoder to different times would otherwise fight.
//!
//! Hold the lock for one operation, not for a frame of UI work.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tilo_clock::Instant;

use crate::cache::{FrameCache, FrameKey, SourceKey, bucket_ms_for_fps, bucket_of};
use crate::engine::{EngineConfig, MediaEngine};
use crate::frame::Frame;
use crate::hw::HwReport;
use crate::source::Source;

/// How long to wait for a frame when a caller asks for one synchronously.
/// Long enough for a hardware decoder to spin up and run a GOP, short enough
/// that a wedged pipeline does not hang a thumbnail worker.
pub const DEFAULT_FRAME_TIMEOUT: Duration = Duration::from_millis(2_000);

/// A decode pipeline for one source, plus the frames already pulled from it.
pub struct EngineSession {
    engine: MediaEngine,
    cache: FrameCache,
    key: SourceKey,
    bucket_ms: u64,
    last_used: Instant,
}

impl EngineSession {
    /// Open `source` in a new session.
    pub fn open(source: Source, key: SourceKey, config: EngineConfig) -> Self {
        let mut engine = MediaEngine::with_config(config);
        engine.open(source);
        Self {
            engine,
            cache: FrameCache::for_preview(),
            key,
            // Until the source's frame rate is known, bucket at 1ms so no
            // two distinct times are ever conflated. `set_fps` narrows it.
            bucket_ms: 1,
            last_used: Instant::now(),
        }
    }

    /// Tell the session the source's frame rate so cache buckets match frames.
    pub fn set_fps(&mut self, fps: f64) {
        let b = bucket_ms_for_fps(fps);
        if b != self.bucket_ms {
            // Buckets computed under the old width no longer mean the same
            // thing, so the entries under them are not reusable.
            self.cache.invalidate_source(&self.key);
            self.bucket_ms = b;
        }
    }

    pub fn bucket_ms(&self) -> u64 {
        self.bucket_ms
    }

    /// Borrow the engine — for transport control (play/pause/volume) and for
    /// anything the session does not wrap.
    pub fn engine(&mut self) -> &mut MediaEngine {
        self.last_used = Instant::now();
        &mut self.engine
    }

    /// What the pipeline actually plugged.
    pub fn hw_report(&self) -> HwReport {
        self.engine.hw_report()
    }

    /// Cache hits and misses, for a diagnostic overlay.
    pub fn cache_stats(&self) -> (u64, u64) {
        self.cache.stats()
    }

    fn key_for(&self, time_ms: u64) -> FrameKey {
        FrameKey {
            source: self.key.clone(),
            bucket: bucket_of(time_ms, self.bucket_ms),
        }
    }

    /// The frame at `time_ms` if it is already cached. Never seeks, never
    /// blocks — the fast path for a repaint that has not moved the playhead.
    pub fn cached_frame_at(&mut self, time_ms: u64) -> Option<Arc<Frame>> {
        self.last_used = Instant::now();
        let k = self.key_for(time_ms);
        self.cache.get(&k)
    }

    /// The frame at `time_ms`, decoding it if it is not cached.
    ///
    /// Blocks for up to `timeout`. Returns `None` when the pipeline could not
    /// produce a picture in time — the caller shows whatever it had rather
    /// than stalling the UI.
    ///
    /// Never answers with a picture from before the seek. A pipeline that
    /// was running when the seek arrived can still hand over a frame it
    /// decoded for the old position, and the seek's own bookkeeping counts it
    /// as new; served, it would be a repeated frame in an export. So the
    /// picture has to land on `time_ms` by its own timestamp, measured from
    /// the stream's start ([`Self::lands_on`]), or else be the picture a
    /// second seek lands on too (a frame rate the bucket does not describe
    /// closely). Anything else is sought again until `timeout`, and only the
    /// picture that landed is cached.
    pub fn frame_at(&mut self, time_ms: u64, timeout: Duration) -> Option<Arc<Frame>> {
        if let Some(f) = self.cached_frame_at(time_ms) {
            return Some(f);
        }
        let deadline = Instant::now().checked_add(timeout);
        let left = || deadline.map_or(timeout, |d| d.saturating_duration_since(Instant::now()));
        self.engine.seek_and_wait(time_ms, timeout).ok()?;
        let mut offered = self.engine.latest()?.pts_ms;
        while !self.lands_on(offered, time_ms) {
            if deadline.is_some_and(|d| Instant::now() >= d) {
                tracing::debug!(
                    "media: seek to {time_ms}ms kept answering the picture at {offered}ms"
                );
                return None;
            }
            self.engine.seek_and_wait(time_ms, left()).ok()?;
            let again = self.engine.latest()?.pts_ms;
            if again == offered {
                // Two seeks agree: `time_ms` falls inside this picture.
                break;
            }
            offered = again;
        }
        let frame = Arc::new(self.engine.latest()?.clone());
        // Key the entry by the picture's own timestamp as well as the
        // requested time: an accurate seek lands on the frame covering
        // `time_ms`, whose PTS may sit slightly earlier, and a later request
        // at that PTS must hit rather than re-seek.
        self.cache.insert(self.key_for(time_ms), frame.clone());
        let pts_key = self.key_for(frame.pts_ms);
        if !self.cache.contains(&pts_key) {
            self.cache.insert(pts_key, frame.clone());
        }
        Some(frame)
    }

    /// Whether a picture at `pts_ms` is the one showing at `time_ms`: it
    /// starts there, give or take the millisecond both lost to truncation,
    /// or `time_ms` falls inside it. A frame lasts at least the bucket width
    /// (the frame interval rounded down), so a picture that ends by then is
    /// the one before; with no frame rate known only the first case counts.
    fn lands_on(&self, pts_ms: u64, time_ms: u64) -> bool {
        pts_ms.abs_diff(time_ms) <= 1
            || (self.bucket_ms > 1
                && pts_ms < time_ms
                && time_ms < pts_ms.saturating_add(self.bucket_ms))
    }

    /// Pump the pipeline and file whatever frame arrived into the cache.
    ///
    /// The non-blocking counterpart to [`Self::frame_at`], for a preview that
    /// polls once per rendered frame: issue a seek with
    /// [`MediaEngine::seek_ms`], then call this each frame until it returns
    /// `true`.
    pub fn poll_into_cache(&mut self) -> bool {
        self.last_used = Instant::now();
        self.engine.poll();
        if !self.engine.frame_is_current() {
            // Still showing the previous position's picture; nothing new to
            // file yet.
            return false;
        }
        let Some(frame) = self.engine.latest() else {
            return false;
        };
        let key = self.key_for(frame.pts_ms);
        if self.cache.contains(&key) {
            return false;
        }
        let frame = Arc::new(frame.clone());
        self.cache.insert(key, frame);
        true
    }

    /// How long since anything touched this session, for pool eviction.
    pub fn idle_for(&self) -> Duration {
        self.last_used.elapsed()
    }
}

/// A set of live decode sessions, one per source.
///
/// Sessions are created on demand and evicted when they have been idle longer
/// than the pool's limit or when the pool is over its size cap — an open
/// pipeline holds a decoder and, on the hardware path, a slice of the GPU's
/// fixed-function video engine, so they must not accumulate.
pub struct SessionPool {
    sessions: HashMap<SourceKey, Arc<Mutex<EngineSession>>>,
    config: EngineConfig,
    max_sessions: usize,
    max_idle: Duration,
}

impl SessionPool {
    /// A pool whose sessions are built with `config`.
    ///
    /// `max_sessions` bounds how many pipelines exist at once; `max_idle` is
    /// how long an untouched session survives a [`Self::prune`].
    pub fn new(config: EngineConfig, max_sessions: usize, max_idle: Duration) -> Self {
        Self {
            sessions: HashMap::new(),
            config,
            max_sessions: max_sessions.max(1),
            max_idle,
        }
    }

    /// The pool an editor wants: GPU-native frames, silent (the preview and
    /// thumbnails must not make a sound), a handful of concurrent pipelines,
    /// and a session dropped after half a minute untouched.
    pub fn for_editor() -> Self {
        Self::new(
            EngineConfig::gpu().frames_only(),
            4,
            Duration::from_secs(30),
        )
    }

    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// The session for `source`, creating it if it does not exist.
    ///
    /// The same source always yields the same session, so a thumbnail request
    /// reuses the preview's pipeline instead of standing up its own.
    pub fn session(&mut self, source: &Source) -> Arc<Mutex<EngineSession>> {
        let key: SourceKey = Arc::from(session_key(source).as_str());
        if let Some(s) = self.sessions.get(&key) {
            return s.clone();
        }
        self.evict_to_capacity();
        let session = Arc::new(Mutex::new(EngineSession::open(
            source.clone(),
            key.clone(),
            self.config,
        )));
        self.sessions.insert(key, session.clone());
        session
    }

    /// Drop the session for `source`, if any. Closes its pipeline.
    pub fn close(&mut self, source: &Source) {
        self.sessions.remove(session_key(source).as_str());
    }

    /// Drop every session idle longer than the pool's limit. Call this
    /// periodically — once a second is plenty — so a closed clip's decoder
    /// does not linger.
    pub fn prune(&mut self) {
        let max_idle = self.max_idle;
        self.sessions.retain(|_, s| match s.lock() {
            Ok(g) => g.idle_for() < max_idle,
            // A poisoned session is unusable; dropping it is the recovery.
            Err(_) => false,
        });
    }

    /// Drop the longest-idle sessions until there is room for one more.
    fn evict_to_capacity(&mut self) {
        while self.sessions.len() >= self.max_sessions {
            let victim = self
                .sessions
                .iter()
                .max_by_key(|(_, s)| s.lock().map_or(Duration::MAX, |g| g.idle_for()))
                .map(|(k, _)| k.clone());
            match victim {
                Some(k) => {
                    self.sessions.remove(&k);
                }
                None => break,
            }
        }
    }
}

/// The identity a session is keyed by: the source's resolved URI, so the same
/// file reached as a local path and as a workspace path shares one pipeline.
/// Unresolvable sources keep distinct diagnostic keys; those keys are never
/// sent to a decoder, which reports the URI conversion error separately.
pub fn session_key(source: &Source) -> String {
    source
        .to_uri()
        .unwrap_or_else(|_| format!("unresolved:{source:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tilo-session-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("clip.mp4");
        std::fs::write(&p, b"not really a video, but a real path").unwrap();
        p
    }

    /// An export rolls the decoder forward and, when the roll does not
    /// reach the next frame in time, seeks for it instead. A pipeline still
    /// running then can hand over the picture it decoded for its old
    /// position, and the seek counts it as new: the seek must go on to the
    /// picture asked for, and never file the stale one under the request.
    #[test]
    fn a_seek_never_answers_with_a_picture_from_before_it() {
        let p = scratch("stale");
        let key: SourceKey = Arc::from(session_key(&Source::LocalFile(p.clone())).as_str());
        let mut s = EngineSession {
            engine: MediaEngine::new_synthesized(),
            cache: FrameCache::for_preview(),
            key,
            bucket_ms: 1,
            last_used: Instant::now(),
        };
        s.engine.open(Source::LocalFile(p.clone()));
        s.set_fps(30.0);
        // The roll: running, and polled before it got to the frame wanted.
        s.engine.play();
        s.poll_into_cache();
        assert!(
            s.cached_frame_at(567).is_none(),
            "the roll is still short of it"
        );
        // The frame before the one asked for, decoded before the seek.
        s.engine.offer_stale_on_next_seek(533);
        let frame = s
            .frame_at(567, Duration::from_secs(5))
            .expect("the picture asked for");
        assert_eq!(frame.pts_ms, 567, "not the picture from before the seek");
        assert_eq!(
            s.cached_frame_at(567).map(|f| f.pts_ms),
            Some(567),
            "the stale picture is not filed under the request"
        );
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn sessions_are_shared_per_source_capped_pruned_and_retired() {
        // Phase `changing_the_frame_rate_retires_buckets_measured_at_the_old_width`.
        fn changing_the_frame_rate_retires_buckets_measured_at_the_old_width() {
            // A bucket index means nothing without the width it was computed at;
            // keeping entries across a change would return the wrong picture.
            let p = scratch("fps");
            let key: SourceKey = Arc::from(session_key(&Source::LocalFile(p.clone())).as_str());
            let mut s = EngineSession::open(
                Source::LocalFile(p.clone()),
                key,
                EngineConfig::default().frames_only(),
            );
            assert_eq!(
                s.bucket_ms(),
                1,
                "unknown frame rate never conflates two times"
            );
            s.set_fps(30.0);
            assert_eq!(s.bucket_ms(), bucket_ms_for_fps(30.0));
            s.set_fps(30.0);
            assert_eq!(
                s.bucket_ms(),
                bucket_ms_for_fps(30.0),
                "an unchanged rate is a no-op"
            );
            s.set_fps(60.0);
            assert_eq!(s.bucket_ms(), bucket_ms_for_fps(60.0));
            std::fs::remove_dir_all(p.parent().unwrap()).ok();
        }
        // Phase `the_same_file_reached_two_ways_shares_one_session_key`.
        {
            // A clip imported by absolute path and the same clip named through the
            // workspace mount must not stand up two decoders for one file.
            let p = scratch("key");
            let a = Source::LocalFile(p.clone());
            let b = Source::Workspace(p.clone());
            assert_eq!(session_key(&a), session_key(&b), "one file, one pipeline");

            let other = Source::Remote {
                url: "https://cdn.test/v.mp4".into(),
            };
            assert_ne!(session_key(&a), session_key(&other));
            std::fs::remove_dir_all(p.parent().unwrap()).ok();
        }
        // Phase `asking_twice_for_a_source_returns_the_same_session_not_a_second_pipeline`.
        {
            // The bug this pool exists to fix: a whole engine per thumbnail.
            let p = scratch("reuse");
            let mut pool = SessionPool::new(
                EngineConfig::default().frames_only(),
                4,
                Duration::from_secs(30),
            );
            let src = Source::LocalFile(p.clone());
            let a = pool.session(&src);
            let b = pool.session(&src);
            assert!(Arc::ptr_eq(&a, &b), "one decode pipeline per source");
            assert_eq!(pool.len(), 1);
            std::fs::remove_dir_all(p.parent().unwrap()).ok();
        }
        // Phase `distinct_sources_get_distinct_sessions_up_to_the_cap`.
        {
            let base = scratch("cap");
            let dir = base.parent().unwrap().to_path_buf();
            let mut pool = SessionPool::new(
                EngineConfig::default().frames_only(),
                2,
                Duration::from_secs(30),
            );
            let mut handles = Vec::new();
            for i in 0..2 {
                let p = dir.join(format!("c{i}.mp4"));
                std::fs::write(&p, b"x").unwrap();
                handles.push(pool.session(&Source::LocalFile(p)));
            }
            assert_eq!(pool.len(), 2, "distinct sources are distinct sessions");
            assert!(
                !Arc::ptr_eq(&handles[0], &handles[1]),
                "two sources must not be handed the same decode pipeline"
            );

            // A third source must evict rather than grow the pool: every live
            // pipeline holds a decoder.
            let p = dir.join("c2.mp4");
            std::fs::write(&p, b"x").unwrap();
            let _third = pool.session(&Source::LocalFile(p));
            assert!(
                pool.len() <= 2,
                "the cap bounds live pipelines, got {}",
                pool.len()
            );
            std::fs::remove_dir_all(&dir).ok();
        }
        // Phase `closing_a_source_drops_its_session`.
        {
            let p = scratch("close");
            let mut pool = SessionPool::new(
                EngineConfig::default().frames_only(),
                4,
                Duration::from_secs(30),
            );
            let src = Source::LocalFile(p.clone());
            let _ = pool.session(&src);
            assert_eq!(pool.len(), 1);
            pool.close(&src);
            assert!(pool.is_empty());
            std::fs::remove_dir_all(p.parent().unwrap()).ok();
        }
        // Phase `prune_drops_only_sessions_past_the_idle_limit`.
        {
            let p = scratch("prune");
            // Zero idle tolerance: everything untouched is immediately stale.
            let mut pool =
                SessionPool::new(EngineConfig::default().frames_only(), 4, Duration::ZERO);
            let _ = pool.session(&Source::LocalFile(p.clone()));
            assert_eq!(pool.len(), 1);
            pool.prune();
            assert!(
                pool.is_empty(),
                "an idle session is closed rather than held open"
            );

            // With a generous limit the same session survives.
            let mut pool = SessionPool::new(
                EngineConfig::default().frames_only(),
                4,
                Duration::from_secs(600),
            );
            let _ = pool.session(&Source::LocalFile(p.clone()));
            pool.prune();
            assert_eq!(pool.len(), 1, "a fresh session is not pruned");
            std::fs::remove_dir_all(p.parent().unwrap()).ok();
        }
        // Phase `changing_the_frame_rate_retires_buckets_measured_at_the_old_width` (defined above).
        changing_the_frame_rate_retires_buckets_measured_at_the_old_width();
    }
}
