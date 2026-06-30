//! Backend trait abstracting frame submission + event polling for the
//! native compositor mode.
//!
//! [`WinitBackend`] (this phase) hosts on top of an existing
//! Wayland/X11 session for development. [`DrmBackend`] (Phase 5) will
//! drive a TTY directly.

use vello::Scene;
// Absolute path: this module has a `pub mod winit;` child below that
// would otherwise shadow the external `winit` crate.
use ::winit::event::{Modifiers, WindowEvent};

pub mod drm;
pub mod headless;
pub mod winit;

pub use drm::DrmBackend;
pub use headless::HeadlessBackend;
pub use winit::WinitBackend;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct OutputId(pub u32);

#[derive(Clone, Debug)]
pub struct Output {
    pub id: OutputId,
    /// Top-left corner of this output in the compositor's combined
    /// coordinate space. Single-output backends place their one output
    /// at the origin; the multi-output DRM backend lays connectors out
    /// side-by-side, so the second monitor sits at e.g. `(1920, 0)`.
    /// The compositor renders one scene spanning the bounding box of all
    /// outputs (see [`combined_geometry`]) and each output presents the
    /// sub-rect at its `position`.
    pub position: (i32, i32),
    pub size: (u32, u32),
    pub scale: f32,
}

impl Output {
    /// Construct an output at the origin. Convenience for the
    /// single-output backends (winit / headless) so adding the
    /// `position` field didn't churn every call site into a struct
    /// literal.
    pub fn new(id: OutputId, size: (u32, u32), scale: f32) -> Self {
        Self {
            id,
            position: (0, 0),
            size,
            scale,
        }
    }
}

/// Bounding box (width, height) that covers every output given its
/// `position` + `size`. This is the size of the single scene the
/// compositor paints: each output then presents its own sub-rect.
///
/// For a single output at the origin this equals that output's size, so
/// the single-output path is unchanged. Returns `(0, 0)` for an empty
/// list (the caller falls back to a default viewport).
pub fn combined_geometry(outputs: &[Output]) -> (u32, u32) {
    let mut w: i32 = 0;
    let mut h: i32 = 0;
    for o in outputs {
        w = w.max(o.position.0 + o.size.0 as i32);
        h = h.max(o.position.1 + o.size.1 as i32);
    }
    (w.max(0) as u32, h.max(0) as u32)
}

#[derive(Debug)]
pub enum BackendEvent {
    CloseRequested,
    OutputResized { id: OutputId, size: (u32, u32), scale: f32 },
    /// A pointer / keyboard / wheel event. The full `winit::WindowEvent`
    /// is carried so `compositor::native::run` can hand it to
    /// `FrameBuilder::dispatch_event` — the same dispatch path the
    /// headless `execute_planned_action` uses.
    ///
    /// Previously a marker variant: the compositor knew "some input
    /// happened" but the event was discarded. That meant the native
    /// backend silently swallowed every click — visible only because
    /// hardware-clicking the window produced no widget reaction
    /// (headless tests bypassed this path entirely, so the bug hid).
    Input(WindowEvent),
    /// Modifier-state changed. Tracked separately because
    /// `WindowEvent::ModifiersChanged` carries a `Modifiers` struct
    /// the loop needs to retain across subsequent events.
    ModifiersChanged(Modifiers),
    Redraw,
    Focus(bool),
}

pub trait Backend {
    fn submit_frame(&mut self, scene: &Scene, output: OutputId);
    fn poll_events(&mut self, callback: &mut dyn FnMut(BackendEvent));
    fn outputs(&self) -> &[Output];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combined_geometry_single_output_equals_its_size() {
        let outs = [Output::new(OutputId(0), (1920, 1080), 1.0)];
        assert_eq!(combined_geometry(&outs), (1920, 1080));
    }

    #[test]
    fn combined_geometry_side_by_side_sums_width_takes_max_height() {
        let outs = [
            Output {
                id: OutputId(0),
                position: (0, 0),
                size: (1920, 1080),
                scale: 1.0,
            },
            Output {
                id: OutputId(1),
                position: (1920, 0),
                size: (1280, 1024),
                scale: 1.0,
            },
        ];
        // Combined width = 1920 + 1280 = 3200; height = max(1080, 1024).
        assert_eq!(combined_geometry(&outs), (3200, 1080));
    }

    #[test]
    fn combined_geometry_empty_is_zero() {
        assert_eq!(combined_geometry(&[]), (0, 0));
    }
}
