//! `tilo-capture`: cameras, microphones, the screen and document scanners, with
//! one API on Linux, macOS and the web.
//!
//! | Capability | Linux | macOS | Web |
//! |---|---|---|---|
//! | camera (`camera`) | `GStreamer` `v4l2src` | `AVFoundation` | `getUserMedia` |
//! | microphone (`microphone`) | `GStreamer` `autoaudiosrc` | `AVFoundation` | `getUserMedia` + an audio worklet |
//! | screen (`screen`) | `ScreenCast` portal + `pipewiresrc` | `AVFoundation` | `getDisplayMedia` |
//! | scanner (`scanner`) | `SANE` | `Unsupported` | `Unsupported` |
//!
//! Every capture is request based and poll driven, because the browser and macOS
//! ask the user first: `open_*` returns at once in [`CaptureState::Starting`], and
//! the state moves to [`CaptureState::Live`] or [`CaptureState::Failed`]. The UI
//! drains [`VideoCapture::take_latest`] (the newest picture only, never a queue) or
//! [`AudioCapture::read`] once per frame, and [`VideoCapture::notify`] wakes it when
//! the state moves.
//!
//! A native build without a capability's feature answers [`CaptureError::Unsupported`]
//! for it; the web serves every capability. An app its sandbox confines is served by
//! the host that launched it ([`broker`], [`grants`]).

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feature {
    Camera,
    Microphone,
    Screen,
    Scanner,
}

pub use tilo_platform_caps::{Availability, availability, available};

const NO_SCANNER: &str = "document scanners are reached through SANE, on Linux only";

impl tilo_platform_caps::Feature for Feature {
    /// The feature's availability on Linux, macOS and the web, in that order, for a
    /// build with every capability feature enabled.
    fn targets(self) -> [Availability; 3] {
        use Availability::{Available, Prompt, Unsupported};
        match self {
            Self::Camera | Self::Microphone => [Available, Prompt, Prompt],
            Self::Screen => [Prompt, Prompt, Prompt],
            Self::Scanner => [Available, Unsupported(NO_SCANNER), Unsupported(NO_SCANNER)],
        }
    }

    fn availability(self) -> Availability {
        let built = match self {
            Self::Camera => cfg!(any(native_camera, web_page)),
            Self::Microphone => cfg!(any(native_microphone, web_page)),
            Self::Screen => cfg!(any(native_screen, web_page)),
            Self::Scanner => cfg!(sane_scanner) || tilo_platform_caps::COLUMN != 0,
        };
        if built {
            self.targets()[tilo_platform_caps::COLUMN]
        } else {
            Availability::Unsupported("this build leaves the capability out")
        }
    }
}

/// Who answers a capture permission prompt where this runs, and so where a person
/// undoes a refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessGrantor {
    Browser,
    System,
}

pub const ACCESS_GRANTOR: AccessGrantor = if cfg!(target_family = "wasm") {
    AccessGrantor::Browser
} else {
    AccessGrantor::System
};

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CaptureError {
    #[error("{0}")]
    Unsupported(&'static str),
    #[error("access to the device was refused")]
    PermissionDenied,
    #[error("no matching device")]
    NotFound,
    #[error("the request was cancelled")]
    Cancelled,
    #[error("{0}")]
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureState {
    Starting,
    Live,
    Failed(CaptureError),
    Ended,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceKind {
    Camera,
    Microphone,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    /// What `open_*` takes: a `/dev/video*` node on Linux, an `AVFoundation` unique id
    /// on macOS, a page-scoped device id on the web.
    pub id: String,
    /// For people. Empty on the web until the user has granted access once.
    pub label: String,
    pub kind: DeviceKind,
}

/// The devices of `kind` known now. On the web enumeration is asynchronous: this
/// answers from the last one and starts a fresh one, so ask again on a later
/// frame.
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

/// Whether `bytes` is exactly a tightly packed RGBA picture of this size, within
/// [`MAX_FRAME_PIXELS`].
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CameraRequest {
    pub device: Option<String>,
    pub width: u32,
    pub height: u32,
}

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

pub struct VideoCapture {
    inner: Box<dyn VideoSource>,
}

pub type Wake = std::sync::Arc<dyn Fn() + Send + Sync>;

trait VideoSource {
    fn state(&mut self) -> CaptureState;
    fn take_latest(&mut self) -> Option<Frame>;
    fn notify(&mut self, wake: Wake) -> bool {
        let _ = wake;
        false
    }
}

impl VideoCapture {
    /// Start a camera.
    ///
    /// # Errors
    ///
    /// [`CaptureError::Unsupported`] where this build has no camera backend; a native
    /// pipeline that cannot be built fails here, and anything needing the user fails
    /// later through [`Self::state`].
    pub fn open_camera(request: &CameraRequest) -> Result<Self, CaptureError> {
        backend::open_camera(request).map(|inner| Self { inner })
    }

    pub fn open_screen(request: ScreenRequest) -> Result<Self, CaptureError> {
        backend::open_screen(request).map(|inner| Self { inner })
    }

    #[must_use]
    pub fn state(&mut self) -> CaptureState {
        self.inner.state()
    }

    pub fn take_latest(&mut self) -> Option<Frame> {
        self.inner.take_latest()
    }

    pub fn notify(&mut self, wake: impl Fn() + Send + Sync + 'static) -> bool {
        self.inner.notify(std::sync::Arc::new(wake))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MicrophoneRequest {
    pub device: Option<String>,
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

/// A running microphone capture: mono `f32` samples in `-1.0..=1.0`. Drop it to
/// stop.
pub struct AudioCapture {
    inner: Box<dyn AudioSource>,
}

trait AudioSource {
    fn state(&mut self) -> CaptureState;
    fn read(&mut self) -> Vec<f32>;
    fn sample_rate(&self) -> u32;
}

/// Samples [`AudioCapture::read`] keeps for a UI that does not drain them: two
/// seconds at 48 kHz, older ones dropped.
pub const MAX_BUFFERED_SAMPLES: usize = 96_000;

impl AudioCapture {
    pub fn open_microphone(request: &MicrophoneRequest) -> Result<Self, CaptureError> {
        backend::open_microphone(request).map(|inner| Self { inner })
    }

    #[must_use]
    pub fn state(&mut self) -> CaptureState {
        self.inner.state()
    }

    pub fn read(&mut self) -> Vec<f32> {
        self.inner.read()
    }

    /// The rate [`Self::read`]'s samples are at, in Hz; 0 until live.
    #[must_use]
    pub fn sample_rate(&self) -> u32 {
        self.inner.sample_rate()
    }
}

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
    use tilo_platform_caps::Feature as _;

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
        for feature in [
            Feature::Camera,
            Feature::Microphone,
            Feature::Screen,
            Feature::Scanner,
        ] {
            let got = availability(feature);
            assert!(
                got == feature.targets()[tilo_platform_caps::COLUMN]
                    || matches!(got, Availability::Unsupported(_)),
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
