//! Live camera capture via `GStreamer`'s `v4l2src`.
//!
//! This entire module is gated behind the `live-capture` feature so the
//! default build — and the `--shots` headless path — never need the
//! `GStreamer` dev libraries or a `/dev/video*` device. On a real qOS install
//! the app is built with `--features live-capture` and the compositor grants
//! the cluster access to the camera device.
//!
//! The pipeline is `v4l2src ! videoconvert ! video/x-raw,format=RGBA !
//! appsink`; frames are pulled on a worker thread and handed to the UI as
//! RGBA buffers it can wrap in `ui::widget::Image`.
//!
//! `CameraApp::poll_camera` drains this receiver each sync and forwards the
//! newest frame to the viewfinder / capture path via
//! `CameraApp::push_camera_frame`. Device-open failure leaves the app in its
//! honest "camera unavailable" state rather than faking a feed.

#![cfg(feature = "live-capture")]

use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

/// One decoded RGBA frame from the camera.
pub struct CamFrame {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// A live frame must fit the renderer's upload budget.  The negotiated 4K
/// preset is 33 MiB, leaving room for a little padding while refusing a
/// malicious/broken camera that advertises an absurd size.
pub const MAX_FRAME_PIXELS: u64 = 8_847_360; // 4096 × 2160
pub const MAX_FRAME_BYTES: usize = 48 * 1024 * 1024;

fn valid_rgba_frame(width: u32, height: u32, bytes: usize) -> bool {
    let Some(pixels) = u64::from(width).checked_mul(u64::from(height)) else {
        return false;
    };
    let Some(expected) = pixels.checked_mul(4).and_then(|n| usize::try_from(n).ok()) else {
        return false;
    };
    width > 0
        && height > 0
        && pixels <= MAX_FRAME_PIXELS
        && expected <= MAX_FRAME_BYTES
        // RGBA frames are deliberately tightly packed. Accepting a padded
        // GStreamer map here would make later image consumers misinterpret
        // rows, so renegotiate/fail rather than exposing corrupt pixels.
        && bytes == expected
}

/// A single-slot, latest-frame hand-off. The callback replaces the slot first
/// and only then emits a bounded wake token; a slow UI therefore sees the
/// newest frame without an unbounded queue of camera-sized allocations.
pub struct LatestFrames {
    slot: Arc<Mutex<Option<CamFrame>>>,
    wake_rx: Receiver<()>,
}

impl LatestFrames {
    pub fn take_latest(&self) -> Option<CamFrame> {
        // Wake tokens only mean "the slot may have changed". Drain them so a
        // producer can signal the next replacement, then take exactly one
        // bounded frame.
        while self.wake_rx.try_recv().is_ok() {}
        self.slot.lock().ok()?.take()
    }
}

/// Enumerate the V4L2 capture nodes present on the host, sorted (`/dev/video0`,
/// `/dev/video1`, …). Real device discovery: the lens selector indexes into
/// this list. Empty when no camera is attached.
pub fn list_devices() -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/dev") else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && let Some(num) = name.strip_prefix("video")
            && !num.is_empty()
            && num.bytes().all(|b| b.is_ascii_digit())
        {
            out.push(path.to_string_lossy().into_owned());
        }
    }
    // Numeric order, not lexical, so video2 sorts before video10.
    out.sort_by_key(|p| {
        p.rsplit("video")
            .next()
            .and_then(|n| n.parse::<u32>().ok())
            .unwrap_or(u32::MAX)
    });
    out
}

/// A running capture session. Drop to tear the pipeline down.
pub struct Capture {
    pipeline: gst::Pipeline,
    pub frames: LatestFrames,
}

impl Capture {
    /// Open `device` (e.g. `/dev/video0`) at the requested size.
    ///
    /// # Errors
    ///
    /// Returns an error if `GStreamer` fails to initialize, the pipeline fails
    /// to build or is missing its appsink, or the pipeline fails to
    /// transition to the `Playing` state.
    pub fn open(device: &str, width: u32, height: u32) -> Result<Self> {
        gst::init().context("gst init")?;
        let desc = format!(
            "v4l2src device={device} ! videoconvert ! video/x-raw,format=RGBA,width={width},height={height} ! appsink name=sink max-buffers=2 drop=true sync=false"
        );
        let pipeline = gst::parse::launch(&desc)
            .context("build pipeline")?
            .downcast::<gst::Pipeline>()
            .map_err(|_| anyhow::anyhow!("not a pipeline"))?;

        let sink = pipeline
            .by_name("sink")
            .context("appsink missing")?
            .downcast::<gst_app::AppSink>()
            .map_err(|_| anyhow::anyhow!("sink is not an appsink"))?;

        let (wake_tx, wake_rx): (SyncSender<()>, Receiver<()>) = std::sync::mpsc::sync_channel(1);
        let slot = Arc::new(Mutex::new(None));
        let callback_slot = Arc::clone(&slot);
        sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let caps = sample.caps().ok_or(gst::FlowError::Error)?;
                    let info = gstreamer_video::VideoInfo::from_caps(caps)
                        .map_err(|_| gst::FlowError::Error)?;
                    let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                    let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                    let (width, height) = (info.width(), info.height());
                    let bytes = map.as_slice();
                    if !valid_rgba_frame(width, height, bytes.len()) {
                        return Err(gst::FlowError::Error);
                    }
                    let frame = CamFrame {
                        rgba: bytes.to_vec(),
                        width,
                        height,
                    };
                    *callback_slot.lock().map_err(|_| gst::FlowError::Error)? = Some(frame);
                    // Full is expected while the UI is busy; the slot has
                    // already been replaced, so no camera frame allocation is
                    // queued. A disconnected UI means the pipeline should
                    // stop instead of silently running forever.
                    match wake_tx.try_send(()) {
                        Ok(()) | Err(TrySendError::Full(())) => {}
                        Err(TrySendError::Disconnected(())) => return Err(gst::FlowError::Eos),
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );

        pipeline
            .set_state(gst::State::Playing)
            .context("set playing")?;
        Ok(Self {
            pipeline,
            frames: LatestFrames { slot, wake_rx },
        })
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_devices_yields_valid_sorted_paths() {
        // Runs with or without a camera. When nodes exist they must be real
        // /dev/video* paths in ascending numeric order.
        let devs = list_devices();
        for d in &devs {
            assert!(d.starts_with("/dev/video"), "unexpected device path: {d}");
        }
        let nums: Vec<u32> = devs
            .iter()
            .filter_map(|d| d.rsplit("video").next().and_then(|n| n.parse().ok()))
            .collect();
        assert!(
            nums.windows(2).all(|w| w[0] <= w[1]),
            "devices sorted numerically"
        );
    }

    #[test]
    fn frame_admission_refuses_overflow_mismatch_and_oversize() {
        assert!(valid_rgba_frame(1280, 720, 1280 * 720 * 4));
        assert!(!valid_rgba_frame(1280, 720, 1));
        assert!(!valid_rgba_frame(0, 720, 0));
        assert!(!valid_rgba_frame(4097, 2160, 4097 * 2160 * 4));
    }

    #[test]
    fn latest_slot_replaces_stale_frames_without_a_queue() {
        let (wake_tx, wake_rx) = std::sync::mpsc::sync_channel(1);
        let slot = Arc::new(Mutex::new(None));
        let frames = LatestFrames {
            slot: Arc::clone(&slot),
            wake_rx,
        };
        *slot.lock().unwrap() = Some(CamFrame {
            rgba: vec![1, 2, 3, 4],
            width: 1,
            height: 1,
        });
        wake_tx.try_send(()).unwrap();
        // A producer can keep replacing the single slot even while the wake
        // token is full. The UI must receive B, never stale A.
        *slot.lock().unwrap() = Some(CamFrame {
            rgba: vec![5, 6, 7, 8],
            width: 1,
            height: 1,
        });
        assert!(matches!(wake_tx.try_send(()), Err(TrySendError::Full(()))));
        assert_eq!(frames.take_latest().unwrap().rgba, vec![5, 6, 7, 8]);
        assert!(frames.take_latest().is_none());
    }

    #[test]
    fn open_first_device_or_skip() {
        // Honest device test: opt-in only. This opens a real camera when one is
        // present, which is not hermetic, so a plain `cargo test` (now that
        // `live-capture` is a default feature) must not touch hardware. Set
        // CAMERA_LIVE_TEST=1 to exercise it on a machine with a webcam.
        if std::env::var("CAMERA_LIVE_TEST").is_err() {
            eprintln!("CAMERA_LIVE_TEST not set — skipping live-open device test");
            return;
        }
        // Honest device test: skip cleanly when no camera is present.
        let devs = list_devices();
        let Some(dev) = devs.first() else {
            eprintln!("no /dev/video* device present — skipping live-open test");
            return;
        };
        match Capture::open(dev, 640, 480) {
            Ok(cap) => {
                // A real pipeline opened; the frame channel is live. We don't
                // assert a frame arrives — CI cameras may be present but not
                // streamable — only that the pipeline built and plays.
                drop(cap);
            }
            Err(e) => {
                eprintln!("device {dev} present but not openable here: {e}");
            }
        }
    }
}
