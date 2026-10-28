//! Editing without waiting: a source's picture at an exact frame
//! ([`FrameSeeker`]), its sound as samples ([`PcmDecoder`]), mixed sound
//! played on the output ([`PcmPlayer`]) and an encoder that writes a
//! finished file ([`ClipEncoder`]).
//!
//! Both are for a host whose thread may never block, which is a browser
//! page: there they run on the browser's own media stack (the media element
//! decodes, `WebCodecs` encodes), so nothing patent-encumbered ships with the
//! page. A native host decodes and encodes through `GStreamer` sessions
//! instead, which may wait; there [`FrameSeeker::open`] answers `None` and
//! [`ClipEncoder::available`] `false`, so an app asks at run time rather than
//! compiling a path per target.
//!
//! Nothing here waits. A seeker is asked for a frame and says which frame
//! it holds on each later call; an encoder takes frames once it is ready and
//! hands back the file once it is done.

use crate::source::Source;

/// A [`Source`] over `bytes` held in memory, for a host whose files have no
/// paths (a browser page): `None` where sources are read from paths.
#[must_use]
pub fn in_memory_source(bytes: &[u8], mime: &str) -> Option<Source> {
    #[cfg(web_page)]
    {
        Some(Source::Remote {
            url: crate::web::blob_url(bytes, mime),
        })
    }
    #[cfg(not(web_page))]
    {
        let _ = (bytes, mime);
        None
    }
}

/// The frame of a source running at `fps` that is showing at `seconds`.
///
/// Matches the page's own arithmetic (`frameAt` in the page module), so the
/// frame asked for and the frame reported held are counted alike.
#[must_use]
pub fn frame_at(seconds: f64, fps: f64) -> u64 {
    let fps = if fps.is_finite() && fps > 0.0 {
        fps
    } else {
        30.0
    };
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "floored and clamped to zero first"
    )]
    let frame = (seconds * fps + 1e-6).floor().max(0.0) as u64;
    frame
}

/// One source's pictures, frame by frame.
pub struct FrameSeeker {
    #[cfg(web_page)]
    js: crate::web_edit::TiloFrameSeeker,
    #[cfg(not(web_page))]
    never: std::convert::Infallible,
}

/// A picture a [`FrameSeeker`] holds, as tightly packed RGBA.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldPicture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl FrameSeeker {
    /// Whether seekers open on this host at all.
    #[must_use]
    pub fn available() -> bool {
        cfg!(web_page)
    }

    /// A seeker over `source`, or `None` where this host decodes through
    /// sessions that may wait, or where `source` cannot be loaded this way.
    #[must_use]
    pub fn open(source: &Source) -> Option<Self> {
        #[cfg(web_page)]
        {
            let Source::Remote { url } = source else {
                return None;
            };
            let js = crate::web_edit::TiloFrameSeeker::new().ok()?;
            js.open(url);
            Some(Self { js })
        }
        #[cfg(not(web_page))]
        {
            let _ = source;
            None
        }
    }

    /// A seeker over the still picture `source`: it holds frame 0, whatever
    /// time is asked, once the picture is decoded.
    #[must_use]
    pub fn open_still(source: &Source) -> Option<Self> {
        #[cfg(web_page)]
        {
            let Source::Remote { url } = source else {
                return None;
            };
            let js = crate::web_edit::TiloFrameSeeker::new().ok()?;
            js.open_image(url);
            Some(Self { js })
        }
        #[cfg(not(web_page))]
        {
            let _ = source;
            None
        }
    }

    /// Hold the picture of the frame showing at `seconds` of a source
    /// running at `fps`, exactly; [`Self::shown_frame`] says when it is held.
    pub fn seek(&self, seconds: f64, fps: f64) {
        #[cfg(web_page)]
        self.js.seek(seconds, fps);
        #[cfg(not(web_page))]
        {
            let _ = (seconds, fps);
            match self.never {}
        }
    }

    /// Play along with a playhead moving forward at `rate` times normal
    /// speed from `seconds`: smooth pictures, each the one shown as the
    /// source plays at that rate, until [`Self::seek`] holds an exact one
    /// again.
    pub fn follow(&self, seconds: f64, fps: f64, rate: f64) {
        #[cfg(web_page)]
        self.js.follow(seconds, fps, rate);
        #[cfg(not(web_page))]
        {
            let _ = (seconds, fps, rate);
            match self.never {}
        }
    }

    /// The frame shown now, or `None` while there is none or the seeker is
    /// on its way to the one asked for.
    #[must_use]
    pub fn shown_frame(&self) -> Option<u64> {
        #[cfg(web_page)]
        {
            let frame = self.js.shown_frame();
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a frame index, checked whole and non-negative"
            )]
            (frame.is_finite() && frame >= 0.0).then_some(frame as u64)
        }
        #[cfg(not(web_page))]
        match self.never {}
    }

    /// The source's picture size, zero before it has loaded.
    #[must_use]
    pub fn size(&self) -> (u32, u32) {
        #[cfg(web_page)]
        {
            (self.js.width(), self.js.height())
        }
        #[cfg(not(web_page))]
        match self.never {}
    }

    /// The picture shown now, scaled to fit `max_width`x`max_height`, or
    /// `None` while there is none.
    #[must_use]
    pub fn picture(&self, max_width: u32, max_height: u32) -> Option<HeldPicture> {
        #[cfg(web_page)]
        {
            let rgba = self.js.picture(max_width.max(1), max_height.max(1));
            let (width, height) = (self.js.picture_width(), self.js.picture_height());
            let whole = (width as usize)
                .checked_mul(height as usize)
                .and_then(|pixels| pixels.checked_mul(4));
            (!rgba.is_empty() && whole == Some(rgba.len())).then_some(HeldPicture {
                width,
                height,
                rgba,
            })
        }
        #[cfg(not(web_page))]
        {
            let _ = (max_width, max_height);
            match self.never {}
        }
    }

    /// Why the source cannot be shown, once.
    #[must_use]
    pub fn take_error(&self) -> Option<String> {
        #[cfg(web_page)]
        {
            self.js.take_error()
        }
        #[cfg(not(web_page))]
        match self.never {}
    }
}

#[cfg(web_page)]
impl Drop for FrameSeeker {
    fn drop(&mut self) {
        self.js.close();
    }
}

/// Where a [`ClipEncoder`] has got to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EncoderState {
    /// Choosing a codec; frames are not taken yet.
    Opening,
    /// Taking frames.
    Ready,
    /// Encoding what it was given and writing the file.
    Finishing,
    /// The file is written.
    Done,
    /// It stopped, and why.
    Failed(String),
}

/// A video file encoded without waiting.
///
/// On a browser page it picks the best codec the browser encodes (VP9, then
/// AV1, then VP8, then the browser's own H.264) and writes `WebM`, or Matroska
/// when the codec is H.264; nothing patent-encumbered ships with the page.
pub struct ClipEncoder {
    #[cfg(web_page)]
    js: crate::web_edit::TiloClipEncoder,
    #[cfg(not(web_page))]
    never: std::convert::Infallible,
}

impl ClipEncoder {
    /// Whether this host encodes this way at all.
    #[must_use]
    pub fn available() -> bool {
        #[cfg(web_page)]
        {
            crate::web_edit::video_encoder_available()
        }
        #[cfg(not(web_page))]
        {
            false
        }
    }

    /// Start an encoder for `width`x`height` frames at `fps`, aiming at
    /// `bitrate` bits per second. With `sound`, the file also carries the
    /// sound given to [`Self::encode_sound`], where the host encodes Opus
    /// ([`Self::has_sound`] says, once ready).
    ///
    /// # Errors
    /// Why no encoder could be made here.
    pub fn start(
        width: u32,
        height: u32,
        fps: f64,
        bitrate: f64,
        sound: bool,
    ) -> Result<Self, String> {
        #[cfg(web_page)]
        {
            crate::web_edit::TiloClipEncoder::new(width, height, fps, bitrate, sound)
                .map(|js| Self { js })
                .map_err(|e| {
                    e.as_string()
                        .unwrap_or_else(|| "this browser cannot encode video".into())
                })
        }
        #[cfg(not(web_page))]
        {
            let _ = (width, height, fps, bitrate, sound);
            Err("this build encodes through GStreamer".into())
        }
    }

    #[must_use]
    pub fn state(&self) -> EncoderState {
        #[cfg(web_page)]
        match self.js.state().as_str() {
            "opening" => EncoderState::Opening,
            "ready" => EncoderState::Ready,
            "finishing" => EncoderState::Finishing,
            "done" => EncoderState::Done,
            _ => EncoderState::Failed(self.js.take_reason()),
        }
        #[cfg(not(web_page))]
        match self.never {}
    }

    /// The codec it encodes with, once ready (`vp09.00.40.08`, ...).
    #[must_use]
    pub fn codec(&self) -> String {
        #[cfg(web_page)]
        {
            self.js.codec()
        }
        #[cfg(not(web_page))]
        match self.never {}
    }

    /// Frames handed over that the encoder has not got through yet.
    #[must_use]
    pub fn backlog(&self) -> u32 {
        #[cfg(web_page)]
        {
            self.js.backlog()
        }
        #[cfg(not(web_page))]
        match self.never {}
    }

    /// Encode frame `index` from tightly packed RGBA of the size it was
    /// started with; `key` asks for a frame a player can start from.
    /// `false` once the encoder has failed.
    pub fn encode(&self, rgba: &[u8], index: u64, key: bool) -> bool {
        #[cfg(web_page)]
        {
            #[expect(
                clippy::cast_precision_loss,
                reason = "a frame index within an export is far inside f64's exact range"
            )]
            let index = index as f64;
            self.js.encode(rgba, index, key)
        }
        #[cfg(not(web_page))]
        {
            let _ = (rgba, index, key);
            match self.never {}
        }
    }

    /// Whether the file carries sound, once ready.
    #[must_use]
    pub fn has_sound(&self) -> bool {
        #[cfg(web_page)]
        {
            self.js.has_sound()
        }
        #[cfg(not(web_page))]
        match self.never {}
    }

    /// Encode interleaved stereo `samples` at [`SAMPLE_RATE`], starting
    /// `start` samples into the file. Nothing is written when the file
    /// carries no sound; `false` once the encoder has failed.
    pub fn encode_sound(&self, samples: &[f32], start: u64) -> bool {
        #[cfg(web_page)]
        {
            #[expect(
                clippy::cast_precision_loss,
                reason = "a sample count within an export is far inside f64's exact range"
            )]
            let start = start as f64;
            self.js.encode_sound(samples, start)
        }
        #[cfg(not(web_page))]
        {
            let _ = (samples, start);
            match self.never {}
        }
    }

    /// No more frames: encode the rest and write the file.
    pub fn finish(&self) {
        #[cfg(web_page)]
        self.js.finish();
        #[cfg(not(web_page))]
        match self.never {}
    }

    /// The finished file, once [`EncoderState::Done`].
    #[must_use]
    pub fn take_bytes(&self) -> Vec<u8> {
        #[cfg(web_page)]
        {
            self.js.take_bytes()
        }
        #[cfg(not(web_page))]
        match self.never {}
    }
}

#[cfg(web_page)]
impl Drop for ClipEncoder {
    fn drop(&mut self) {
        self.js.close();
    }
}

/// The rate [`PcmDecoder`] decodes to and [`PcmPlayer`] and
/// [`ClipEncoder::encode_sound`] take samples at, in stereo.
pub const SAMPLE_RATE: u32 = 48_000;

/// Where a [`PcmDecoder`] has got to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PcmState {
    Decoding,
    /// The samples are here.
    Ready,
    /// The source has no sound this host decodes; it mixes as silence.
    None,
}

/// A source's whole sound, decoded once to interleaved stereo at
/// [`SAMPLE_RATE`] by the host's own audio decoding, without waiting.
pub struct PcmDecoder {
    #[cfg(web_page)]
    js: crate::web_edit::TiloPcmDecoder,
    #[cfg(not(web_page))]
    never: std::convert::Infallible,
}

impl PcmDecoder {
    /// Start decoding `source`, or `None` where this host decodes through
    /// sessions that may wait instead.
    #[must_use]
    pub fn open(source: &Source) -> Option<Self> {
        #[cfg(web_page)]
        {
            let Source::Remote { url } = source else {
                return None;
            };
            crate::web_edit::TiloPcmDecoder::new(url)
                .ok()
                .map(|js| Self { js })
        }
        #[cfg(not(web_page))]
        {
            let _ = source;
            None
        }
    }

    #[must_use]
    pub fn state(&self) -> PcmState {
        #[cfg(web_page)]
        match self.js.state().as_str() {
            "decoding" => PcmState::Decoding,
            "ready" => PcmState::Ready,
            _ => PcmState::None,
        }
        #[cfg(not(web_page))]
        match self.never {}
    }

    /// `frames` interleaved stereo frames from frame `from`: short at the
    /// end, empty until [`PcmState::Ready`].
    #[must_use]
    pub fn samples(&self, from: u64, frames: usize) -> Vec<f32> {
        #[cfg(web_page)]
        {
            #[expect(
                clippy::cast_precision_loss,
                reason = "sample positions are far inside f64's exact range"
            )]
            let (from, frames) = (from as f64, frames as f64);
            self.js.samples(from, frames)
        }
        #[cfg(not(web_page))]
        {
            let _ = (from, frames);
            match self.never {}
        }
    }
}

#[cfg(web_page)]
impl Drop for PcmDecoder {
    fn drop(&mut self) {
        self.js.close();
    }
}

/// Mixed sound on the host's output, each block played at the timeline time
/// it was mixed for, without waiting.
pub struct PcmPlayer {
    #[cfg(web_page)]
    js: crate::web_edit::TiloPcmPlayer,
    #[cfg(not(web_page))]
    never: std::convert::Infallible,
}

impl PcmPlayer {
    /// A player, or `None` where this host plays sound through sessions
    /// that may wait instead.
    #[must_use]
    pub fn open() -> Option<Self> {
        #[cfg(web_page)]
        {
            crate::web_edit::TiloPcmPlayer::new()
                .ok()
                .map(|js| Self { js })
        }
        #[cfg(not(web_page))]
        {
            None
        }
    }

    /// Begin playing timeline second `at` a moment from now, silencing
    /// what was scheduled. `false` where the output cannot open.
    pub fn start(&self, at: f64) -> bool {
        #[cfg(web_page)]
        {
            self.js.start(at)
        }
        #[cfg(not(web_page))]
        {
            let _ = at;
            match self.never {}
        }
    }

    /// The timeline second the output is playing now.
    #[must_use]
    pub fn position(&self) -> f64 {
        #[cfg(web_page)]
        {
            self.js.position()
        }
        #[cfg(not(web_page))]
        match self.never {}
    }

    /// Play interleaved stereo `samples` at [`SAMPLE_RATE`], mixed for
    /// timeline second `at`.
    pub fn push(&self, samples: &[f32], at: f64) {
        #[cfg(web_page)]
        self.js.push(samples, at);
        #[cfg(not(web_page))]
        {
            let _ = (samples, at);
            match self.never {}
        }
    }

    /// Silence what is scheduled.
    pub fn stop(&self) {
        #[cfg(web_page)]
        self.js.stop();
        #[cfg(not(web_page))]
        match self.never {}
    }
}

#[cfg(web_page)]
impl Drop for PcmPlayer {
    fn drop(&mut self) {
        self.js.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_time_names_the_frame_whose_interval_holds_it() {
        // Frame 62 at 30 fps starts at 2066.67 ms: the millisecond before it
        // is still frame 61, the instant itself is 62.
        assert_eq!(frame_at(2.066, 30.0), 61);
        assert_eq!(frame_at(62.0 / 30.0, 30.0), 62);
        // Frame starts that land a hair under the boundary in floating point
        // still count as that frame.
        assert_eq!(frame_at(0.1 * 3.0, 10.0), 3);
        assert_eq!(frame_at(-1.0, 30.0), 0);
        assert_eq!(frame_at(1.0, 0.0), 30, "an unknown rate counts at 30");
    }

    #[test]
    fn a_native_host_seeks_and_encodes_through_its_sessions_instead() {
        let source = Source::LocalFile("/nonexistent/clip.mp4".into());
        assert!(!FrameSeeker::available());
        assert!(FrameSeeker::open(&source).is_none());
        assert!(!ClipEncoder::available());
        assert!(ClipEncoder::start(64, 64, 30.0, 1e6, true).is_err());
        assert!(FrameSeeker::open_still(&source).is_none());
        assert!(PcmDecoder::open(&source).is_none());
        assert!(PcmPlayer::open().is_none());
        assert!(in_memory_source(b"bytes", "video/mp4").is_none());
    }
}
