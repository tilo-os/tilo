//! GStreamer video decode + playback. **Compiled only under `--features
//! media`.**
//!
//! This replaces the openh264-only decode path ([`crate::playback`]) with a
//! GStreamer pipeline that decodes **any** codec YouTube serves — VP9, AV1 and
//! H.264 — plus audio, and handles adaptive/progressive containers, because
//! `uridecodebin` negotiates the demuxer + decoder itself. The stream URL is
//! resolved by yt-dlp ([`crate::backend::ytdlp`]); GStreamer does the rest.
//!
//! The idiom matches `apps/camera` and `apps/music`: a `playbin` whose **video**
//! output is diverted through `videoconvert ! appsink` so the watch page paints
//! decoded RGBA frames, while `playbin` routes any **audio** to the system sink
//! automatically. Frames are pulled on the worker's playback thread — never the
//! UI thread — and handed to the UI as toolkit-free `RGBA8` bytes
//! ([`crate::api::DecodedFrame`]), the same shape [`crate::playback`] produces,
//! so the [`crate::widgets::VideoSurface`] paint path is unchanged.
//!
//! The appsink runs with `sync=true`, so pulled frames are already paced to the
//! stream's presentation timestamps (real-time playback) — no hand-rolled
//! frame-rate sleeping like the openh264 path needed.

#![cfg(feature = "media")]

use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;

/// A decoded picture: `(width, height, RGBA8)` with `width * height * 4` bytes.
pub type Frame = (u32, u32, Vec<u8>);

/// The outcome of one [`GstPlayback::pull`].
pub enum Pull {
    /// A decoded RGBA frame.
    Frame(Frame),
    /// No frame within the timeout (still buffering / paused) — try again.
    Timeout,
    /// The stream ended.
    Eos,
    /// A fatal pipeline error.
    Err(String),
}

/// A running `GStreamer` playback session. Drop (or [`GstPlayback::stop`]) tears
/// the pipeline down.
pub struct GstPlayback {
    /// The `playbin` (or, in tests, a parsed pipeline). Both are `GstPipeline`
    /// subclasses, so an `Element` handle drives state + teardown for either.
    pipeline: gst::Element,
    appsink: gst_app::AppSink,
}

impl GstPlayback {
    /// Initialise `GStreamer`. Idempotent; safe to call repeatedly.
    pub fn init() -> Result<(), String> {
        gst::init().map_err(|e| format!("gst init: {e}"))
    }

    /// Open a directly-playable media `uri` (a yt-dlp-resolved googlevideo URL)
    /// and start decoding. Video is diverted to an appsink emitting RGBA; audio
    /// (when the format is muxed) plays through the system sink.
    pub fn open(uri: &str) -> Result<Self, String> {
        Self::init()?;
        let playbin = gst::ElementFactory::make("playbin")
            .property("uri", uri)
            .build()
            .map_err(|e| format!("make playbin: {e}"))?;

        // videoconvert → appsink(RGBA), ghosting the videoconvert sink pad so
        // playbin can feed decoded video into the bin. `sync=true` paces the
        // pulled frames to the stream's PTS (real-time playback).
        let sink_bin = gst::parse::bin_from_description(
            "videoconvert ! appsink name=tubesink caps=video/x-raw,format=RGBA \
             sync=true max-buffers=4 drop=false",
            true,
        )
        .map_err(|e| format!("build video sink: {e}"))?;
        let appsink = sink_bin
            .by_name("tubesink")
            .ok_or("appsink 'tubesink' missing")?
            .downcast::<gst_app::AppSink>()
            .map_err(|_| "sink is not an appsink".to_string())?;
        playbin.set_property("video-sink", &sink_bin);

        playbin
            .set_state(gst::State::Playing)
            .map_err(|e| format!("set playing: {e}"))?;
        Ok(Self {
            pipeline: playbin,
            appsink,
        })
    }

    /// Pull the next decoded frame, waiting up to `timeout`.
    pub fn pull(&self, timeout: Duration) -> Pull {
        let clock = gst::ClockTime::from_nseconds(timeout.as_nanos() as u64);
        match self.appsink.try_pull_sample(Some(clock)) {
            Some(sample) => match frame_from_sample(&sample) {
                Ok(f) => Pull::Frame(f),
                Err(e) => Pull::Err(e),
            },
            None => {
                if self.appsink.is_eos() {
                    Pull::Eos
                } else {
                    Pull::Timeout
                }
            }
        }
    }

    /// Hold or resume playback. Pausing stops the pipeline clock so
    /// [`Self::pull`] just times out until resumed.
    pub fn set_paused(&self, paused: bool) {
        let state = if paused {
            gst::State::Paused
        } else {
            gst::State::Playing
        };
        let _ = self.pipeline.set_state(state);
    }

    /// Current playback position in seconds, if the pipeline can report one.
    pub fn position_secs(&self) -> Option<f64> {
        self.pipeline
            .query_position::<gst::ClockTime>()
            .map(|p| p.nseconds() as f64 / 1_000_000_000.0)
    }

    /// Stop and tear down the pipeline.
    pub fn stop(self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }

    /// Build a session directly from a `GStreamer` pipeline description that
    /// contains an `appsink name=tubesink`. Used by the hermetic test harness
    /// (e.g. a `videotestsrc` source) to exercise the sample→RGBA extraction
    /// without a network or a real stream.
    #[cfg(test)]
    pub fn from_description(desc: &str) -> Result<Self, String> {
        Self::init()?;
        let pipeline = gst::parse::launch(desc)
            .map_err(|e| format!("parse pipeline: {e}"))?
            .downcast::<gst::Pipeline>()
            .map_err(|_| "not a pipeline".to_string())?;
        let appsink = pipeline
            .by_name("tubesink")
            .ok_or("appsink 'tubesink' missing")?
            .downcast::<gst_app::AppSink>()
            .map_err(|_| "sink is not an appsink".to_string())?;
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| format!("set playing: {e}"))?;
        Ok(Self {
            pipeline: pipeline.upcast(),
            appsink,
        })
    }
}

impl Drop for GstPlayback {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

/// Copy one RGBA sample out of the appsink into an owned, tightly-packed buffer,
/// honouring `GStreamer`'s row stride (which may be padded past `width * 4`).
fn frame_from_sample(sample: &gst::Sample) -> Result<Frame, String> {
    let caps = sample.caps().ok_or("sample has no caps")?;
    let info = gst_video::VideoInfo::from_caps(caps).map_err(|_| "bad VideoInfo".to_string())?;
    let buffer = sample.buffer().ok_or("sample has no buffer")?;
    let frame = gst_video::VideoFrameRef::from_buffer_ref_readable(buffer, &info)
        .map_err(|_| "map video frame".to_string())?;

    let w = info.width() as usize;
    let h = info.height() as usize;
    let stride = info.stride()[0] as usize;
    let data = frame.plane_data(0).map_err(|_| "plane data".to_string())?;
    let row_bytes = w * 4;
    if stride < row_bytes || data.len() < stride * h {
        return Err(format!(
            "unexpected frame layout: stride={stride} row_bytes={row_bytes} len={} for {w}x{h}",
            data.len()
        ));
    }
    let mut rgba = Vec::with_capacity(row_bytes * h);
    for y in 0..h {
        let start = y * stride;
        rgba.extend_from_slice(&data[start..start + row_bytes]);
    }
    Ok((info.width(), info.height(), rgba))
}

/// Decode up to `max_frames` frames from `uri`, giving up after `overall`
/// wall-time. Used by the `--probe-gst` proof path.
pub fn decode_frames(
    uri: &str,
    max_frames: usize,
    overall: Duration,
) -> Result<Vec<Frame>, String> {
    let play = GstPlayback::open(uri)?;
    let deadline = std::time::Instant::now() + overall;
    let mut out = Vec::new();
    while out.len() < max_frames && std::time::Instant::now() < deadline {
        match play.pull(Duration::from_secs(5)) {
            Pull::Frame(f) => out.push(f),
            Pull::Timeout => continue,
            Pull::Eos => break,
            Pull::Err(e) => return Err(e),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The appsink→RGBA extraction must produce a correctly-sized frame from a
    /// synthetic `videotestsrc` source — hermetic proof of the decode/convert
    /// plumbing with no network or real stream. This is the exact path a real
    /// googlevideo stream takes once `uridecodebin` has produced raw video.
    #[test]
    fn videotestsrc_yields_a_correct_rgba_frame() {
        let play = GstPlayback::from_description(
            "videotestsrc num-buffers=3 ! videoconvert ! \
             video/x-raw,format=RGBA,width=64,height=48 ! \
             appsink name=tubesink sync=false max-buffers=4",
        )
        .expect("build test pipeline");

        let mut got = None;
        for _ in 0..20 {
            match play.pull(Duration::from_secs(2)) {
                Pull::Frame(f) => {
                    got = Some(f);
                    break;
                }
                Pull::Timeout => continue,
                Pull::Eos => break,
                Pull::Err(e) => panic!("pipeline error: {e}"),
            }
        }
        let (w, h, rgba) = got.expect("a decoded RGBA frame");
        assert_eq!((w, h), (64, 48));
        assert_eq!(rgba.len(), 64 * 48 * 4, "tightly-packed RGBA8");
        // videotestsrc's default pattern is not uniformly zero.
        assert!(rgba.iter().any(|&b| b != 0), "frame carries real pixels");
    }
}
