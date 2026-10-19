//! Live video capture for calls: the device's camera, or a moving test
//! picture, delivered as tightly packed NV12 frames of one fixed size and
//! rate, ready for [`super::RealtimeEncoder`].
//!
//! The camera is read through the OS media stack (`v4l2src`, converted and
//! scaled in `GStreamer`). The test picture is `videotestsrc`'s colour bars,
//! scrolling, through the same conversion: tests and screenshots see real
//! frames that went through a real pipeline, never a painted stand-in.
//!
//! Without `--features realtime` [`VideoCapture::start`] says
//! [`RealtimeError::NotCompiled`].

use std::time::Instant;

use super::RealtimeError;

/// What to capture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureDevice {
    /// A camera's V4L2 capture node (see [`camera_node`]).
    Camera { node: std::path::PathBuf },
    /// Colour bars scrolling sideways (`videotestsrc`): a moving picture
    /// with known colours, for tests and screenshots.
    TestPattern,
}

/// How a [`VideoCapture`] is set up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureConfig {
    pub device: CaptureDevice,
    /// Frame size; both even.
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

/// One captured frame: NV12, tightly packed (`width × height` luma, then
/// half as much interleaved chroma).
#[derive(Clone, Debug)]
pub struct CapturedFrame {
    pub nv12: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub captured_at: Instant,
}

/// Receives each captured frame on the capture's own thread. It must not
/// block: hand the frame over and return.
pub type FrameSink = Box<dyn Fn(CapturedFrame) + Send + Sync>;

impl CaptureConfig {
    /// # Errors
    ///
    /// [`RealtimeError::InvalidConfig`] for a zero, odd or oversized frame,
    /// or a rate of zero.
    pub fn validate(&self) -> Result<(), RealtimeError> {
        if self.width == 0
            || self.height == 0
            || !self.width.is_multiple_of(2)
            || !self.height.is_multiple_of(2)
            || self.width > super::MAX_DIMENSION
            || self.height > super::MAX_DIMENSION
            || self.fps == 0
            || self.fps > 120
        {
            return Err(RealtimeError::InvalidConfig(format!(
                "capture {}x{} at {} frames a second",
                self.width, self.height, self.fps
            )));
        }
        Ok(())
    }
}

/// The device's camera: the first V4L2 node that captures video and is not
/// a camera this OS projects from another device ("Tilo Remote Camera").
#[must_use]
pub fn camera_node() -> Option<std::path::PathBuf> {
    let mut nodes: Vec<_> = std::fs::read_dir("/sys/class/video4linux")
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| {
            let path = entry.path();
            let name = std::fs::read_to_string(path.join("name")).unwrap_or_default();
            // A camera's first node captures; later ones carry metadata.
            let index = std::fs::read_to_string(path.join("index")).unwrap_or_default();
            !name.trim().starts_with("Tilo Remote Camera") && index.trim() == "0"
        })
        .map(|entry| entry.file_name())
        .collect();
    nodes.sort();
    nodes
        .into_iter()
        .next()
        .map(|name| std::path::Path::new("/dev").join(name))
}

#[cfg(gst_realtime)]
mod imp {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;

    use gstreamer as gst;
    use gstreamer::prelude::*;
    use gstreamer_app as gst_app;
    use gstreamer_video as gst_video;

    use super::{CaptureConfig, CaptureDevice, CapturedFrame, FrameSink, RealtimeError};

    /// A running capture; see the [module docs](super). Dropping it stops
    /// the capture.
    pub struct VideoCapture {
        pipeline: gst::Pipeline,
        failed: Arc<AtomicBool>,
    }

    impl std::fmt::Debug for VideoCapture {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("VideoCapture")
                .field("failed", &self.failed())
                .finish_non_exhaustive()
        }
    }

    fn make(name: &str) -> Result<gst::Element, RealtimeError> {
        gst::ElementFactory::make(name)
            .build()
            .map_err(|error| RealtimeError::Unavailable {
                codec: crate::hw::realtime::StreamCodec::H264,
                tried: vec![format!("{name}: {error}")],
            })
    }

    impl VideoCapture {
        /// Start capturing; every frame goes to `sink` on the capture's
        /// thread.
        ///
        /// # Errors
        ///
        /// An invalid configuration, `GStreamer` missing a needed element, or
        /// a pipeline that would not start. A camera that fails later (it is
        /// unplugged, another program holds it) reports [`Self::failed`].
        pub fn start(config: &CaptureConfig, sink: FrameSink) -> Result<Self, RealtimeError> {
            config.validate()?;
            gst::init().map_err(|error| RealtimeError::Pipeline(error.to_string()))?;
            let pipeline = gst::Pipeline::new();
            let source = match &config.device {
                CaptureDevice::Camera { node } => {
                    let source = make("v4l2src")?;
                    source.set_property("device", node.to_string_lossy().as_ref());
                    source
                }
                CaptureDevice::TestPattern => {
                    let source = make("videotestsrc")?;
                    source.set_property("is-live", true);
                    source.set_property_from_str("pattern", "smpte75");
                    source.set_property("horizontal-speed", 4_i32);
                    source
                }
            };
            let convert = make("videoconvert")?;
            let scale = make("videoscale")?;
            let rate = make("videorate")?;
            let caps = gst::Caps::builder("video/x-raw")
                .field("format", "NV12")
                .field("width", i32::try_from(config.width).unwrap_or(i32::MAX))
                .field("height", i32::try_from(config.height).unwrap_or(i32::MAX))
                .field(
                    "framerate",
                    gst::Fraction::new(i32::try_from(config.fps).unwrap_or(30), 1),
                )
                .build();
            let filter = gst::ElementFactory::make("capsfilter")
                .property("caps", &caps)
                .build()
                .map_err(|error| RealtimeError::Pipeline(error.to_string()))?;
            // The newest frame only: a slow consumer skips frames, never
            // falls behind.
            let appsink = gst_app::AppSink::builder()
                .sync(false)
                .max_buffers(1)
                .drop(true)
                .enable_last_sample(false)
                .build();
            let sink_element = appsink.clone().upcast::<gst::Element>();
            let chain = [&source, &convert, &scale, &rate, &filter, &sink_element];
            pipeline
                .add_many(chain)
                .map_err(|error| RealtimeError::Pipeline(error.to_string()))?;
            gst::Element::link_many(chain)
                .map_err(|error| RealtimeError::Pipeline(error.to_string()))?;
            let failed = Arc::new(AtomicBool::new(false));
            let failing = failed.clone();
            appsink.set_callbacks(
                gst_app::AppSinkCallbacks::builder()
                    .new_sample(move |appsink| {
                        let sample = appsink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                        if let Some(frame) = tight_nv12(&sample) {
                            sink(frame);
                        }
                        Ok(gst::FlowSuccess::Ok)
                    })
                    .build(),
            );
            if let Some(bus) = pipeline.bus() {
                bus.set_sync_handler(move |_, message| {
                    if matches!(
                        message.view(),
                        gst::MessageView::Error(_) | gst::MessageView::Eos(_)
                    ) {
                        failing.store(true, Ordering::Release);
                    }
                    gst::BusSyncReply::Drop
                });
            }
            pipeline
                .set_state(gst::State::Playing)
                .map_err(|error| RealtimeError::Pipeline(format!("could not start: {error}")))?;
            Ok(Self { pipeline, failed })
        }

        /// The capture stopped on its own: the camera went away or failed.
        #[must_use]
        pub fn failed(&self) -> bool {
            self.failed.load(Ordering::Acquire)
        }
    }

    impl Drop for VideoCapture {
        /// Stop on a thread of its own: a camera driver can take a while to
        /// let go, and the caller must not wait for it.
        fn drop(&mut self) {
            let pipeline = self.pipeline.clone();
            let spawned = std::thread::Builder::new()
                .name("media-capture-teardown".into())
                .spawn(move || {
                    let _ = pipeline.set_state(gst::State::Null);
                });
            if spawned.is_err() {
                let _ = self.pipeline.set_state(gst::State::Null);
            }
        }
    }

    /// `sample`'s NV12 picture with its rows tightly packed.
    fn tight_nv12(sample: &gst::Sample) -> Option<CapturedFrame> {
        let captured_at = Instant::now();
        let info = gst_video::VideoInfo::from_caps(sample.caps()?).ok()?;
        if info.format() != gst_video::VideoFormat::Nv12 {
            return None;
        }
        let frame =
            gst_video::VideoFrameRef::from_buffer_ref_readable(sample.buffer()?, &info).ok()?;
        let width = usize::try_from(info.width()).ok()?;
        let height = usize::try_from(info.height()).ok()?;
        let mut nv12 = Vec::with_capacity(width.checked_mul(height)?.checked_mul(3)? / 2);
        for (plane, rows) in [(0_u32, height), (1, height / 2)] {
            let data = frame.plane_data(plane).ok()?;
            let stride = usize::try_from(*info.stride().get(usize::try_from(plane).ok()?)?).ok()?;
            for row in 0..rows {
                let start = row.checked_mul(stride)?;
                nv12.extend_from_slice(data.get(start..start.checked_add(width)?)?);
            }
        }
        Some(CapturedFrame {
            nv12,
            width: info.width(),
            height: info.height(),
            captured_at,
        })
    }
}

#[cfg(not(gst_realtime))]
mod imp {
    use super::{CaptureConfig, FrameSink, RealtimeError};

    /// No value of this type exists without `--features realtime`.
    #[derive(Debug)]
    enum Never {}

    /// Placeholder for builds without `--features realtime`.
    #[derive(Debug)]
    pub struct VideoCapture {
        never: Never,
    }

    impl VideoCapture {
        /// # Errors
        ///
        /// Always [`RealtimeError::NotCompiled`].
        pub fn start(_config: &CaptureConfig, _sink: FrameSink) -> Result<Self, RealtimeError> {
            Err(RealtimeError::NotCompiled)
        }

        #[must_use]
        pub fn failed(&self) -> bool {
            match self.never {}
        }
    }
}

pub use imp::VideoCapture;
