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

use crate::output::{OutputChange, OutputConfig};
use crate::state::State;

pub use drm::input_config::InputConfig;
pub use drm::tablet::TabletConfig;
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

    /// Apply the person's input preferences to the backend's devices.
    /// Backends whose input comes already shaped (a host window, a test)
    /// ignore them.
    fn set_input_config(&mut self, _state: &mut State, _config: InputConfig) {}

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

    /// Apply a display configuration (already checked to name real
    /// outputs, with usable scales and one output left on), or with `test`
    /// only say whether it could be applied. All or nothing. Outputs it
    /// turns off or on are then switched by [`Backend::set_power`] and
    /// shown or hidden by the compositor.
    fn configure(
        &mut self,
        _state: &mut State,
        _changes: &[OutputChange],
        _test: bool,
    ) -> Result<(), BackendError> {
        Err(BackendError(
            "this backend's outputs cannot be reconfigured".into(),
        ))
    }

    /// Light the panel of the output named `name` or switch it off (idle
    /// blanking, a power tool, a configuration that turns it off). An
    /// output switched off draws nothing until it is lit again. Backends
    /// without panels have nothing to switch.
    fn set_power(
        &mut self,
        _state: &mut State,
        _name: &str,
        _on: bool,
    ) -> Result<(), BackendError> {
        Ok(())
    }

    /// The last frame drawn on the output named `name`.
    fn screenshot(&mut self, name: &str) -> Result<Screenshot, BackendError> {
        Err(BackendError(format!(
            "{name}: this backend does not read frames back"
        )))
    }
}

impl dyn Backend {
    /// [`Backend::configure`] after the checks every backend shares, then
    /// the windows laid out on the new arrangement.
    pub(crate) fn configure_checked(
        &mut self,
        state: &mut State,
        changes: &[OutputChange],
        test: bool,
    ) -> Result<(), BackendError> {
        let outputs = state.output_states();
        crate::output::check_changes(changes, &outputs).map_err(BackendError)?;
        self.configure(state, changes, test)?;
        if test {
            return Ok(());
        }
        for change in changes {
            let on = outputs.iter().any(|(name, on)| name == &change.name && *on);
            if on && !change.enabled {
                state.disable_output(&change.name);
            } else if !on && change.enabled {
                state.enable_output(&change.name);
            }
        }
        state.rearrange();
        self.sync_power(state);
        Ok(())
    }

    /// Switch the panels whose wanted power changed (idle blanking, a power
    /// tool, a configuration) and tell power tools how they are.
    pub(crate) fn sync_power(&mut self, state: &mut State) {
        let outputs = state.output_states();
        for (name, on) in state.output_power.changes(&outputs) {
            match self.set_power(state, &name, on) {
                Ok(()) => state.output_power.applied(&name, on),
                Err(error) => {
                    tracing::warn!(%error, output = name, on, "output power not switched");
                    state.output_power.applied(&name, !on);
                }
            }
        }
        let shown: Vec<String> = outputs.into_iter().map(|(name, _)| name).collect();
        state.output_power.retain_outputs(&shown);
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

/// Refuse turning an output off: the backend's one output is its window.
pub(crate) fn refuse_disabling(changes: &[OutputChange]) -> Result<(), BackendError> {
    match changes.iter().find(|change| !change.enabled) {
        Some(change) => Err(BackendError(format!(
            "{}: the window is the output; close it instead",
            change.name
        ))),
        None => Ok(()),
    }
}

/// A [`BackendError`] for `what`, with the underlying error's description.
pub fn failed(what: &str, error: impl std::fmt::Debug) -> BackendError {
    BackendError(format!("{what}: {error:?}"))
}
