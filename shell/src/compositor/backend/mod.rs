//! Backend trait abstracting frame submission + event polling for the
//! native compositor mode.
//!
//! [`WinitBackend`] hosts on top of an existing Wayland/X11 session for
//! development. [`DrmBackend`] drives a TTY directly.

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
    /// Compositor-space orientation. `size` is the post-transform logical
    /// extent; the DRM mode remains available via [`Self::physical_size`].
    pub transform: settings_store::hardware::OutputTransform,
    /// Canonical ICC path selected from the compositor-discovered profile
    /// set. Presenter re-validates/parses it with qcms before use.
    pub color_profile: Option<String>,
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
            transform: settings_store::hardware::OutputTransform::Normal,
            color_profile: None,
        }
    }

    pub fn physical_size(&self) -> (u32, u32) {
        use settings_store::hardware::OutputTransform::{Rotate90, Rotate270};
        if matches!(self.transform, Rotate90 | Rotate270) {
            (self.size.1, self.size.0)
        } else {
            self.size
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

/// A touchpad gesture, in the shape `zwp_pointer_gestures_v1` carries.
///
/// Backend-neutral rather than a `winit::WindowEvent`, because winit has
/// no event that carries a finger count — and the finger count is what
/// decides whether a gesture is the shell's or the focused client's.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum GestureEvent {
    SwipeBegin {
        fingers: u32,
    },
    SwipeUpdate {
        dx: f64,
        dy: f64,
    },
    SwipeEnd {
        cancelled: bool,
    },
    PinchBegin {
        fingers: u32,
    },
    PinchUpdate {
        dx: f64,
        dy: f64,
        scale: f64,
        rotation: f64,
    },
    PinchEnd {
        cancelled: bool,
    },
    HoldBegin {
        fingers: u32,
    },
    HoldEnd {
        cancelled: bool,
    },
}

/// One touch point's movement, in combined-output coordinates.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum TouchEvent {
    Down { slot: u32, x: f64, y: f64 },
    Motion { slot: u32, x: f64, y: f64 },
    Up { slot: u32 },
    Cancel,
    Frame,
}

#[derive(Debug)]
pub enum BackendEvent {
    CloseRequested,
    OutputResized {
        id: OutputId,
        size: (u32, u32),
        scale: f32,
    },
    /// The set of outputs changed — a display was plugged in, unplugged,
    /// or re-moded. Distinct from [`Self::OutputResized`], which is one
    /// output changing size: this one means the compositor has to re-read
    /// [`Backend::outputs`] wholesale and re-sync its `wl_output` globals,
    /// because an output may have appeared or vanished rather than moved.
    OutputsChanged,
    /// A pointer / keyboard / wheel event. The full `winit::WindowEvent`
    /// is carried so `compositor::native::run` can hand it to
    /// `FrameBuilder::dispatch_event` — the same dispatch path the
    /// headless `execute_planned_action` uses.
    Input(WindowEvent),
    /// Modifier-state changed. Tracked separately because
    /// `WindowEvent::ModifiersChanged` carries a `Modifiers` struct
    /// the loop needs to retain across subsequent events.
    ModifiersChanged(Modifiers),
    /// A gesture the shell did **not** claim, for forwarding to the
    /// focused client over `zwp_pointer_gestures_v1`.
    Gesture(GestureEvent),
    /// A gesture the shell *did* claim, already resolved into the
    /// navigation it means. Never also delivered as [`Self::Gesture`].
    ShellGesture(crate::compositor::backend::drm::gestures::GestureAction),
    /// A touch point, for forwarding to the focused client over
    /// `wl_touch`. The same touch is *also* delivered as pointer
    /// [`Self::Input`] events so the shell chrome — which has no touch
    /// path — responds to a finger.
    Touch(TouchEvent),
    Redraw,
    Focus(bool),
}

pub trait Backend {
    fn submit_frame(&mut self, scene: &Scene, output: OutputId);
    fn poll_events(&mut self, callback: &mut dyn FnMut(BackendEvent));
    fn outputs(&self) -> &[Output];

    /// The presentation interval of the display this backend drives, if
    /// it has one. The compositor paces its loop to this rather than to a
    /// hard-coded 60 Hz (see [`crate::compositor::pacing`]).
    ///
    /// Default `None`: [`HeadlessBackend`] has no display and reports
    /// nothing, so the compositor falls back to
    /// [`crate::compositor::pacing::FALLBACK_INTERVAL`] rather than a
    /// fake refresh rate.
    fn refresh_interval(&self) -> Option<std::time::Duration> {
        None
    }

    /// Split the presentation half of this backend off so the compositor
    /// can run it on a dedicated thread (see
    /// [`crate::compositor::present`]). Called once, before the loop
    /// starts. After a backend hands its presenter over,
    /// [`Backend::submit_frame`] is never called on it again.
    ///
    /// Default `None`, keeping presentation inline: [`HeadlessBackend`]
    /// does this deliberately, since its rendered bytes are read back
    /// synchronously by the test-control `snapshot` command, and a
    /// harness that waits for a frame to arrive has a race in it.
    fn take_presenter(&mut self) -> Option<Box<dyn crate::compositor::present::Presenter>> {
        None
    }

    /// Whether this backend puts a pointer on screen by itself.
    ///
    /// `true` for a backend whose display already carries a cursor the
    /// compositor doesn't paint — [`DrmBackend`]'s hardware cursor plane,
    /// and [`WinitBackend`], where the host session draws its own.
    /// Painting one into the scene too would put two on screen.
    ///
    /// Default `false` ([`HeadlessBackend`]: no display, nobody to draw
    /// it) rather than default-true, so a new display-free backend is
    /// visible by construction — an unnecessary arrow is a far easier
    /// defect to notice than an invisible one.
    fn has_cursor_plane(&self) -> bool {
        false
    }

    /// Power the physical display(s) on (`true`) or off (`false`) for
    /// idle / session-lock blanking.
    ///
    /// Default no-op: backends with no real output ([`WinitBackend`],
    /// [`HeadlessBackend`]) are unaffected. Only [`DrmBackend`] overrides
    /// this, stopping scanout and toggling the connector's DPMS state.
    /// Idempotent: redundant calls are harmless.
    fn set_dpms(&mut self, _on: bool) {}

    /// Re-establish the display after the system came back from suspend.
    ///
    /// Default no-op: a hosted backend's display is the host's problem,
    /// and a headless one has none. Only [`DrmBackend`] overrides it —
    /// the DRM device comes back from suspend with no mode set, no
    /// framebuffer bound and a cleared cursor plane, so this is what
    /// asks for all of that back. Separate from the VT-switch path
    /// because a suspend doesn't pause the seat: no session event
    /// arrives, so nothing else would trigger it.
    fn reset_display(&mut self) {}

    /// Re-read the persisted input preferences and apply them.
    ///
    /// Default no-op — only the DRM backend owns input devices. Called
    /// when the Settings app writes a change, so pointer speed and
    /// tap-to-click take effect without restarting the session.
    fn reload_input_settings(&mut self) {}

    /// Switch to another virtual terminal.
    ///
    /// Default no-op: a hosted backend has no VT of its own to leave.
    /// [`DrmBackend`] performs it through libseat, and has to — the
    /// compositor holds the keyboard in a raw mode, so the kernel never
    /// sees `Ctrl+Alt+Fn` and would otherwise leave the user with no way
    /// off this terminal for the life of the session.
    fn switch_vt(&mut self, _vt: i32) {}
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
                transform: settings_store::hardware::OutputTransform::Normal,
                color_profile: None,
            },
            Output {
                id: OutputId(1),
                position: (1920, 0),
                size: (1280, 1024),
                scale: 1.0,
                transform: settings_store::hardware::OutputTransform::Normal,
                color_profile: None,
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
