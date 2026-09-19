//! Backend trait abstracting frame submission + event polling for the
//! native compositor mode.
//!
//! [`WinitBackend`] hosts on top of an existing Wayland/X11 session for
//! development. [`DrmBackend`] drives a TTY directly.

use crate::compositor::frame::FramePaint;
use std::os::fd::OwnedFd;
use vello::Scene;
// Absolute path: this module has a `winit` child below that
// would otherwise shadow the external `winit` crate.
use ::winit::event::{Modifiers, WindowEvent};

#[cfg(target_os = "linux")]
pub mod drm;
#[cfg(not(target_os = "linux"))]
#[path = "drm/policy.rs"]
pub mod drm;
pub mod headless;
#[cfg(target_os = "linux")]
pub mod winit;

#[cfg(target_os = "linux")]
pub use drm::DrmBackend;
pub use headless::HeadlessBackend;
#[cfg(target_os = "linux")]
pub use winit::WinitBackend;

pub use crate::shell::render::OutputId;

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
    // Widen to `i64` up front: `i32::from` / `i64::from` on an `i32`
    // position or a `u32` size are lossless, so the running max can never
    // wrap or need a sign-losing cast, no matter what the backend reports.
    let mut w: i64 = 0;
    let mut h: i64 = 0;
    for o in outputs {
        let right = i64::from(o.position.0).saturating_add(i64::from(o.size.0));
        let bottom = i64::from(o.position.1).saturating_add(i64::from(o.size.1));
        w = w.max(right);
        h = h.max(bottom);
    }
    let w = u32::try_from(w.max(0)).unwrap_or(u32::MAX);
    let h = u32::try_from(h.max(0)).unwrap_or(u32::MAX);
    (w, h)
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
    /// Raw relative mouse motion, emitted by the direct backend alongside the
    /// clamped absolute cursor event. Spatial device control needs the delta
    /// after the local cursor reaches a display edge.
    RelativePointerMotion {
        dx: f64,
        dy: f64,
    },
    /// Modifier-state changed. Tracked separately because
    /// `WindowEvent::ModifiersChanged` carries a `Modifiers` struct
    /// the loop needs to retain across subsequent events.
    ModifiersChanged(Modifiers),
    /// The backend lost the ability to promise matching releases for keys it
    /// previously reported (keyboard unplug, VT pause, or keymap replacement).
    /// The compositor must retire its seat state before accepting new input.
    InputStateReset,
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

    /// Whether this backend consumes versioned native-pixel retained damage. Other backends use
    /// the established dense `Scene` path and should not pay to construct segmented recordings.
    fn supports_retained_scene_damage(&self) -> bool {
        false
    }

    /// Submit a frame with optional retained-damage authority. Backends that cannot retain a
    /// previous native target keep the established dense scene path.
    fn submit_frame_with_damage(&mut self, frame: &FramePaint, output: OutputId) {
        self.submit_frame(&frame.scene, output);
    }
    fn poll_events(&mut self, callback: &mut dyn FnMut(BackendEvent));

    /// Duplicate an fd which becomes readable whenever this backend has
    /// events waiting for [`Self::poll_events`]. The compositor registers it
    /// in its outer calloop, avoiding periodic polling of a backend that owns
    /// a nested event loop (notably DRM/libinput).
    fn event_fd(&self) -> Option<OwnedFd> {
        None
    }

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

    /// Cancel raw direct-touch contacts at a compositor ownership boundary.
    /// Display-only and hosted backends have no such source to retire.
    fn cancel_touch(&mut self) {}

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

/// Apply a host-window DPI change to the compositor's single output.
///
/// Winit emits `ScaleFactorChanged` when a window crosses monitors even when
/// its physical size stays the same.  Treating that as a resize lets the
/// compositor re-run its ordinary output/scale propagation path, including
/// `wp_fractional_scale` and layout.  The suggested inner size is deliberately
/// left to the subsequent `Resized` event; a scale-only event must still be
/// useful on platforms that keep the pixel size unchanged.
pub fn apply_scale_factor_changed(output: &mut Output, scale_factor: f64) -> Option<BackendEvent> {
    // Winit reports a positive, finite scale factor. Keep the guard here so a
    // malformed backend event cannot poison all subsequent geometry with NaN
    // or zero, and so the pure helper remains safe to reuse in tests.
    if !scale_factor.is_finite() || scale_factor <= 0.0 {
        return None;
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        reason = "monitor scale factor; f32 precision is ample for a UI scale value"
    )]
    let scale = scale_factor as f32;
    // A duplicate notification is harmless but needlessly rebuilds layout and
    // reconfigures the presentation pipe, so preserve the resize no-op rule.
    #[allow(
        clippy::float_cmp,
        reason = "exact equality avoids rebuilding layout for duplicate host notifications"
    )]
    if output.scale == scale {
        return None;
    }
    output.scale = scale;
    Some(BackendEvent::OutputResized {
        id: output.id,
        size: output.size,
        scale,
    })
}
#[cfg(test)]
mod scale_factor_tests {
    use super::*;

    #[test]
    fn scale_factor_change_updates_output_without_a_pixel_resize() {
        let mut output = Output::new(OutputId(7), (1920, 1080), 1.0);

        let event = apply_scale_factor_changed(&mut output, 1.5)
            .expect("a real DPI change must reach the compositor");

        assert_eq!(output.scale, 1.5);
        assert!(matches!(
            event,
            BackendEvent::OutputResized {
                id: OutputId(7),
                size: (1920, 1080),
                scale: 1.5,
            }
        ));
    }

    #[test]
    fn duplicate_or_invalid_scale_changes_are_ignored() {
        let mut output = Output::new(OutputId(0), (1280, 720), 1.25);
        assert!(apply_scale_factor_changed(&mut output, 1.25).is_none());
        assert!(apply_scale_factor_changed(&mut output, 0.0).is_none());
        assert!(apply_scale_factor_changed(&mut output, f64::NAN).is_none());
        assert_eq!(output.scale, 1.25);
    }
}
