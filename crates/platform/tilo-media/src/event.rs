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
    /// No installed decoder handles one of the source's streams. Comes in the
    /// same poll as, and just before, the [`MediaEvent::Error`] that stops
    /// playback, so a player can offer to add formats instead of only showing
    /// the reason. `codec` is a human name ("H.264", "HEVC", "AAC").
    MissingDecoder { codec: String },
    /// The host relaying a page's source (its fetch proxy) streamed as much
    /// as it allows this visitor or room for now. Comes in the same poll as,
    /// and just before, the [`MediaEvent::Error`] that stops playback, so a
    /// player can say that playback may resume later rather than that the
    /// source is broken.
    LimitReached,
}

/// The human name of the codec a stream of `media_type` (a caps name such as
/// `video/x-h265`) carries; `mpeg_version` is the caps' `mpegversion`, which
/// tells MPEG audio and video apart. Unknown types keep their caps name.
#[must_use]
pub fn codec_name(media_type: &str, mpeg_version: Option<i32>) -> String {
    let name = match (media_type, mpeg_version) {
        ("video/x-h264", _) => "H.264",
        ("video/x-h265", _) => "HEVC",
        ("video/x-vp8", _) => "VP8",
        ("video/x-vp9", _) => "VP9",
        ("video/x-av1", _) => "AV1",
        ("video/mpeg", Some(1)) => "MPEG-1",
        ("video/mpeg", Some(2)) => "MPEG-2",
        ("video/mpeg", Some(4)) => "MPEG-4",
        ("audio/mpeg", Some(1)) => "MP3",
        ("audio/mpeg", Some(2 | 4)) => "AAC",
        ("audio/x-ac3", _) => "AC-3",
        ("audio/x-eac3", _) => "E-AC-3",
        ("audio/x-dts", _) => "DTS",
        ("audio/x-true-hd", _) => "TrueHD",
        ("audio/x-opus", _) => "Opus",
        ("audio/x-vorbis", _) => "Vorbis",
        ("audio/x-flac", _) => "FLAC",
        _ => return media_type.to_owned(),
    };
    name.to_owned()
}

#[cfg(test)]
mod tests {
    use super::codec_name;

    #[test]
    fn codecs_have_human_names() {
        assert_eq!(codec_name("video/x-h264", None), "H.264");
        assert_eq!(codec_name("video/x-h265", None), "HEVC");
        assert_eq!(codec_name("audio/mpeg", Some(4)), "AAC");
        assert_eq!(codec_name("audio/mpeg", Some(1)), "MP3");
        assert_eq!(codec_name("video/mpeg", Some(2)), "MPEG-2");
        assert_eq!(codec_name("video/x-weird", None), "video/x-weird");
    }
}

/// A picture that stays where the page's media element shows it: a renderer
/// copies it from `element` on the GPU, instead of the engine reading its
/// pixels back ([`crate::MediaEngine::take_element_picture`]). `element` is
/// the page's video element (a `wasm_bindgen::JsValue`), opaque here.
#[derive(Clone)]
pub struct ElementPicture {
    pub width: u32,
    pub height: u32,
    /// Counts the pictures the element showed: a new one, a new serial.
    pub serial: u64,
    pub element: std::rc::Rc<dyn std::any::Any>,
}

impl std::fmt::Debug for ElementPicture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ElementPicture")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("serial", &self.serial)
            .finish_non_exhaustive()
    }
}
