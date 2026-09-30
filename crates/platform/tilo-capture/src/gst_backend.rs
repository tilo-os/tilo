//! Native capture through `GStreamer` (everywhere native but macOS):
//! `v4l2src` for cameras, `autoaudiosrc` for microphones, and `pipewiresrc` on
//! the node the `ScreenCast` portal grants.
//! Frames arrive on the streaming thread of `GStreamer` and are handed over through
//! a single latest-frame slot, so a slow UI never queues camera-sized
//! allocations.

#[cfg(gst_microphone)]
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

#[cfg(gst_camera)]
use crate::CameraRequest;
#[cfg(gst_microphone)]
use crate::{AudioSource, MAX_BUFFERED_SAMPLES, MicrophoneRequest};
use crate::{CaptureError, CaptureState, Device, DeviceKind};
#[cfg(any(gst_camera, portal_screen))]
use crate::{Frame, VideoSource, valid_rgba_frame};

fn init() -> Result<(), CaptureError> {
    gst::init().map_err(|e| CaptureError::Failed(format!("GStreamer: {e}")))
}

/// State shared with the streaming thread.
struct Shared<T> {
    latest: Option<T>,
    state: Option<CaptureState>,
}

impl<T> Default for Shared<T> {
    fn default() -> Self {
        Self {
            latest: None,
            state: None,
        }
    }
}

fn classify(message: &str) -> CaptureError {
    let lower = message.to_ascii_lowercase();
    if lower.contains("permission") || lower.contains("not permitted") || lower.contains("denied") {
        CaptureError::PermissionDenied
    } else if lower.contains("no such")
        || lower.contains("not found")
        || lower.contains("cannot identify")
    {
        CaptureError::NotFound
    } else {
        CaptureError::Failed(message.to_owned())
    }
}

/// Watch the bus from a `GStreamer` signal: errors and end-of-stream move the
/// shared state, so `state()` reports them without a bus thread of our own.
fn watch_bus<T: Send + 'static>(pipeline: &gst::Pipeline, shared: &Arc<Mutex<Shared<T>>>) {
    let Some(bus) = pipeline.bus() else {
        return;
    };
    let shared = Arc::clone(shared);
    bus.set_sync_handler(move |_, message| {
        let next = match message.view() {
            gst::MessageView::Error(error) => {
                Some(CaptureState::Failed(classify(&error.error().to_string())))
            }
            gst::MessageView::Eos(_) => Some(CaptureState::Ended),
            _ => None,
        };
        if let Some(next) = next
            && let Ok(mut shared) = shared.lock()
        {
            shared.state = Some(next);
        }
        gst::BusSyncReply::Pass
    });
}

fn start(pipeline: &gst::Pipeline) -> Result<(), CaptureError> {
    pipeline
        .set_state(gst::State::Playing)
        .map(|_| ())
        .map_err(|_| CaptureError::Failed("the capture pipeline did not start".into()))
}

fn build(description: &str) -> Result<gst::Pipeline, CaptureError> {
    gst::parse::launch(description)
        .map_err(|e| classify(&e.to_string()))?
        .downcast::<gst::Pipeline>()
        .map_err(|_| CaptureError::Failed("not a pipeline".into()))
}

fn sink(pipeline: &gst::Pipeline) -> Result<gst_app::AppSink, CaptureError> {
    pipeline
        .by_name("sink")
        .and_then(|e| e.downcast::<gst_app::AppSink>().ok())
        .ok_or_else(|| CaptureError::Failed("the capture pipeline has no sink".into()))
}

pub(crate) fn devices(kind: DeviceKind) -> Vec<Device> {
    if kind == DeviceKind::Camera && cfg!(target_os = "linux") {
        return v4l2_nodes();
    }
    if init().is_err() {
        return Vec::new();
    }
    let monitor = gst::DeviceMonitor::new();
    let class = match kind {
        DeviceKind::Camera => "Video/Source",
        DeviceKind::Microphone => "Audio/Source",
    };
    let _ = monitor.add_filter(Some(class), None);
    if monitor.start().is_err() {
        return Vec::new();
    }
    let found = monitor
        .devices()
        .iter()
        .enumerate()
        .map(|(index, device)| Device {
            id: index.to_string(),
            label: device.display_name().to_string(),
            kind,
        })
        .collect();
    monitor.stop();
    found
}

/// The V4L2 capture nodes, in numeric order (`video2` before `video10`).
fn v4l2_nodes() -> Vec<Device> {
    let Ok(entries) = std::fs::read_dir("/dev") else {
        return Vec::new();
    };
    let mut nodes: Vec<(u32, String)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_owned();
            let number = name.strip_prefix("video")?.parse::<u32>().ok()?;
            Some((number, entry.path().to_string_lossy().into_owned()))
        })
        .collect();
    nodes.sort();
    nodes
        .into_iter()
        .map(|(_, path)| Device {
            label: path.clone(),
            id: path,
            kind: DeviceKind::Camera,
        })
        .collect()
}

#[cfg(any(gst_camera, portal_screen))]
struct Video {
    pipeline: gst::Pipeline,
    shared: Arc<Mutex<Shared<Frame>>>,
}

#[cfg(any(gst_camera, portal_screen))]
impl Video {
    fn open(source: &str, caps: &str) -> Result<Self, CaptureError> {
        init()?;
        let pipeline = build(&format!(
            "{source} ! videoconvert ! videoscale ! video/x-raw,format=RGBA{caps} ! appsink name=sink max-buffers=2 drop=true sync=false"
        ))?;
        let shared: Arc<Mutex<Shared<Frame>>> = Arc::default();
        let slot = Arc::clone(&shared);
        sink(&pipeline)?.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let caps = sample.caps().ok_or(gst::FlowError::Error)?;
                    let info = gstreamer_video::VideoInfo::from_caps(caps)
                        .map_err(|_| gst::FlowError::Error)?;
                    let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                    let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                    let (width, height) = (info.width(), info.height());
                    if !valid_rgba_frame(width, height, map.as_slice().len()) {
                        return Err(gst::FlowError::Error);
                    }
                    let mut shared = slot.lock().map_err(|_| gst::FlowError::Error)?;
                    shared.latest = Some(Frame {
                        width,
                        height,
                        rgba: map.as_slice().to_vec(),
                    });
                    shared.state.get_or_insert(CaptureState::Live);
                    drop(shared);
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
        watch_bus(&pipeline, &shared);
        start(&pipeline)?;
        Ok(Self { pipeline, shared })
    }
}

#[cfg(any(gst_camera, portal_screen))]
impl VideoSource for Video {
    fn state(&mut self) -> CaptureState {
        self.shared
            .lock()
            .ok()
            .and_then(|s| s.state.clone())
            .unwrap_or(CaptureState::Starting)
    }

    fn take_latest(&mut self) -> Option<Frame> {
        self.shared.lock().ok()?.latest.take()
    }
}

#[cfg(any(gst_camera, portal_screen))]
impl Drop for Video {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

/// Only characters a device path or index can contain, so an id can never
/// add pipeline syntax.
#[cfg(gst_camera)]
fn safe_id(id: &str) -> Option<&str> {
    (!id.is_empty()
        && id.len() <= 256
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'-' | b'.')))
    .then_some(id)
}

#[cfg(gst_camera)]
pub(crate) fn camera(request: &CameraRequest) -> Result<Box<dyn VideoSource>, CaptureError> {
    let device = match request.device.as_deref() {
        Some(id) => Some(safe_id(id).ok_or(CaptureError::NotFound)?),
        None => None,
    };
    let source = device.map_or_else(
        || "v4l2src".to_owned(),
        |path| format!("v4l2src device={path}"),
    );
    let caps = format!(",width={},height={}", request.width, request.height);
    Ok(Box::new(Video::open(&source, &caps)?))
}

/// A `pipewiresrc` on the node the ScreenCast portal granted.
#[cfg(portal_screen)]
pub(crate) fn pipewire_node(
    fd: std::os::fd::RawFd,
    node: u32,
) -> Result<Box<dyn VideoSource>, CaptureError> {
    Ok(Box::new(Video::open(
        &format!("pipewiresrc fd={fd} path={node} always-copy=true"),
        "",
    )?))
}

#[cfg(gst_microphone)]
struct Audio {
    pipeline: gst::Pipeline,
    shared: Arc<Mutex<Shared<VecDeque<f32>>>>,
    rate: u32,
}

#[cfg(gst_microphone)]
pub(crate) fn microphone(
    request: &MicrophoneRequest,
) -> Result<Box<dyn AudioSource>, CaptureError> {
    init()?;
    let rate = request.sample_rate.clamp(8_000, 192_000);
    let pipeline = build(&format!(
        "autoaudiosrc ! audioconvert ! audioresample ! audio/x-raw,format=F32LE,channels=1,rate={rate},layout=interleaved ! appsink name=sink max-buffers=8 drop=true sync=false"
    ))?;
    let shared: Arc<Mutex<Shared<VecDeque<f32>>>> = Arc::default();
    let slot = Arc::clone(&shared);
    sink(&pipeline)?.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                let mut shared = slot.lock().map_err(|_| gst::FlowError::Error)?;
                let queue = shared.latest.get_or_insert_with(VecDeque::new);
                queue.extend(
                    map.as_slice()
                        .chunks_exact(4)
                        .filter_map(|b| b.try_into().ok().map(f32::from_le_bytes)),
                );
                let excess = queue.len().saturating_sub(MAX_BUFFERED_SAMPLES);
                queue.drain(..excess);
                shared.state.get_or_insert(CaptureState::Live);
                drop(shared);
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
    watch_bus(&pipeline, &shared);
    start(&pipeline)?;
    Ok(Box::new(Audio {
        pipeline,
        shared,
        rate,
    }))
}

#[cfg(gst_microphone)]
impl AudioSource for Audio {
    fn state(&mut self) -> CaptureState {
        self.shared
            .lock()
            .ok()
            .and_then(|s| s.state.clone())
            .unwrap_or(CaptureState::Starting)
    }

    fn read(&mut self) -> Vec<f32> {
        self.shared
            .lock()
            .ok()
            .and_then(|mut s| s.latest.take())
            .map(Vec::from)
            .unwrap_or_default()
    }

    fn sample_rate(&self) -> u32 {
        self.rate
    }
}

#[cfg(gst_microphone)]
impl Drop for Audio {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}
