//! DRM/KMS + libinput backend for running on a TTY without a parent
//! Wayland/X11 compositor. Phase 5 Task 5.1 of the compositor plan.
//!
//! **Status: skeleton.** This unit lands the trait stub + `--backend
//! drm` flag dispatch. The actual implementation (libseat session
//! acquisition, udev DRM device enumeration, page-flip event sources
//! on the calloop loop, libinput integration) is deferred to a
//! follow-up task that requires TTY hardware to validate. Mirrors
//! smithay's `anvil/src/udev.rs` example.
//!
//! Until the real impl lands, `--backend drm` exits with a "not yet
//! implemented" message — the flag itself is wired through so future
//! work plugs in.

use vello::Scene;

use super::{Backend, BackendEvent, Output};

pub struct DrmBackend {
    /// Phase 5 follow-up: replace with real udev/DRM state.
    outputs: Vec<Output>,
}

impl DrmBackend {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        // Phase 5 follow-up: acquire libseat session, enumerate DRM
        // devices via udev, configure modes, set up page-flip event
        // sources on the calloop loop. For now, a hardware-less stub.
        Err("DRM backend not yet implemented; --backend drm requires Phase 5 follow-up".into())
    }
}

impl Backend for DrmBackend {
    fn submit_frame(&mut self, _scene: &Scene, _output: super::OutputId) {
        // Phase 5 follow-up: gbm buffer allocation + page flip.
    }

    fn poll_events(&mut self, _callback: &mut dyn FnMut(BackendEvent)) {
        // Phase 5 follow-up: drain libinput + DRM page-flip events.
    }

    fn outputs(&self) -> &[Output] {
        &self.outputs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drm_backend_new_returns_not_implemented_error() {
        // The skeleton's contract: `new()` errors with a message
        // explaining the follow-up. When the real impl lands, this
        // test gets replaced with hardware-gated integration tests.
        assert!(DrmBackend::new().is_err());
    }
}
