//! The toolkit-free frame + event vocabulary the engine speaks.
//!
//! The `GStreamer` backend and synthesized fallback produce the same
//! [`VideoFrame`] (tightly packed RGBA8) and [`MediaEvent`] stream.

use std::sync::Arc;

/// One decoded picture, owned and render-agnostic: tightly-packed RGBA8 with
/// `width * height * 4` bytes. `Arc`-backed so handing the same frame to the UI
/// scene every idle frame is cheap (no re-copy while the playhead is paused).
#[derive(Clone)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, row-major, no stride padding.
    pub rgba: Arc<Vec<u8>>,
    /// Presentation timestamp of this frame, in milliseconds from the start of
    /// the stream. The widget uses it to keep the scrubber accurate when the
    /// pipeline's position query lags the last pulled frame.
    pub pts_ms: u64,
}

impl VideoFrame {
    pub fn new(width: u32, height: u32, rgba: Vec<u8>, pts_ms: u64) -> Self {
        debug_assert_eq!(
            rgba.len(),
            (width as usize)
                .saturating_mul(height as usize)
                .saturating_mul(4)
        );
        Self {
            width,
            height,
            rgba: Arc::new(rgba),
            pts_ms,
        }
    }
}

impl std::fmt::Debug for VideoFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoFrame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("bytes", &self.rgba.len())
            .field("pts_ms", &self.pts_ms)
            .finish()
    }
}

/// An out-of-band signal from the engine, drained once per frame via
/// [`crate::MediaEngine::poll`]. Ordering within a single `poll` is stable:
/// a freshly-opened source always emits [`MediaEvent::DurationKnown`] before
/// its first [`MediaEvent::Position`], and [`MediaEvent::Ended`] is always the
/// last event of the poll in which the playhead reached the duration.
#[derive(Clone, Debug, PartialEq)]
pub enum MediaEvent {
    /// Fetch/prefetch progress, 0..=100. Emitted only when the percentage
    /// changes, so a consumer can drive a buffered-range indicator without
    /// debouncing.
    Buffering { percent: u8 },
    /// The monotonic playhead advanced (or was seeked) to this millisecond.
    Position { ms: u64 },
    /// The total stream duration became known (once per open).
    DurationKnown { ms: u64 },
    /// The stream reached its end; playback has stopped at the duration.
    Ended,
    /// A fatal pipeline error. The engine has stopped; the string is a
    /// human-readable reason for a status line, never a control-flow token.
    Error(String),
}
