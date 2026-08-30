//! Representative-keyframe extraction from a video (feature `video`).
//!
//! Reuses the shared `media` GStreamer engine — the exact decode path
//! `apps/video_editor` drives — rather than a second codec stack: open the
//! file, seek a little way in (past a black intro frame), play, and poll until
//! a decoded RGBA frame lands or a deadline passes. Only compiled under
//! `--features video` (which turns on `media/decode`); without it the caller
//! draws a video type badge instead.

use std::path::Path;
use std::time::{Duration, Instant};

use media::{MediaEngine, Source};

use crate::generate::{RgbaThumb, rgba_to_thumb};

/// How long to wait for the pipeline to produce the first decoded frame.
const DEADLINE: Duration = Duration::from_secs(6);
/// Seek this far in before grabbing a frame — most clips open on black/near-black.
const SEEK_MS: u64 = 1_000;

/// Extract a representative keyframe from `path` as an RGBA thumbnail no wider
/// than `max_edge`. `None` if real decode is unavailable or no frame arrives.
pub fn video_thumb(path: &Path, max_edge: u32) -> Option<RgbaThumb> {
    let mut engine = MediaEngine::new();
    // With the `decode` feature off the engine only synthesises frames; never
    // pass those off as a real preview.
    if !engine.is_real_decode() {
        return None;
    }
    engine.open(Source::LocalFile(path.to_path_buf()));
    // Seek past the intro when the clip is long enough, else start at zero.
    let target = match engine.duration_ms() {
        Some(d) if d > SEEK_MS * 2 => SEEK_MS,
        _ => 0,
    };
    if target > 0 {
        engine.seek_ms(target);
    }
    engine.play();

    let start = Instant::now();
    while start.elapsed() < DEADLINE {
        let _ = engine.poll();
        if let Some(frame) = engine.latest_frame() {
            let (w, h) = (frame.width, frame.height);
            if w > 0 && h > 0 && frame.rgba.len() == (w as usize) * (h as usize) * 4 {
                return rgba_to_thumb(frame.rgba.as_ref().clone(), w, h, max_edge);
            }
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    None
}
