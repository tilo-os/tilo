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
    /// to bring it: an output with something new to show that cannot take a
    /// frame yet. Nothing owed, nothing due: the compositor sleeps.
    fn next_deadline(&self, _state: &State) -> Option<std::time::Instant> {
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

    /// Apply the person's typing and pointing assists to the backend's
    /// devices, now and as they are plugged in. Backends whose input comes
    /// already shaped (a host window, a test) ignore them.
    fn set_assists(&mut self, _state: &mut State, _assists: &crate::Assists) {}

    /// Keep the person's display configuration for outputs that appear
    /// later (the compositor applies it to the present ones).
    fn set_display_config(&mut self, _config: &crate::config::DisplayConfig) {}

    /// What each output's sink and the pipeline driving it can do beyond
    /// what the output reports: a built-in panel, HDR and adaptive sync, the
    /// colour profiles on offer, why a connected display is dark. Backends
    /// without hardware of their own have nothing to add.
    fn output_capabilities(&self) -> Vec<crate::settings_bridge::Capability> {
        Vec::new()
    }

    /// How this backend drives its outputs (`drm-kms`, `headless`), as
    /// Settings names it when it explains a display.
    fn describe(&self) -> &'static str {
        "unknown"
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

    /// Send the output named `name`'s signal through `ramp` (`None`: no
    /// ramp), whose size the backend gave
    /// ([`crate::gamma::Gamma::set_size`]). Backends without ramps carry
    /// no sizes and are never asked.
    fn set_gamma(
        &mut self,
        _state: &mut State,
        name: &str,
        _ramp: Option<&crate::gamma::Ramp>,
    ) -> Result<(), BackendError> {
        Err(BackendError(format!(
            "{name}: this backend carries no gamma ramps"
        )))
    }

    /// Draw and blur what lies below a hosted surface's translucent region
    /// (see [`crate::backdrop::draw`]); `None` when it did not change.
    /// Backends that cannot draw outside their frames have none.
    fn backdrop(
        &mut self,
        _state: &mut State,
        _region: (usize, usize),
        _request: &crate::BackdropRequest,
    ) -> Result<Option<crate::BackdropImage>, BackendError> {
        Ok(None)
    }

    /// Draw what a capture frame copies (part of an output, a window) into
    /// the client's buffer with the backend's renderer (see
    /// [`crate::capture::copy`]). Backends that cannot draw outside their
    /// frames refuse.
    fn capture(
        &mut self,
        _state: &mut State,
        _frame: &crate::capture::Frame,
    ) -> Result<crate::capture::Copied, crate::capture::CaptureError> {
        Err(crate::capture::CaptureError::Backend(BackendError(
            "this backend does not capture".into(),
        )))
    }

    /// What `target` shows now, read back as rows of R, G, B, A (see
    /// [`crate::capture::read_back`]): what a share stream pushes.
    fn read_back(
        &mut self,
        _state: &mut State,
        _target: &crate::capture::Target,
    ) -> Result<Screenshot, crate::capture::CaptureError> {
        Err(crate::capture::CaptureError::Backend(BackendError(
            "this backend does not read frames back".into(),
        )))
    }

    /// The last frame drawn on the output named `name`.
    fn screenshot(&mut self, _state: &mut State, name: &str) -> Result<Screenshot, BackendError> {
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

    /// Write the gamma ramps whose wanted value changed (night light, a
    /// gamma tool).
    pub(crate) fn sync_gamma(&mut self, state: &mut State) {
        let shown: Vec<String> = state
            .space
            .outputs()
            .map(smithay::output::Output::name)
            .collect();
        for (name, ramp) in state.gamma.changes(&shown) {
            match self.set_gamma(state, &name, ramp.as_ref()) {
                Ok(()) => state.gamma.applied(&name, ramp),
                Err(error) => {
                    tracing::warn!(%error, output = name, "gamma ramp not written");
                    state.gamma.refused(&name);
                }
            }
        }
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
