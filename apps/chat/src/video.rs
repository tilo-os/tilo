//! Call video: camera + screen capture, H.264 encode, and remote decode.
//!
//! The codec is **openh264** built from source (no system lib — the same
//! decoder [`ui::widget::VideoPlayer::feed_frame`] uses), so an encoded access
//! unit produced here decodes identically on the receiving peer's call tile.
//! Capture is **GStreamer**: `autovideosrc` for the webcam and `pipewiresrc`
//! for a portal ScreenCast node — both yield tightly-packed RGBA frames that
//! [`H264Encoder`] turns into Annex-B NALUs to publish. The whole module is
//! compiled only under `live-media`.
//!
//! Capture needs a device (camera) or a live portal (screen), so those
//! pipelines return `None` when absent and the call falls back to the honest
//! "camera off" placeholder. The codec itself is device-free and is exercised
//! by a real encode→decode round-trip test.

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate};
use openh264::formats::{RgbaSliceU8, YUVBuffer, YUVSource};
use openh264::{OpenH264API, Timestamp};

/// Default capture geometry for a call (VGA is plenty for a face tile and keeps
/// the software encoder real-time).
pub const DEFAULT_W: u32 = 640;
pub const DEFAULT_H: u32 = 480;
pub const DEFAULT_FPS: u32 = 30;

fn ensure_init() -> bool {
    gst::init().is_ok()
}

/// Streaming H.264 encoder: RGBA frame → Annex-B NALU bytes. Mirrors the
/// compositor's surface encoder so the wire format is identical across the OS.
pub struct H264Encoder {
    encoder: Encoder,
    width: u32,
    height: u32,
    fps: u32,
    frame_idx: u64,
}

impl H264Encoder {
    /// Build an encoder for `width × height` at `fps`. `None` if openh264 can't
    /// initialise.
    pub fn new(width: u32, height: u32, fps: u32) -> Option<Self> {
        let config = EncoderConfig::new()
            .max_frame_rate(FrameRate::from_hz(fps.max(1) as f32))
            .num_threads(1)
            .bitrate(BitRate::from_bps(2_000_000))
            .skip_frames(false);
        let encoder = Encoder::with_api_config(OpenH264API::from_source(), config).ok()?;
        Some(Self { encoder, width, height, fps: fps.max(1), frame_idx: 0 })
    }

    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Encode one tightly-packed RGBA frame (`width*height*4` bytes). Returns
    /// the Annex-B access unit (SPS/PPS on the first/keyframes). Empty on a size
    /// mismatch or backend error — never panics.
    pub fn encode(&mut self, rgba: &[u8]) -> Vec<u8> {
        let expected = self.width as usize * self.height as usize * 4;
        if rgba.len() != expected {
            return Vec::new();
        }
        let ts_ms = self.frame_idx.saturating_mul(1000) / self.fps as u64;
        self.frame_idx = self.frame_idx.saturating_add(1);
        let source = RgbaSliceU8::new(rgba, (self.width as usize, self.height as usize));
        let yuv = YUVBuffer::from_rgb_source(source);
        match self.encoder.encode_at(&yuv, Timestamp::from_millis(ts_ms)) {
            Ok(stream) => stream.to_vec(),
            Err(_) => Vec::new(),
        }
    }
}

/// One decoded video frame: tightly-packed RGBA plus its dimensions.
#[derive(Clone)]
pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Per-peer H.264 decoder producing RGBA frames for a remote call tile. Wraps
/// the same openh264 decoder the `ui` video player uses.
pub struct H264FrameDecoder {
    decoder: Option<openh264::decoder::Decoder>,
}

impl Default for H264FrameDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl H264FrameDecoder {
    pub fn new() -> Self {
        Self { decoder: None }
    }

    /// Decode an Annex-B access unit into an RGBA frame, or `None` if the
    /// decoder needs more data / the bitstream is invalid.
    pub fn decode(&mut self, nalus: &[u8]) -> Option<DecodedFrame> {
        if nalus.is_empty() {
            return None;
        }
        if self.decoder.is_none() {
            self.decoder = openh264::decoder::Decoder::new().ok();
        }
        let decoder = self.decoder.as_mut()?;
        let yuv = decoder.decode(nalus).ok().flatten()?;
        let (width, height) = yuv.dimensions();
        let mut rgba = vec![0u8; width * height * 4];
        yuv.write_rgba8(&mut rgba);
        Some(DecodedFrame { width: width as u32, height: height as u32, rgba })
    }
}

fn rgba_capture_pipeline(desc: &str) -> Option<(gst::Pipeline, gst_app::AppSink)> {
    if !ensure_init() {
        return None;
    }
    let pipeline = gst::parse::launch(desc).ok()?.downcast::<gst::Pipeline>().ok()?;
    let sink = pipeline.by_name("sink")?.downcast::<gst_app::AppSink>().ok()?;
    if pipeline.set_state(gst::State::Playing).is_err() {
        return None;
    }
    Some((pipeline, sink))
}

/// Live webcam → RGBA frames. `None` when no camera device is present.
pub struct CameraCapture {
    pipeline: gst::Pipeline,
    sink: gst_app::AppSink,
    pub width: u32,
    pub height: u32,
}

impl CameraCapture {
    pub fn new(width: u32, height: u32) -> Option<Self> {
        let desc = format!(
            "autovideosrc ! videoconvert ! videoscale \
             ! video/x-raw,format=RGBA,width={width},height={height} \
             ! appsink name=sink sync=false max-buffers=2 drop=true"
        );
        let (pipeline, sink) = rgba_capture_pipeline(&desc)?;
        Some(Self { pipeline, sink, width, height })
    }

    /// Non-blocking: the most recent captured RGBA frame, if any (older queued
    /// frames are coalesced by `drop=true`).
    pub fn poll(&mut self) -> Option<Vec<u8>> {
        let mut latest = None;
        while let Some(sample) = self.sink.try_pull_sample(gst::ClockTime::ZERO) {
            if let Some(buffer) = sample.buffer() {
                if let Ok(map) = buffer.map_readable() {
                    latest = Some(map.as_slice().to_vec());
                }
            }
        }
        latest
    }
}

impl Drop for CameraCapture {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

/// Portal ScreenCast PipeWire node → RGBA frames. `None` when the `pipewiresrc`
/// element is unavailable or the node can't be opened. `node_id` comes from
/// [`crate::media::request_screencast`].
pub struct ScreenCapture {
    pipeline: gst::Pipeline,
    sink: gst_app::AppSink,
    pub width: u32,
    pub height: u32,
}

impl ScreenCapture {
    pub fn new(node_id: u32, width: u32, height: u32) -> Option<Self> {
        // `pipewiresrc` is provided by gst-plugin-pipewire; when it is missing
        // (as in a bare CI image) this simply yields `None`.
        let desc = format!(
            "pipewiresrc path={node_id} ! videoconvert ! videoscale \
             ! video/x-raw,format=RGBA,width={width},height={height} \
             ! appsink name=sink sync=false max-buffers=2 drop=true"
        );
        let (pipeline, sink) = rgba_capture_pipeline(&desc)?;
        Some(Self { pipeline, sink, width, height })
    }

    pub fn poll(&mut self) -> Option<Vec<u8>> {
        let mut latest = None;
        while let Some(sample) = self.sink.try_pull_sample(gst::ClockTime::ZERO) {
            if let Some(buffer) = sample.buffer() {
                if let Ok(map) = buffer.map_readable() {
                    latest = Some(map.as_slice().to_vec());
                }
            }
        }
        latest
    }
}

impl Drop for ScreenCapture {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::{MediaObject, TrackKind};
    use std::time::Duration;

    fn gradient(width: u32, height: u32) -> Vec<u8> {
        let mut out = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                out.push(((x * 255) / width.max(1)) as u8);
                out.push(((y * 255) / height.max(1)) as u8);
                out.push(128);
                out.push(255);
            }
        }
        out
    }

    /// The video codec contract with no camera: an RGBA frame encodes to H.264,
    /// travels as a `MediaObject` over the wire form, and decodes back to a
    /// same-sized RGBA frame — exactly the path a remote call tile runs.
    #[test]
    fn rgba_frame_encodes_transports_and_decodes() {
        const W: u32 = 128;
        const H: u32 = 96;
        let mut enc = H264Encoder::new(W, H, 30).expect("openh264 encoder");
        let nalus = enc.encode(&gradient(W, H));
        assert!(!nalus.is_empty(), "encoder produced a bitstream");

        // Cross the wire form the QUIC transport uses.
        let obj = MediaObject {
            sender: "u_a".into(),
            kind: TrackKind::Video,
            seq: 0,
            pts: Duration::from_millis(0),
            payload: nalus,
        };
        let back = MediaObject::from_wire(&obj.to_wire()).expect("wire round-trip");

        let mut dec = H264FrameDecoder::new();
        let frame = dec.decode(&back.payload).expect("decoder yields a frame");
        assert_eq!((frame.width, frame.height), (W, H));
        assert_eq!(frame.rgba.len(), (W * H * 4) as usize);
    }
}
