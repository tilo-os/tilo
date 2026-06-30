//! Live camera capture via GStreamer's `v4l2src`.
//!
//! This entire module is gated behind the `live-capture` feature so the
//! default build — and the `--shots` headless path — never need the
//! GStreamer dev libraries or a `/dev/video*` device. On a real qOS install
//! the app is built with `--features live-capture` and the compositor grants
//! the cluster access to the camera device.
//!
//! The pipeline is `v4l2src ! videoconvert ! video/x-raw,format=RGBA !
//! appsink`; frames are pulled on a worker thread and handed to the UI as
//! RGBA buffers it can wrap in `ui::widget::Image`.
//!
//! TODO(live): wire the worker's frames into `CameraApp` via
//! `Widget::on_external_event` (the App requests a redraw + re-sync after
//! dispatch), and surface device-permission failures as an in-app prompt.

#![cfg(feature = "live-capture")]

use std::sync::mpsc::{Receiver, Sender};
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

/// A running capture session. Drop to tear the pipeline down.
pub struct Capture {
    pipeline: gst::Pipeline,
    pub frames: Receiver<CamFrame>,
}

impl Capture {
    /// Open `device` (e.g. `/dev/video0`) at the requested size.
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

        let (tx, rx): (Sender<CamFrame>, Receiver<CamFrame>) = std::sync::mpsc::channel();
        let tx = Arc::new(Mutex::new(tx));
        sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let caps = sample.caps().ok_or(gst::FlowError::Error)?;
                    let info = gstreamer_video::VideoInfo::from_caps(caps)
                        .map_err(|_| gst::FlowError::Error)?;
                    let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                    let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                    let frame = CamFrame {
                        rgba: map.as_slice().to_vec(),
                        width: info.width(),
                        height: info.height(),
                    };
                    let _ = tx.lock().unwrap().send(frame);
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );

        pipeline.set_state(gst::State::Playing).context("set playing")?;
        Ok(Self { pipeline, frames: rx })
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}
