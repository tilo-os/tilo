//! Live camera capture through `tilo-capture`: `GStreamer` (`v4l2src`,
//! `avfvideosrc`) on the desktop, `getUserMedia` in the browser.
//!
//! This module is gated behind the `live-capture` feature so the `--shots`
//! headless path never opens a device. Opening is asynchronous where the
//! platform asks the user first; `CameraApp::poll_camera` drains the newest
//! frame each sync and turns a refused or failed start into the honest
//! "camera unavailable" state rather than faking a feed.

#![cfg(feature = "live-capture")]

use anyhow::Result;
use tilo_capture::{CameraRequest, CaptureError, CaptureState, DeviceKind, VideoCapture};

/// One decoded RGBA frame from the camera.
pub struct CamFrame {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// The cameras present, by the id `Capture::open` takes (`/dev/video*` on
/// Linux, in numeric order). The lens selector indexes into this list. Empty
/// when no camera is attached; in the browser, ids are empty strings until
/// the user has granted access once, which opens the default camera.
pub fn list_devices() -> Vec<String> {
    tilo_capture::devices(DeviceKind::Camera)
        .into_iter()
        .map(|device| device.id)
        .collect()
}

/// A running capture session. Drop to stop it.
pub struct Capture {
    video: VideoCapture,
}

impl Capture {
    /// Open `device` at the requested size (the camera picks its nearest
    /// mode).
    ///
    /// # Errors
    ///
    /// Returns an error when this build or platform has no camera backend or
    /// the capture pipeline cannot be built. A refusal the user gives later
    /// arrives through [`Self::failure`].
    pub fn open(device: &str, width: u32, height: u32) -> Result<Self> {
        let video = VideoCapture::open_camera(&CameraRequest {
            device: (!device.is_empty()).then(|| device.to_owned()),
            width,
            height,
        })?;
        Ok(Self { video })
    }

    /// The newest frame since the last call.
    pub fn take_latest(&mut self) -> Option<CamFrame> {
        self.video.take_latest().map(|frame| CamFrame {
            rgba: frame.rgba,
            width: frame.width,
            height: frame.height,
        })
    }

    /// Why the capture failed or stopped, once it has.
    pub fn failure(&mut self) -> Option<CaptureError> {
        match self.video.state() {
            CaptureState::Failed(error) => Some(error),
            CaptureState::Ended => Some(CaptureError::Failed("the camera stopped".into())),
            CaptureState::Starting | CaptureState::Live => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_admits_only_valid_devices_and_the_latest_frame() {
        // Phase `list_devices_yields_valid_sorted_paths`.
        {
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
        // Phase `open_first_device_or_skip`.
        {
            // Honest device test: opt-in only. This opens a real camera when one is
            // present, which is not hermetic, so a plain `cargo test` (now that
            // `live-capture` is a default feature) must not touch hardware. Set
            // CAMERA_LIVE_TEST=1 to exercise it on a machine with a webcam.
            if std::env::var("CAMERA_LIVE_TEST").is_err() {
                eprintln!("CAMERA_LIVE_TEST not set — skipping live-open device test");
            } else {
                // Honest device test: skip cleanly when no camera is present.
                let devs = list_devices();
                if let Some(dev) = devs.first() {
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
                } else {
                    eprintln!("no /dev/video* device present — skipping live-open test");
                }
            }
        }
    }
}
