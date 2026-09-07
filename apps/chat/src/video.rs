//! Call video capture — camera + portal `ScreenCast`, yielding RGBA frames.
//!
//! Capture is **`GStreamer`**: `autovideosrc` for the webcam and `pipewiresrc`
//! for a portal `ScreenCast` node — both yield tightly-packed RGBA frames. The
//! frames feed the **shared qOS video-streaming stack** ([`qos_callstream`]):
//! encode, per-share framing/transport, and decode all live there, so the
//! call app reuses the exact same H.264 codec + wire format the compositor's
//! collab surface streaming uses instead of a second in-app codec. This
//! module is compiled only under `live-media`.
//!
//! Capture needs a device (camera) or a live portal (screen), so those
//! pipelines return `None` when absent and the call falls back to the honest
//! "camera off" / "connecting" placeholder. The codec itself is device-free
//! and is exercised by a real encode→transport→decode round-trip test against
//! [`qos_callstream`].

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

/// Default capture geometry for a call (VGA is plenty for a face tile and keeps
/// the software encoder real-time).
pub const DEFAULT_W: u32 = 640;
pub const DEFAULT_H: u32 = 480;
pub const DEFAULT_FPS: u32 = 30;

fn ensure_init() -> bool {
    gst::init().is_ok()
}

fn rgba_capture_pipeline(desc: &str) -> Option<(gst::Pipeline, gst_app::AppSink)> {
    if !ensure_init() {
        return None;
    }
    let pipeline = gst::parse::launch(desc)
        .ok()?
        .downcast::<gst::Pipeline>()
        .ok()?;
    let sink = pipeline
        .by_name("sink")?
        .downcast::<gst_app::AppSink>()
        .ok()?;
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
        Some(Self {
            pipeline,
            sink,
            width,
            height,
        })
    }

    /// Non-blocking: the most recent captured RGBA frame, if any (older queued
    /// frames are coalesced by `drop=true`).
    pub fn poll(&mut self) -> Option<Vec<u8>> {
        let mut latest = None;
        while let Some(sample) = self.sink.try_pull_sample(gst::ClockTime::ZERO) {
            if let Some(buffer) = sample.buffer()
                && let Ok(map) = buffer.map_readable()
            {
                latest = Some(map.as_slice().to_vec());
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

/// Portal `ScreenCast` `PipeWire` node → RGBA frames. `None` when the `pipewiresrc`
/// element is unavailable or the node can't be opened. `node_id` comes from
/// [`crate::media::request_screencast`]. The captured RGBA is handed to a
/// [`qos_callstream::VideoStreamSender`] for encode + framing — the source
/// selection stays the xdg `ScreenCast` portal, the encode + transport reuse the
/// shared stack.
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
        Some(Self {
            pipeline,
            sink,
            width,
            height,
        })
    }

    pub fn poll(&mut self) -> Option<Vec<u8>> {
        let mut latest = None;
        while let Some(sample) = self.sink.try_pull_sample(gst::ClockTime::ZERO) {
            if let Some(buffer) = sample.buffer()
                && let Ok(map) = buffer.map_readable()
            {
                latest = Some(map.as_slice().to_vec());
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
    use crate::media::{MediaObject, TrackKind};
    use qos_callstream::{VideoStreamReceiver, VideoStreamSender};
    use std::time::{Duration, Instant};

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

    /// The screen-share codec contract, now on the SHARED stack: a captured
    /// RGBA frame runs the compositor's video-streaming stack —
    /// [`VideoStreamSender`] (throttle + H.264 encode → framed `SurfaceFrame`
    /// bytes) — travels as a `MediaObject` over the call transport's wire
    /// form, and decodes back through [`VideoStreamReceiver`] (reframe +
    /// decode) to a same-sized RGBA frame. No in-app second codec is
    /// involved — this is the exact path a remote screen-share tile runs.
    #[test]
    fn rgba_frame_streams_through_shared_stack_over_the_wire() {
        const W: u32 = 128;
        const H: u32 = 96;
        let mut sender = VideoStreamSender::new(W, H, 30).expect("shared-stack sender");
        let wire = sender
            .offer_frame(&gradient(W, H), Instant::now())
            .expect("shared sender accepts the frame")
            .expect("throttle admits first frame + encoder emits a NALU");

        // Cross the wire form the call transport uses.
        let obj = MediaObject {
            sender: "u_a".into(),
            kind: TrackKind::Screen,
            seq: 0,
            pts: Duration::from_millis(0),
            payload: wire,
        };
        let back = MediaObject::from_wire(&obj.to_wire()).expect("wire round-trip");

        let mut receiver = VideoStreamReceiver::new();
        let frames = receiver.push(&back.payload);
        assert_eq!(frames.len(), 1, "one screen frame decoded off the wire");
        assert_eq!((frames[0].width, frames[0].height), (W, H));
        assert_eq!(frames[0].rgba.len(), (W * H * 4) as usize);
    }
}
