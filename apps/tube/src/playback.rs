//! Playback bridge around an openh264 H.264 decoder.
//!
//! The data plane resolves a real googlevideo H.264 stream
//! ([`crate::backend::YtBackend::resolve_video_stream`]), downloads fragments and
//! demuxes each into per-frame **access units** ([`crate::backend::stream`] →
//! [`crate::backend::mp4::demux_fragment`]). Those access units are pushed here
//! via [`Playback::feed`], one frame at a time; the decoder emits a decoded
//! picture which is copied out as RGBA8 for the watch page's
//! [`crate::widgets::VideoSurface`] to paint. Feeding happens off the UI thread
//! (the fetch/playback worker owns the network + demux + decode); the UI only
//! drains finished frames.
//!
//! ## Why not `ui::widget::VideoPlayer`?
//!
//! `VideoPlayer` decodes through openh264's convenience default, which flushes a
//! frame after **every** decode call. That default prematurely releases the
//! reference frames inter-frames (P-slices) depend on, cascading into
//! `dsRefLost` / bitstream errors that openh264 surfaces as decode failures — in
//! practice dropping ~95% of a real continuous stream's frames. openh264's own
//! docs say to disable automatic flushing when you hit decoder errors, so Tube
//! drives the decoder directly with [`Flush::NoFlush`] and drains any buffered
//! tail once via [`Decoder::flush_remaining`]. A single decoder instance is kept
//! for the whole stream so inter-frames resolve against their prior references.
//! `VideoPlayer` (the toolkit widget) is unchanged and simply not on Tube's
//! decode path — `VideoSurface` does the painting.

use openh264::OpenH264API;
use openh264::decoder::{DecodedYUV, Decoder, DecoderConfig, Flush};
use openh264::formats::YUVSource;

/// A decoded picture, owned and toolkit-free: `(width, height, RGBA8)` with
/// `width * height * 4` bytes.
pub type Frame = (u32, u32, Vec<u8>);

pub struct Playback {
    /// Lazily constructed on the first non-empty [`Playback::feed`] so
    /// constructing a `Playback` never touches the openh264 runtime.
    decoder: Option<Decoder>,
    /// Most recently decoded picture, kept so the watch page can repaint it.
    latest: Option<Frame>,
    frames_decoded: u64,
    playing: bool,
}

impl Default for Playback {
    fn default() -> Self {
        Self::new()
    }
}

impl Playback {
    pub fn new() -> Self {
        Self {
            decoder: None,
            latest: None,
            frames_decoded: 0,
            playing: true,
        }
    }

    /// Construct the decoder with automatic per-decode flushing **disabled**
    /// (see the module docs). Returns `None` if the openh264 runtime fails to
    /// initialise.
    fn make_decoder() -> Option<Decoder> {
        let config = DecoderConfig::new().flush_after_decode(Flush::NoFlush);
        Decoder::with_api_config(OpenH264API::from_source(), config).ok()
    }

    fn yuv_to_rgba(yuv: &DecodedYUV<'_>) -> Frame {
        let (w, h) = yuv.dimensions();
        let mut rgba = vec![0u8; w * h * 4];
        yuv.write_rgba8(&mut rgba);
        (w as u32, h as u32, rgba)
    }

    /// Feed one access unit: Annex-B NAL units for a single frame. Returns
    /// true when a new picture became available (and `latest` updated).
    pub fn feed(&mut self, nalus: &[u8]) -> bool {
        if nalus.is_empty() {
            return false;
        }
        if self.decoder.is_none() {
            match Self::make_decoder() {
                Some(d) => self.decoder = Some(d),
                None => return false,
            }
        }
        let decoder = self.decoder.as_mut().expect("decoder just initialised");
        match decoder.decode(nalus) {
            Ok(Some(yuv)) => {
                self.latest = Some(Self::yuv_to_rgba(&yuv));
                self.frames_decoded += 1;
                true
            }
            // `Ok(None)` = decoder consumed the unit but no picture out yet;
            // `Err` = openh264 rejected the bitstream. Neither yields a frame.
            Ok(None) | Err(_) => false,
        }
    }

    /// Drain any pictures still buffered in the decoder's reorder queue at the
    /// natural end of a stream. With `NoFlush` these accumulate (e.g. B-frame
    /// reordering) until explicitly flushed; each drained picture updates
    /// `latest` and is returned so the caller can emit it.
    pub fn flush(&mut self) -> Vec<Frame> {
        let mut out = Vec::new();
        if let Some(decoder) = self.decoder.as_mut()
            && let Ok(frames) = decoder.flush_remaining()
        {
            for yuv in frames {
                let frame = Self::yuv_to_rgba(&yuv);
                self.frames_decoded += 1;
                out.push(frame);
            }
        }
        if let Some(last) = out.last() {
            self.latest = Some(last.clone());
        }
        out
    }

    /// Whether at least one real frame has decoded (poster → video swap point).
    pub fn has_frame(&self) -> bool {
        self.frames_decoded > 0
    }

    pub fn frames_decoded(&self) -> u64 {
        self.frames_decoded
    }

    pub fn frame_dimensions(&self) -> Option<(u32, u32)> {
        self.latest.as_ref().map(|(w, h, _)| (*w, *h))
    }

    pub fn latest_rgba(&self) -> Option<Frame> {
        self.latest.clone()
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn play(&mut self) {
        self.playing = true;
    }
    pub fn pause(&mut self) {
        self.playing = false;
    }
}
