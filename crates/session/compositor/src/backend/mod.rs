//! Backends: where outputs come from, where frames go and where input
//! starts.
//!
//! A backend owns its renderer and devices. It registers what it listens to
//! (device descriptors, vblank events, a host window) in the compositor's
//! event loop when it is created, routes input into the [`State`], and draws
//! each output's scene ([`crate::render`]) when that output can take a frame.

pub mod drm;
pub mod headless;
pub mod nested;

use crate::output::OutputConfig;
use crate::state::State;

pub use drm::{DisplayConfig, DrmBackend};
pub use headless::HeadlessBackend;
pub use nested::{NestedBackend, NestedConfig};

/// What a backend does for the compositor.
pub trait Backend {
    /// Draw the outputs that can take a frame and tell their clients when
    /// it was shown.
    fn render(&mut self, state: &mut State) -> Result<(), BackendError>;

    /// When the backend next wants [`Backend::render`] called with no event
    /// to bring it (an output whose frame is due).
    fn next_deadline(&self) -> Option<std::time::Instant> {
        None
    }

    /// Whether the backend is done (its window was closed): the run loop
    /// ends.
    fn closed(&self) -> bool {
        false
    }

    /// Add an output the backend simulates. Backends with real outputs find
    /// them themselves and refuse.
    fn add_output(
        &mut self,
        _state: &mut State,
        config: &OutputConfig,
    ) -> Result<(), BackendError> {
        Err(BackendError(format!(
            "{}: this backend's outputs are its devices",
            config.name
        )))
    }

    /// The last frame drawn on the output named `name`.
    fn screenshot(&mut self, name: &str) -> Result<Screenshot, BackendError> {
        Err(BackendError(format!(
            "{name}: this backend does not read frames back"
        )))
    }
}

/// An output's pixels, read back after a frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Screenshot {
    /// Width in physical pixels.
    pub width: u32,
    /// Height in physical pixels.
    pub height: u32,
    /// Rows of RGBA8, top first, `width * 4` bytes each.
    pub rgba: Vec<u8>,
}

impl Screenshot {
    /// The RGBA of one pixel, if it lies inside the image.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let offset = (y as usize)
            .checked_mul(self.width as usize)?
            .checked_add(x as usize)?
            .checked_mul(4)?;
        let pixel = self.rgba.get(offset..offset.checked_add(4)?)?;
        pixel.try_into().ok()
    }
}

/// Why a backend failed: which step, and the device's or renderer's answer.
#[derive(Debug)]
pub struct BackendError(pub String);

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BackendError {}

/// A [`BackendError`] for `what`, with the underlying error's description.
pub fn failed(what: &str, error: impl std::fmt::Debug) -> BackendError {
    BackendError(format!("{what}: {error:?}"))
}
