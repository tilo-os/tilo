//! One decode pipeline per source, shared by everything that needs it: a pipeline
//! costs element discovery, a decoder and, on the hardware path, a GPU context, so
//! the preview and thumbnail requests for a source share one [`EngineSession`] and
//! its [`FrameCache`].
//!
//! Sessions are handed out as `Arc<Mutex<EngineSession>>`: a pipeline is not `Sync`
//! and the mutex makes sharing one legal, and it also serialises the seeks, which
//! two callers scrubbing the same decoder to different times would fight over. Hold
//! the lock for one operation, not for a frame of UI work.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tilo_clock::time::Instant;

use crate::cache::{FrameCache, FrameKey, SourceKey, bucket_ms_for_fps, bucket_of};
use crate::engine::{EngineConfig, MediaEngine};
use crate::frame::Frame;
use crate::hw::HwReport;
use crate::source::Source;

// Long enough for a hardware decoder to spin up and run a GOP, short enough that a
// wedged pipeline does not hang a thumbnail worker.
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
    pub fn open(source: Source, key: SourceKey, config: EngineConfig) -> Self {
        let mut engine = MediaEngine::with_config(config);
        engine.open(source);
        Self {
            engine,
            cache: FrameCache::for_preview(),
            key,
            // Until the frame rate is known, 1 ms buckets conflate no two times;
            // `set_fps` narrows it.
            bucket_ms: 1,
            last_used: tilo_clock::now(),
        }
    }

    /// Tell the session the source's frame rate so cache buckets match frames.
    pub fn set_fps(&mut self, fps: f64) {
        let b = bucket_ms_for_fps(fps);
        if b != self.bucket_ms {
            // Buckets computed under the old width mean something else, so the entries
            // under them are not reusable.
            self.cache.invalidate_source(&self.key);
            self.bucket_ms = b;
        }
    }

    pub fn bucket_ms(&self) -> u64 {
        self.bucket_ms
    }

    pub fn engine(&mut self) -> &mut MediaEngine {
        self.last_used = tilo_clock::now();
        &mut self.engine
    }

    pub fn hw_report(&self) -> HwReport {
        self.engine.hw_report()
    }

    pub fn cache_stats(&self) -> (u64, u64) {
        self.cache.stats()
    }

    fn key_for(&self, time_ms: u64) -> FrameKey {
        FrameKey {
            source: self.key.clone(),
            bucket: bucket_of(time_ms, self.bucket_ms),
        }
    }

    /// Never seeks, never blocks: the fast path for a repaint that has not moved the
    /// playhead.
    pub fn cached_frame_at(&mut self, time_ms: u64) -> Option<Arc<Frame>> {
        self.last_used = tilo_clock::now();
        let k = self.key_for(time_ms);
        self.cache.get(&k)
    }

    /// The frame at `time_ms`, decoding it if it is not cached, for up to `timeout`;
    /// `None` when no picture could be produced in time, so the caller shows whatever
    /// it had rather than stalling.
    ///
    /// Never answers with a picture from before the seek: a pipeline running when the
    /// seek arrived can hand over a frame it decoded for the old position while the
    /// seek's bookkeeping counts it as new, which an export would show as a repeated
    /// frame. The picture must land on `time_ms` by its own timestamp
    /// ([`Self::lands_on`]), or be the one a second seek lands on too; anything else is
    /// sought again until `timeout`, and only the picture that landed is cached.
    pub fn frame_at(&mut self, time_ms: u64, timeout: Duration) -> Option<Arc<Frame>> {
        if let Some(f) = self.cached_frame_at(time_ms) {
            return Some(f);
        }
        let deadline = tilo_clock::now().checked_add(timeout);
        let left = || deadline.map_or(timeout, |d| d.saturating_duration_since(tilo_clock::now()));
        self.engine.seek_and_wait(time_ms, timeout).ok()?;
        let mut offered = self.engine.latest()?.pts_ms;
        while !self.lands_on(offered, time_ms) {
            if deadline.is_some_and(|d| tilo_clock::now() >= d) {
                tracing::debug!(
                    "media: seek to {time_ms}ms kept answering the picture at {offered}ms"
                );
                return None;
            }
            self.engine.seek_and_wait(time_ms, left()).ok()?;
            let again = self.engine.latest()?.pts_ms;
            if again == offered {
                break;
            }
            offered = again;
        }
        let frame = Arc::new(self.engine.latest()?.clone());
        // Key the entry by the picture's own timestamp as well as the requested time: an
        // accurate seek lands on the frame covering `time_ms`, whose PTS may sit
        // slightly earlier, and a later request at that PTS must hit rather than seek.
        self.cache.insert(self.key_for(time_ms), frame.clone());
        let pts_key = self.key_for(frame.pts_ms);
        if !self.cache.contains(&pts_key) {
            self.cache.insert(pts_key, frame.clone());
        }
        Some(frame)
    }

    /// Whether the picture at `pts_ms` is the one showing at `time_ms`: it starts
    /// there, give or take the millisecond both lost to truncation, or `time_ms` falls
    /// inside it. A frame lasts at least the bucket width, so a picture that ends by
    /// then is the one before.
    fn lands_on(&self, pts_ms: u64, time_ms: u64) -> bool {
        pts_ms.abs_diff(time_ms) <= 1
            || (self.bucket_ms > 1
                && pts_ms < time_ms
                && time_ms < pts_ms.saturating_add(self.bucket_ms))
    }

    /// The non-blocking counterpart to [`Self::frame_at`]: issue a seek with
    /// [`MediaEngine::seek_ms`], then call this each frame until it returns `true`.
    pub fn poll_into_cache(&mut self) -> bool {
        self.last_used = tilo_clock::now();
        self.engine.poll();
        if !self.engine.frame_is_current() {
            // Still showing the previous position's picture; nothing new to file yet.
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

    pub fn idle_for(&self) -> Duration {
        self.last_used.elapsed()
    }
}

/// Sessions created on demand, evicted when idle longer than the pool's limit or
/// when the pool is over its size cap: an open pipeline holds a decoder and, on the
/// hardware path, a slice of the GPU's fixed-function video engine.
pub struct SessionPool {
    sessions: HashMap<SourceKey, Arc<Mutex<EngineSession>>>,
    config: EngineConfig,
    max_sessions: usize,
    max_idle: Duration,
}

impl SessionPool {
    pub fn new(config: EngineConfig, max_sessions: usize, max_idle: Duration) -> Self {
        Self {
            sessions: HashMap::new(),
            config,
            max_sessions: max_sessions.max(1),
            max_idle,
        }
    }

    /// The pool an editor wants: GPU-native frames, silent (preview and thumbnails must
    /// not make a sound), a handful of concurrent pipelines, and a session dropped after
    /// half a minute untouched.
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

    /// The same source always yields the same session, so a thumbnail request reuses
    /// the preview's pipeline instead of standing up its own.
    pub fn session(&mut self, source: &Source) -> Arc<Mutex<EngineSession>> {
        let key: SourceKey = Arc::from(session_key(source).as_str());
        if let Some(s) = self.sessions.get(&key) {
            return s.clone();
        }
        self.evict_to_capacity();
        #[cfg_attr(
            web_page,
            expect(
                clippy::arc_with_non_send_sync,
                reason = "on a page a session holds the page's media element, and the page has one thread"
            )
        )]
        let session = Arc::new(Mutex::new(EngineSession::open(
            source.clone(),
            key.clone(),
            self.config,
        )));
        self.sessions.insert(key, session.clone());
        session
    }

    pub fn close(&mut self, source: &Source) {
        self.sessions.remove(session_key(source).as_str());
    }

    /// Call this periodically — once a second is plenty — so a closed clip's decoder
    /// does not linger.
    pub fn prune(&mut self) {
        let max_idle = self.max_idle;
        self.sessions.retain(|_, s| match s.lock() {
            Ok(g) => g.idle_for() < max_idle,
            Err(_) => false,
        });
    }

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

/// The URI a session is keyed by, so the same file reached as a local path and
/// through the workspace mount shares one pipeline. Unresolvable sources keep
/// distinct diagnostic keys, which are never sent to a decoder.
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

    #[test]
    fn a_seek_never_answers_with_a_picture_from_before_it() {
        let p = scratch("stale");
        let key: SourceKey = Arc::from(session_key(&Source::LocalFile(p.clone())).as_str());
        let mut s = EngineSession {
            engine: MediaEngine::new_synthesized(),
            cache: FrameCache::for_preview(),
            key,
            bucket_ms: 1,
            last_used: tilo_clock::now(),
        };
        s.engine.open(Source::LocalFile(p.clone()));
        s.set_fps(30.0);
        s.engine.play();
        s.poll_into_cache();
        assert!(
            s.cached_frame_at(567).is_none(),
            "the roll is still short of it"
        );
        s.engine.offer_stale_on_next_seek(533);
        let frame = s
            .frame_at(567, tilo_test_env::hang_bound())
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
        fn changing_the_frame_rate_retires_buckets_measured_at_the_old_width() {
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
        {
            // A clip imported by absolute path and the same clip named through the workspace
            // mount must not stand up two decoders for one file.
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
        {
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

            // A third source must evict rather than grow the pool: every live pipeline
            // holds a decoder.
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
        {
            let p = scratch("prune");
            let mut pool =
                SessionPool::new(EngineConfig::default().frames_only(), 4, Duration::ZERO);
            let _ = pool.session(&Source::LocalFile(p.clone()));
            assert_eq!(pool.len(), 1);
            pool.prune();
            assert!(
                pool.is_empty(),
                "an idle session is closed rather than held open"
            );

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
        changing_the_frame_rate_retires_buckets_measured_at_the_old_width();
    }
}
