//! `tilo-capture`: cameras, microphones, the screen and document scanners,
//! with one API on Linux, macOS and the web.
//!
//! | Capability | Linux | macOS | Web |
//! |---|---|---|---|
//! | camera (`camera`) | `GStreamer` `v4l2src` | `AVFoundation` (`AVCaptureSession`) | `getUserMedia` |
//! | microphone (`microphone`) | `GStreamer` `autoaudiosrc` (`PipeWire`) | `AVFoundation` (`AVCaptureSession`) | `getUserMedia` + an audio worklet |
//! | screen (`screen`) | `ScreenCast` portal + `pipewiresrc` | `AVFoundation` (`AVCaptureScreenInput`) | `getDisplayMedia` |
//! | scanner (`scanner`) | `SANE` | `Unsupported` | `Unsupported` |
//!
//! Every capture is request based and poll driven, because the browser and
//! macOS both ask the user first: `open_*` returns at once in
//! [`CaptureState::Starting`], and the state moves to
//! [`CaptureState::Live`] or [`CaptureState::Failed`]. The UI drains
//! [`VideoCapture::take_latest`] (the newest picture only, never a queue) or
//! [`AudioCapture::read`] once per frame.
//!
//! A native build without a capability's feature answers
//! [`CaptureError::Unsupported`] for it; the web serves every capability
//! whatever the features. An app its sandbox confines (macOS Seatbelt) is
//! served by the host that launched it instead ([`broker`]), whatever the
//! features: the host holds the device permission and the person's consent
//! ([`grants`]).

pub mod scanner;

#[cfg(all(unix, not(web_page)))]
pub mod broker;
#[cfg(all(unix, not(web_page)))]
pub mod grants;

#[cfg(avf)]
mod avf_backend;
#[cfg(gst)]
mod gst_backend;
#[cfg(portal_screen)]
mod portal;
#[cfg(web_page)]
mod web;

/// A capability of this crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feature {
    Camera,
    Microphone,
    Screen,
    Scanner,
}

/// Whether a feature works on this target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Availability {
    Available,
    /// Usable once the user allows it (browser permission, macOS TCC).
    Prompt,
    Unsupported(&'static str),
}

const NO_SCANNER: &str = "document scanners are reached through SANE, on Linux only";

/// Each feature's availability on Linux, macOS and the web, in that order,
/// for a build with every capability feature enabled.
pub const CAPABILITIES: &[(Feature, [Availability; 3])] = &[
    (
        Feature::Camera,
        [
            Availability::Available,
            Availability::Prompt,
            Availability::Prompt,
        ],
    ),
    (
        Feature::Microphone,
        [
            Availability::Available,
            Availability::Prompt,
            Availability::Prompt,
        ],
    ),
    (
        Feature::Screen,
        [
            Availability::Prompt,
            Availability::Prompt,
            Availability::Prompt,
        ],
    ),
    (
        Feature::Scanner,
        [
            Availability::Available,
            Availability::Unsupported(NO_SCANNER),
            Availability::Unsupported(NO_SCANNER),
        ],
    ),
];

fn column() -> usize {
    if cfg!(target_family = "wasm") {
        2
    } else {
        usize::from(cfg!(target_os = "macos"))
    }
}

/// Whether `feature` works on the running target, in this build.
#[must_use]
pub fn availability(feature: Feature) -> Availability {
    let built = match feature {
        Feature::Camera => cfg!(any(native_camera, web_page)),
        Feature::Microphone => cfg!(any(native_microphone, web_page)),
        Feature::Screen => cfg!(any(native_screen, web_page)),
        Feature::Scanner => cfg!(sane_scanner) || column() != 0,
    };
    if !built {
        return Availability::Unsupported("this build leaves the capability out");
    }
    CAPABILITIES
        .iter()
        .find(|(f, _)| *f == feature)
        .and_then(|(_, row)| row.get(column()).copied())
        .unwrap_or(Availability::Unsupported("unknown feature"))
}

/// Why a capture could not start or stopped.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CaptureError {
    #[error("{0}")]
    Unsupported(&'static str),
    /// The user or the system refused access.
    #[error("access to the device was refused")]
    PermissionDenied,
    /// No such device, or none at all.
    #[error("no matching device")]
    NotFound,
    /// The user dismissed the chooser (screen capture).
    #[error("the request was cancelled")]
    Cancelled,
    /// The device exists but could not be started.
    #[error("{0}")]
    Failed(String),
}

/// Where a capture is in its life.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureState {
    /// Waiting for the user's permission or the device.
    Starting,
    Live,
    Failed(CaptureError),
    /// The source went away (the user stopped sharing, the camera was
    /// unplugged).
    Ended,
}

/// What kind of device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceKind {
    Camera,
    Microphone,
}

/// A capture device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    /// What `open_*` takes: a `/dev/video*` node on Linux, an `AVFoundation`
    /// unique id on macOS, a page-scoped device id on the web.
    pub id: String,
    /// For people. Empty on the web until the user has granted access once.
    pub label: String,
    pub kind: DeviceKind,
}

/// The devices of `kind` known now. On the web enumeration is asynchronous:
/// this answers from the last enumeration and starts a fresh one, so ask
/// again on a later frame.
#[must_use]
pub fn devices(kind: DeviceKind) -> Vec<Device> {
    backend::devices(kind)
}

/// One picture: RGBA8, rows tightly packed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// The largest picture a capture hands over (4096 × 2160).
pub const MAX_FRAME_PIXELS: u64 = 8_847_360;

/// Whether `bytes` is exactly a tightly packed RGBA picture of this size,
/// within [`MAX_FRAME_PIXELS`].
#[must_use]
pub fn valid_rgba_frame(width: u32, height: u32, bytes: usize) -> bool {
    let Some(pixels) = u64::from(width).checked_mul(u64::from(height)) else {
        return false;
    };
    width > 0
        && height > 0
        && pixels <= MAX_FRAME_PIXELS
        && pixels.checked_mul(4).and_then(|n| usize::try_from(n).ok()) == Some(bytes)
}

/// What a camera should deliver. The device picks the nearest mode it has.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CameraRequest {
    /// A [`Device::id`]; `None` for the default camera.
    pub device: Option<String>,
    pub width: u32,
    pub height: u32,
}

/// What the user should be offered for screen capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenRequest {
    pub monitors: bool,
    pub windows: bool,
    /// Draw the pointer into the frames.
    pub cursor: bool,
}

impl Default for ScreenRequest {
    fn default() -> Self {
        Self {
            monitors: true,
            windows: true,
            cursor: true,
        }
    }
}

/// A running camera or screen capture. Drop it to stop.
pub struct VideoCapture {
    inner: Box<dyn VideoSource>,
}

/// One backend's video capture.
trait VideoSource {
    fn state(&mut self) -> CaptureState;
    fn take_latest(&mut self) -> Option<Frame>;
}

impl VideoCapture {
    /// Start a camera.
    ///
    /// # Errors
    ///
    /// [`CaptureError::Unsupported`] where this build has no camera backend;
    /// a native pipeline that cannot be built fails here, everything that
    /// needs the user fails later through [`Self::state`].
    pub fn open_camera(request: &CameraRequest) -> Result<Self, CaptureError> {
        backend::open_camera(request).map(|inner| Self { inner })
    }

    /// Ask the user for a monitor or window and capture it.
    ///
    /// # Errors
    ///
    /// As [`Self::open_camera`].
    pub fn open_screen(request: ScreenRequest) -> Result<Self, CaptureError> {
        backend::open_screen(request).map(|inner| Self { inner })
    }

    /// Where the capture is.
    #[must_use]
    pub fn state(&mut self) -> CaptureState {
        self.inner.state()
    }

    /// The newest picture since the last call, if any.
    pub fn take_latest(&mut self) -> Option<Frame> {
        self.inner.take_latest()
    }
}

/// What a microphone should deliver.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MicrophoneRequest {
    /// A [`Device::id`]; `None` for the default microphone.
    pub device: Option<String>,
    /// Requested sample rate in Hz; [`AudioCapture::sample_rate`] says what
    /// was delivered.
    pub sample_rate: u32,
}

impl Default for MicrophoneRequest {
    fn default() -> Self {
        Self {
            device: None,
            sample_rate: 48_000,
        }
    }
}

/// A running microphone capture: mono `f32` samples in `-1.0..=1.0`. Drop
/// it to stop.
pub struct AudioCapture {
    inner: Box<dyn AudioSource>,
}

/// One backend's microphone capture.
trait AudioSource {
    fn state(&mut self) -> CaptureState;
    fn read(&mut self) -> Vec<f32>;
    fn sample_rate(&self) -> u32;
}

/// Samples [`AudioCapture::read`] keeps for a UI that does not drain them:
/// two seconds at 48 kHz. Older samples are dropped.
pub const MAX_BUFFERED_SAMPLES: usize = 96_000;

impl AudioCapture {
    /// Start a microphone.
    ///
    /// # Errors
    ///
    /// As [`VideoCapture::open_camera`].
    pub fn open_microphone(request: &MicrophoneRequest) -> Result<Self, CaptureError> {
        backend::open_microphone(request).map(|inner| Self { inner })
    }

    #[must_use]
    pub fn state(&mut self) -> CaptureState {
        self.inner.state()
    }

    /// The samples captured since the last call.
    pub fn read(&mut self) -> Vec<f32> {
        self.inner.read()
    }

    /// The rate [`Self::read`]'s samples are at, in Hz; 0 until live.
    #[must_use]
    pub fn sample_rate(&self) -> u32 {
        self.inner.sample_rate()
    }
}

/// Picks each capability's backend for this target and build.
mod backend {
    #[cfg(not(web_page))]
    use super::{
        AudioSource, CameraRequest, CaptureError, Device, DeviceKind, MicrophoneRequest,
        ScreenRequest, VideoSource,
    };

    #[cfg(web_page)]
    pub(super) use super::web::{devices, open_camera, open_microphone, open_screen};

    #[cfg(not(web_page))]
    pub(super) fn devices(kind: DeviceKind) -> Vec<Device> {
        #[cfg(gst)]
        return super::gst_backend::devices(kind);
        #[cfg(avf)]
        return super::avf_backend::devices(kind);
        #[cfg(not(any(gst, avf)))]
        {
            let _ = kind;
            Vec::new()
        }
    }

    #[cfg(not(web_page))]
    pub(super) fn open_camera(
        request: &CameraRequest,
    ) -> Result<Box<dyn VideoSource>, CaptureError> {
        #[cfg(unix)]
        if let Some(broker) = super::broker::from_env() {
            return broker.camera(request).map(|capture| capture.inner);
        }
        #[cfg(gst_camera)]
        return super::gst_backend::camera(request);
        #[cfg(avf_camera)]
        return super::avf_backend::camera(request);
        #[cfg(not(native_camera))]
        {
            let _ = request;
            Err(CaptureError::Unsupported(
                "this build has no camera backend",
            ))
        }
    }

    #[cfg(not(web_page))]
    pub(super) fn open_screen(
        request: ScreenRequest,
    ) -> Result<Box<dyn VideoSource>, CaptureError> {
        #[cfg(portal_screen)]
        return Ok(Box::new(super::portal::Screen::request(request)));
        #[cfg(avf_screen)]
        return super::avf_backend::screen(request);
        #[cfg(not(any(portal_screen, avf_screen)))]
        {
            let _ = request;
            Err(CaptureError::Unsupported(
                "this build has no screen-capture backend",
            ))
        }
    }

    #[cfg(not(web_page))]
    pub(super) fn open_microphone(
        request: &MicrophoneRequest,
    ) -> Result<Box<dyn AudioSource>, CaptureError> {
        #[cfg(unix)]
        if let Some(broker) = super::broker::from_env() {
            return broker.microphone(request).map(|capture| capture.inner);
        }
        #[cfg(gst_microphone)]
        return super::gst_backend::microphone(request);
        #[cfg(avf_microphone)]
        return super::avf_backend::microphone(request);
        #[cfg(not(native_microphone))]
        {
            let _ = request;
            Err(CaptureError::Unsupported(
                "this build has no microphone backend",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_admission_refuses_overflow_mismatch_and_oversize() {
        assert!(valid_rgba_frame(1280, 720, 1280 * 720 * 4));
        assert!(!valid_rgba_frame(1280, 720, 1));
        assert!(!valid_rgba_frame(0, 720, 0));
        assert!(!valid_rgba_frame(4097, 2160, 4097 * 2160 * 4));
        assert!(!valid_rgba_frame(u32::MAX, u32::MAX, 0));
    }

    #[test]
    fn availability_follows_the_table_where_built() {
        for (feature, row) in CAPABILITIES {
            let got = availability(*feature);
            assert!(
                got == row[column()] || matches!(got, Availability::Unsupported(_)),
                "{feature:?}: {got:?}"
            );
        }
    }

    #[test]
    fn a_build_without_a_backend_says_so_instead_of_failing_later() {
        if !cfg!(native_camera) {
            assert!(matches!(
                VideoCapture::open_camera(&CameraRequest {
                    device: None,
                    width: 640,
                    height: 480,
                }),
                Err(CaptureError::Unsupported(_))
            ));
        }
        if !cfg!(native_microphone) {
            assert!(matches!(
                AudioCapture::open_microphone(&MicrophoneRequest::default()),
                Err(CaptureError::Unsupported(_))
            ));
        }
    }
}
