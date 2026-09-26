//! Frames from a video (feature `video`).
//!
//! Reuses the shared `media` `GStreamer` engine — the exact decode path
//! `apps/video-editor` plays through — rather than a second codec stack. A
//! [`FrameSession`] owns one pipeline and answers any number of "the frame at
//! time t" requests against it, which is what lets a worker keep a source open
//! across a whole filmstrip instead of standing a pipeline up per cell.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use media::MediaEngine;

use crate::generate::{RgbaThumb, ThumbSize, rgba_to_thumb};

/// How long to wait for a frame at the seeked position before giving up on it.
const FRAME_TIMEOUT: Duration = Duration::from_secs(6);
/// How long to let a freshly-opened pipeline preroll before seeking it.
const PREROLL_TIMEOUT: Duration = Duration::from_secs(5);
/// How far past the requested time a frame may sit and still count as the
/// answer to that seek — a little over one frame at any sane rate.
const SEEK_SLOP_MS: u64 = 100;
/// How far from the requested time a frame may sit and still be worth showing
/// when the pipeline could not land on it (a keyframe-only source).
const FALLBACK_SLOP_MS: u64 = 2_000;
/// Poll interval while a pipeline works. The engine exposes no blocking wait.
const POLL: Duration = Duration::from_millis(20);
/// Seek this far in for a file's representative frame — most clips open on
/// black or near-black.
const REPRESENTATIVE_MS: u64 = 1_000;

/// Extract a representative keyframe from `path` as an RGBA thumbnail that
/// fits `size`. `None` if decoding is unavailable or no frame arrives.
pub fn video_thumb(path: &Path, size: impl Into<ThumbSize>) -> Option<RgbaThumb> {
    let mut session = FrameSession::open(path)?;
    let at = match session.duration_ms() {
        Some(d) if d > REPRESENTATIVE_MS * 2 => REPRESENTATIVE_MS,
        _ => 0,
    };
    session.frame_at(at, size.into())
}

/// One open source: the engine plus the last frame timestamp it handed back,
/// so a fresh seek can tell a new picture from the one already on the sink.
pub struct FrameSession {
    path: PathBuf,
    engine: MediaEngine,
    last_pts: Option<u64>,
}

impl FrameSession {
    /// Open `path` (read through the workspace mount when it lives there, so
    /// the bytes hydrate on demand). `None` when no real decoder is available:
    /// synthesised frames are never passed off as a preview.
    pub fn open(path: &Path) -> Option<Self> {
        let mut engine = MediaEngine::new();
        if !engine.is_real_decode() {
            return None;
        }
        engine.open(media::Source::for_path(path));
        // Grabbing a frame drives the pipeline with `play`, and the shared
        // engine always builds a real audio sink — an unmuted thumbnail worker
        // would play the clip out loud and hold an output device while it
        // seeks. A thumbnail is a picture; it must never make a sound.
        engine.set_muted(true);
        engine.set_volume(0.0);
        let mut session = Self {
            path: path.to_path_buf(),
            engine,
            last_pts: None,
        };
        session.await_preroll();
        Some(session)
    }

    /// The source this session decodes.
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn duration_ms(&self) -> Option<u64> {
        self.engine.duration_ms()
    }

    /// Let the pipeline finish coming up before anything is asked of it.
    ///
    /// A seek issued into a pipeline that has not prerolled is dropped — the
    /// sink then still holds the preroll picture at position 0, so the first
    /// frame asked for came back as the start of the clip whatever time it
    /// wanted. The preroll timestamp is remembered so that picture can never be
    /// mistaken for the answer to a later seek.
    fn await_preroll(&mut self) {
        let start = Instant::now();
        while start.elapsed() < PREROLL_TIMEOUT {
            self.engine.poll();
            if let Some(f) = self.engine.latest_frame() {
                self.last_pts = Some(f.pts_ms);
                return;
            }
            std::thread::sleep(POLL);
        }
    }

    /// Seek to `at_ms` and return the frame there, fitted to `size`. `None`
    /// only when the pipeline produced no picture near that time.
    pub fn frame_at(&mut self, at_ms: u64, size: ThumbSize) -> Option<RgbaThumb> {
        self.engine.seek_ms(at_ms);
        self.engine.play();
        let start = Instant::now();
        let mut fallback: Option<(u32, u32, Arc<Vec<u8>>, u64)> = None;
        while start.elapsed() < FRAME_TIMEOUT {
            self.engine.poll();
            if let Some(f) = self.engine.latest_frame() {
                let got = (f.width, f.height, f.rgba.clone(), f.pts_ms);
                // A frame answers *this* seek only when its own timestamp says
                // so. Accepting whatever picture happened to be on the sink
                // hands back the preroll frame at position 0 on a
                // freshly-opened source.
                let answers_seek = got.3.saturating_add(SEEK_SLOP_MS) >= at_ms;
                let fresh = Some(got.3) != self.last_pts;
                if answers_seek && fresh {
                    self.engine.pause();
                    self.last_pts = Some(got.3);
                    return rgba_to_thumb(got.2.as_ref().clone(), got.0, got.1, size);
                }
                // Keep the closest thing seen, in case the pipeline can only
                // land before the requested time.
                let better = fallback
                    .as_ref()
                    .is_none_or(|(_, _, _, p)| got.3.abs_diff(at_ms) < p.abs_diff(at_ms));
                if better {
                    fallback = Some(got);
                }
            }
            std::thread::sleep(POLL);
        }
        self.engine.pause();
        // The pipeline never landed on the requested time. A frame close by is
        // still a real frame of this shot — a source that can only seek to
        // keyframes looks exactly like this — but one from somewhere else
        // entirely is not an answer.
        let (w, h, rgba, pts) =
            fallback.filter(|(_, _, _, pts)| pts.abs_diff(at_ms) <= FALLBACK_SLOP_MS)?;
        self.last_pts = Some(pts);
        rgba_to_thumb(rgba.as_ref().clone(), w, h, size)
    }
}
