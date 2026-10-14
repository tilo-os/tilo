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
pub mod layout;
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
    /// Top-left corner of this output in the scene: the one texture the
    /// compositor paints, in physical pixels, spanning the bounding box of
    /// all outputs (see [`combined_geometry`]). Each output presents the
    /// sub-rect at its `position`. Derived from [`Self::logical_position`]
    /// by [`layout::scene_positions`]; see [`layout`] for why the two
    /// differ once scales do.
    pub position: (i32, i32),
    /// Top-left corner in the logical desktop layout — the coordinates
    /// Settings stores, `wl_output`/`zxdg_output_v1` advertise and the
    /// pointer moves in. Outputs tile edge to edge in
    /// [`Self::logical_size`] units.
    pub logical_position: (i32, i32),
    /// Extent in the scene, in physical pixels (the mode, rotated).
    pub size: (u32, u32),
    pub scale: f32,
    /// Compositor-space orientation. `size` is the post-transform logical
    /// extent; the DRM mode remains available via [`Self::physical_size`].
    pub transform: settings_store::hardware::OutputTransform,
    /// Canonical ICC path selected from the compositor-discovered profile
    /// set. Presenter re-validates/parses it with qcms before use.
    pub color_profile: Option<String>,
    /// The rate this output's mode scans out at, in millihertz, when the
    /// backend drives a real display. Advertised on this output's own
    /// `wl_output`, so a client on a 144 Hz monitor beside a 60 Hz panel
    /// paces to 144. `None` falls back to the compositor's pacing rate.
    pub refresh_mhz: Option<u32>,
    /// What clients are told this output is (`wl_output` name, make, model).
    pub identity: OutputIdentity,
}

/// An output driven in HDR: the sink's EDID description and where SDR white
/// sits, cd/m².
#[derive(Clone, Debug, PartialEq)]
pub struct OutputHdr {
    pub display: crate::wayland::edid::HdrDisplayInfo,
    pub sdr_white_nits: f32,
}

/// A presentation interval as the millihertz a `wl_output` mode carries.
/// `None` for a zero interval, which has no rate.
pub fn interval_to_mhz(interval: std::time::Duration) -> Option<u32> {
    if interval.is_zero() {
        return None;
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to [0.0, u32::MAX] and .round()-ed first, so the cast to u32 \
                  neither truncates a fraction nor loses a sign"
    )]
    let mhz = (1000.0 / interval.as_secs_f64())
        .round()
        .clamp(0.0, f64::from(u32::MAX)) as u32;
    Some(mhz)
}

/// The identity a `wl_output` advertises. Backends without real monitor
/// data leave it empty and the advertiser falls back to generic values.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutputIdentity {
    /// Connector key, e.g. `DP-2` — qualified by card when two GPUs share
    /// a connector name; also the `wl_output` name and the head name.
    pub connector: String,
    pub make: String,
    pub model: String,
    pub serial: String,
    /// Physical image size, zero when unknown.
    pub physical_mm: (u32, u32),
}

impl OutputIdentity {
    /// The device's own panel rather than a plugged-in display.
    pub fn is_builtin(&self) -> bool {
        ui::fullscreen::connector_is_builtin(&self.connector)
    }

    /// Physical size, `None` when the display reported none.
    pub fn known_physical_mm(&self) -> Option<(u32, u32)> {
        let (width, height) = self.physical_mm;
        (width > 0 && height > 0).then_some(self.physical_mm)
    }

    /// Identity from a connector and its EDID. EDID size wins over the
    /// connector-reported one, which some adapters leave at zero.
    pub fn from_monitor(
        connector: &str,
        monitor: Option<&settings_store::hardware::MonitorIdentity>,
        connector_mm: Option<(u32, u32)>,
    ) -> Self {
        Self {
            connector: connector.to_owned(),
            make: monitor.map(|m| m.make.clone()).unwrap_or_default(),
            model: monitor.map(|m| m.model.clone()).unwrap_or_default(),
            serial: monitor.map(|m| m.serial.clone()).unwrap_or_default(),
            physical_mm: monitor
                .and_then(|m| m.physical_size_mm)
                .or(connector_mm)
                .unwrap_or((0, 0)),
        }
    }
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
            refresh_mhz: None,
            logical_position: (0, 0),
            identity: OutputIdentity::default(),
        }
    }

    /// Extent in the logical layout: the mode over the scale, rotated.
    pub fn logical_size(&self) -> (u32, u32) {
        layout::logical_extent(self.physical_size(), f64::from(self.scale), self.transform)
    }

    /// Where the output sits in the logical desktop — the space the shell
    /// lays out in, clients are configured in and the pointer moves in.
    pub fn logical_rect(&self) -> vello::kurbo::Rect {
        let (w, h) = self.logical_size();
        let (x, y) = (
            f64::from(self.logical_position.0),
            f64::from(self.logical_position.1),
        );
        vello::kurbo::Rect::new(x, y, x + f64::from(w), y + f64::from(h))
    }

    /// Where the output's pixels sit in the painted scene.
    pub fn scene_rect(&self) -> vello::kurbo::Rect {
        let (x, y) = (f64::from(self.position.0), f64::from(self.position.1));
        vello::kurbo::Rect::new(x, y, x + f64::from(self.size.0), y + f64::from(self.size.1))
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

/// Bounding box (width, height) of every output's logical rectangle: the
/// size of the desktop the shell lays out. Equal to [`combined_geometry`]
/// when every scale is 1.
pub fn logical_geometry(outputs: &[Output]) -> (u32, u32) {
    let mut w: i64 = 0;
    let mut h: i64 = 0;
    for o in outputs {
        let (lw, lh) = o.logical_size();
        w = w.max(i64::from(o.logical_position.0).saturating_add(i64::from(lw)));
        h = h.max(i64::from(o.logical_position.1).saturating_add(i64::from(lh)));
    }
    let w = u32::try_from(w.max(0)).unwrap_or(u32::MAX);
    let h = u32::try_from(h.max(0)).unwrap_or(u32::MAX);
    (w, h)
}

/// The monitor behind each output, in order: the identity token a saved
/// display layout is matched by ([`settings_store::hardware::DisplaySet`]).
/// That is the EDID make, model and serial, with the connector appended
/// only to tell identical monitors apart, or the connector alone for a sink
/// without a readable EDID — so it survives the output being renumbered by
/// a restart or moved to another port.
fn monitor_keys(outputs: &[Output]) -> Vec<String> {
    let connectors: Vec<String> = outputs
        .iter()
        .map(|o| crate::compositor::output_heads::advertised_name(o, outputs))
        .collect();
    let monitors: Vec<settings_store::hardware::MonitorIdentity> = outputs
        .iter()
        .map(|o| settings_store::hardware::MonitorIdentity {
            make: o.identity.make.clone(),
            model: o.identity.model.clone(),
            serial: o.identity.serial.clone(),
            physical_size_mm: None,
        })
        .collect();
    let set = settings_store::hardware::DisplaySet::of(connectors.iter().zip(&monitors).map(
        |(connector, monitor)| settings_store::hardware::AttachedOutput {
            connector,
            connector_label: connector,
            monitor: Some(monitor),
        },
    ));
    connectors
        .iter()
        .map(|connector| set.members.get(connector).cloned().unwrap_or_default())
        .collect()
}

/// Translate the backend's output list into the geometry the shell lays out
/// and paints across: each display's logical rectangle, its scale, and
/// where its pixels go in the scene.
///
/// Outputs with a zero dimension are dropped: a connector mid-modeset
/// reports 0×0, which would hand a strip a display it can't place a window on.
pub fn output_geometry(outputs: &[Output]) -> Vec<crate::shell::overlay_host::OutputGeometry> {
    let monitors = monitor_keys(outputs);
    outputs
        .iter()
        .zip(monitors)
        .filter(|(o, _)| {
            let (lw, lh) = o.logical_size();
            o.size.0 > 0 && o.size.1 > 0 && lw > 0 && lh > 0
        })
        .map(|(o, monitor)| {
            let logical = o.logical_rect();
            crate::shell::overlay_host::OutputGeometry {
                id: o.id.0,
                monitor,
                x: logical.x0,
                y: logical.y0,
                width: logical.width(),
                height: logical.height(),
                scale: f64::from(o.scale),
                scene: o.scene_rect(),
            }
        })
        .collect()
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

/// One touch point's movement. A backend reports it in scene pixels; the
/// compositor carries it into the logical desktop with
/// [`layout::event_to_logical`] before anything hit-tests it.
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
    /// A real libinput keyboard event, preserving its kernel `CLOCK_MONOTONIC`
    /// timestamp before translation or input-loop queueing. Synthetic input
    /// and compositor-generated repeats never use this variant.
    PhysicalKey {
        event: WindowEvent,
        input_time_ns: u64,
    },
    /// Raw relative mouse motion, emitted by the direct backend alongside the
    /// clamped absolute cursor event. Spatial device control needs the delta
    /// after the local cursor reaches a display edge.
    /// The same delta before pointer acceleration and the event time in
    /// microseconds are what `zwp_relative_pointer_v1` hands to games.
    RelativePointerMotion {
        dx: f64,
        dy: f64,
        dx_unaccel: f64,
        dy_unaccel: f64,
        time_usec: u64,
    },
    /// A pen, eraser or other tablet tool, already mapped to global
    /// coordinates on the output the pen is tied to.
    Tablet(TabletInput),
    /// A tablet pad appeared, with the buttons, rings and strips it has.
    TabletPadAdded(PadDescription),
    /// The tablet pad with this device identity went away.
    TabletPadRemoved(String),
    /// A pad button, ring, strip or mode switch.
    TabletPad(PadEvent),
    /// A pad's mode button, or the button its settings pick for the map,
    /// went down (`shown`, with the pad's saved settings) or came up.
    TabletPadMap {
        pad: String,
        shown: Option<Box<settings_store::hardware::TabletConfig>>,
        /// Milliseconds, the input device's clock.
        time_msec: u32,
    },
    /// Modifier-state changed. Tracked separately because
    /// `WindowEvent::ModifiersChanged` carries a `Modifiers` struct
    /// the loop needs to retain across subsequent events.
    ModifiersChanged(Modifiers),
    /// The backend lost the ability to promise matching releases for keys it
    /// previously reported (keyboard unplug, VT pause, or keymap replacement).
    /// The compositor must retire its seat state before accepting new input.
    InputStateReset,
    /// A discovered typing keyboard's effective source assignment, before its key event.
    KeyboardConfiguration {
        scope: String,
        settings: Box<drm::input_config::KeyboardSettings>,
    },
    /// A gesture the shell did **not** claim, for forwarding to the
    /// focused client over `zwp_pointer_gestures_v1`.
    Gesture(GestureEvent),
    /// A gesture the shell *did* claim, already resolved into the
    /// navigation it means. Never also delivered as [`Self::Gesture`].
    ShellGesture(crate::compositor::backend::drm::gestures::GestureAction),
    /// A touch point, for forwarding to the focused client over
    /// `wl_touch`. The shell receives the same contact as raw
    /// `WindowEvent::Touch` through [`Self::Input`].
    Touch(TouchEvent),
    /// An accessibility filter rejected input (a bounce-keys press) and the
    /// user asked to see it: flash the shell's visual alert.
    VisualAlert,
    /// A physical input device appeared, with the capabilities it really
    /// has. `id` is stable for the device's lifetime and names it again in
    /// [`Self::InputDeviceRemoved`].
    InputDeviceAdded {
        id: String,
        capabilities: ui::InputAvailability,
    },
    /// The device announced under `id` went away.
    InputDeviceRemoved {
        id: String,
    },
    /// The class of physical input changed: the person typed, activated a
    /// pointer, touched the screen or used a controller after using
    /// something else. Reported by the source of the event, so emulated
    /// pointer input from a tap still counts as touch.
    InputUsed(ui::InputModality),
    /// Pointer hover changes precision and shortcut hints, but preserves a
    /// touch-started on-screen keyboard until deliberate pointer input.
    PointerMotionUsed,
    /// A convertible's tablet-mode switch changed: `true` when the keyboard
    /// is folded away and the touchscreen is the primary input.
    TabletMode(bool),
    Redraw,
    Focus(bool),
}

impl BackendEvent {
    /// Whether this event is the person using an input device, which keeps
    /// the session from dimming, blanking, locking and suspending. It does
    /// not matter who receives it: a pen stroke or a two-finger pinch that
    /// goes straight to an app is as much use as a key the shell handles.
    /// Exhaustive, so a new kind of input has to say whether it counts.
    pub fn is_person_input(&self) -> bool {
        match self {
            Self::Input(_)
            | Self::PhysicalKey { .. }
            | Self::RelativePointerMotion { .. }
            | Self::ModifiersChanged(_)
            | Self::Tablet(_)
            | Self::TabletPad(_)
            | Self::TabletPadMap { .. }
            | Self::Gesture(_)
            | Self::ShellGesture(_)
            | Self::Touch(_)
            | Self::TabletMode(_)
            // A key press an accessibility filter rejected was still pressed.
            | Self::VisualAlert => true,
            // Devices and outputs come and go on their own (a Bluetooth
            // reconnect, a monitor waking), and the rest is bookkeeping that
            // follows input already counted.
            Self::CloseRequested
            | Self::OutputResized { .. }
            | Self::OutputsChanged
            | Self::TabletPadAdded(_)
            | Self::TabletPadRemoved(_)
            | Self::InputStateReset
            | Self::KeyboardConfiguration { .. }
            | Self::InputDeviceAdded { .. }
            | Self::InputDeviceRemoved { .. }
            | Self::InputUsed(_)
            | Self::PointerMotionUsed
            | Self::Redraw
            | Self::Focus(_) => false,
        }
    }
}

/// The physical tablet a tool is used on.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TabletDevice {
    pub name: String,
    pub usb_id: Option<(u32, u32)>,
    pub syspath: Option<std::path::PathBuf>,
}

/// What kind of tool touches the tablet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TabletToolKind {
    Pen,
    Eraser,
    Brush,
    Pencil,
    Airbrush,
    Mouse,
    Lens,
    Totem,
    Unknown,
}

/// One physical tool, identified by its serial so a second pen is a second
/// tool, and the axes it reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent hardware capability flags, as the tablet protocol lists them"
)]
pub struct TabletTool {
    pub kind: TabletToolKind,
    pub serial: u64,
    pub wacom_id: u64,
    pub pressure: bool,
    pub distance: bool,
    pub tilt: bool,
    pub rotation: bool,
    pub slider: bool,
    pub wheel: bool,
}

/// What happened to the tool.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TabletPhase {
    /// The tool came into range.
    ProximityIn,
    /// The tool left range.
    ProximityOut,
    /// Position or axes changed.
    Motion,
    /// The tip touched the surface.
    TipDown,
    /// The tip lifted.
    TipUp,
    /// A tool button (evdev code) changed.
    Button { button: u32, pressed: bool },
}

/// One tablet tool event with every axis the tool reports.
#[derive(Clone, Debug, PartialEq)]
pub struct TabletInput {
    pub device: TabletDevice,
    pub tool: TabletTool,
    pub phase: TabletPhase,
    /// Global compositor coordinates.
    pub position: (f64, f64),
    /// `0.0..=1.0`.
    pub pressure: f64,
    /// `0.0..=1.0`.
    pub distance: f64,
    /// Degrees from vertical along each axis.
    pub tilt: (f64, f64),
    /// Degrees.
    pub rotation: f64,
    /// `-1.0..=1.0`.
    pub slider: f64,
    /// Degrees and discrete clicks.
    pub wheel: (f64, i32),
    /// Milliseconds, the input device's clock.
    pub time_msec: u32,
}

/// A tablet pad: its identity and the controls it reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PadDescription {
    /// The pad's `settings_store::hardware::input_device_key`, which its
    /// events and saved settings name it by.
    pub key: String,
    pub device: TabletDevice,
    pub buttons: u32,
    pub groups: Vec<PadGroup>,
    /// Where the controls sit on the pad, when libwacom knows the model.
    pub layout: Option<crate::compositor::pad_layout::PadLayout>,
}

/// One mode group of a pad: the controls that switch mode together.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PadGroup {
    pub buttons: Vec<u32>,
    /// The buttons that switch this group's mode.
    pub mode_buttons: Vec<u32>,
    pub rings: Vec<u32>,
    pub strips: Vec<u32>,
    pub modes: u32,
}

/// What happened on a pad.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PadAction {
    Button {
        button: u32,
        pressed: bool,
    },
    /// Degrees clockwise from the top; `None` when the finger lifted.
    Ring {
        ring: u32,
        angle: Option<f64>,
        finger: bool,
    },
    /// `0.0` at the top or left to `1.0`; `None` when the finger lifted.
    Strip {
        strip: u32,
        position: Option<f64>,
        finger: bool,
    },
    /// A mode button moved `group` to `mode` (of `modes`).
    ModeSwitch {
        group: u32,
        mode: u32,
        modes: u32,
    },
}

/// One pad event. A tablet-aware app with keyboard focus gets it over
/// `zwp_tablet_pad_v2`; anywhere else `fallback` stands in for it: what the
/// pad's settings map the control to, as ordinary input.
#[derive(Debug)]
pub struct PadEvent {
    /// The pad's device identity, as in [`PadDescription::key`].
    pub pad: String,
    pub action: PadAction,
    /// Milliseconds, the input device's clock.
    pub time_msec: u32,
    pub fallback: Vec<BackendEvent>,
}

/// Map a tool's position, normalised to `0.0..=1.0` across the tablet, onto
/// `target` (`x, y, width, height`, global): the output the pen is tied to,
/// or the whole desktop when it is tied to none.
pub fn map_tablet_position(normalised: (f64, f64), target: (f64, f64, f64, f64)) -> (f64, f64) {
    let (x, y, width, height) = target;
    (
        x + normalised.0.clamp(0.0, 1.0) * width,
        y + normalised.1.clamp(0.0, 1.0) * height,
    )
}

/// Where local pointer motion may take the pointer, in global coordinates.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum PointerConstraintRegion {
    #[default]
    Free,
    /// The pointer stays where it is; only relative motion is reported.
    Locked,
    /// The pointer stays inside the union of these `(x0, y0, x1, y1)` rects.
    Confined(Vec<(f64, f64, f64, f64)>),
}

impl PointerConstraintRegion {
    /// Where motion from `current` towards `requested` ends up, or `None`
    /// when the pointer must not move at all.
    pub fn constrain(&self, current: (f64, f64), requested: (f64, f64)) -> Option<(f64, f64)> {
        match self {
            Self::Free => Some(requested),
            Self::Locked => None,
            Self::Confined(rects) => {
                let inside = |(x, y): (f64, f64), &(x0, y0, x1, y1): &(f64, f64, f64, f64)| {
                    x >= x0 && x < x1 && y >= y0 && y < y1
                };
                if rects.iter().any(|rect| inside(requested, rect)) {
                    return Some(requested);
                }
                // Slide along the edge of the rect the pointer is in (or the
                // first one, if a relayout left it outside all of them).
                let &(x0, y0, x1, y1) = rects
                    .iter()
                    .find(|rect| inside(current, rect))
                    .or_else(|| rects.first())?;
                Some((
                    requested.0.clamp(x0, (x1 - 1.0).max(x0)),
                    requested.1.clamp(y0, (y1 - 1.0).max(y0)),
                ))
            }
        }
    }
}

pub trait Backend {
    fn submit_frame(&mut self, scene: &Scene, output: OutputId);

    /// Whether this backend consumes versioned native-pixel retained damage. Other backends use
    /// the established dense `Scene` path and should not pay to construct segmented recordings.
    fn supports_retained_scene_damage(&self) -> bool {
        false
    }

    /// CPU presenters can reuse strip pixels when layout uses whole-pixel pans.
    fn pixel_aligned_strip_pan(&self) -> bool {
        false
    }

    /// The presenter composites strip pans from retained layers
    /// (`FramePaint::cache`) instead of rasterising every pixel of the strip.
    fn composites_strip_layers(&self) -> bool {
        false
    }

    /// The presenter keeps its target between frames and copies what a
    /// scroll in an in-process app moved within it
    /// (`FramePaint::scroll_copies`) before it repaints the damage.
    fn copies_scrolls(&self) -> bool {
        false
    }

    /// Submit a frame with optional retained-damage authority. Backends that cannot retain a
    /// previous native target keep the established dense scene path.
    fn submit_frame_with_damage(&mut self, frame: &FramePaint, output: OutputId) {
        self.submit_frame(&frame.scene, output);
    }
    fn poll_events(&mut self, callback: &mut dyn FnMut(BackendEvent));

    /// Read input that arrived since the last poll, without blocking and
    /// without touching anything else [`Self::poll_events`] applies (the
    /// output table above all), so the run loop can call it between the
    /// phases of a turn while that turn's frame is being built. The events
    /// arrive in order and belong ahead of whatever the next
    /// [`Self::poll_events`] returns. Backends whose input does not arrive
    /// on their own loop have nothing to read here.
    fn poll_input(&mut self, _callback: &mut dyn FnMut(BackendEvent)) {}

    /// Duplicate an fd which becomes readable whenever this backend has
    /// events waiting for [`Self::poll_events`]. The compositor registers it
    /// in its outer calloop, avoiding periodic polling of a backend that owns
    /// a nested event loop (notably DRM/libinput).
    fn event_fd(&self) -> Option<OwnedFd> {
        None
    }

    /// Earliest time at which [`Self::poll_events`] can synthesize an event
    /// without any backend fd becoming readable. DRM uses this for native
    /// keyboard repeat: libinput wakes the first physical press/release, but
    /// repeat ticks are compositor-owned timers rather than kernel events.
    fn next_service_deadline(&self) -> Option<std::time::Instant> {
        None
    }

    /// Whether [`Self::poll_events`] can have news that neither
    /// [`Self::event_fd`] nor [`Self::next_service_deadline`] announces, so an
    /// idle compositor must keep polling it. [`HeadlessBackend`] has no input
    /// and no display: polling it would only wake an idle desktop 20 times a
    /// second to run the whole pre-paint pump for nothing.
    fn needs_idle_poll(&self) -> bool {
        true
    }

    /// Tell a display-less backend whether anyone is looking at its pixels.
    /// Returns `true` when that attaches a viewer to a backend that has
    /// been skipping frames, so the caller must paint a fresh one.
    ///
    /// Default: a backend with a display is always watched.
    fn set_watched(&mut self, _watched: bool) -> bool {
        false
    }

    /// Whether anyone may be looking at this backend's pixels, and so whether
    /// shared legacy windows are worth encoding.
    fn watched(&self) -> bool {
        true
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

    /// Every connector this backend knows, enabled or not, as advertised to
    /// output-management clients. The default treats each live output as a
    /// virtual connector whose only mode is the one it runs.
    fn output_heads(&self) -> Vec<crate::compositor::output_heads::OutputHead> {
        crate::compositor::output_heads::heads_from_outputs(self.outputs())
    }

    /// Power one output's panel on or off for an output-power client,
    /// independently of idle blanking ([`Self::set_dpms`]). Default no-op
    /// for backends with no physical panel.
    fn set_output_power(&mut self, _output: OutputId, _on: bool) {}

    /// The output a fullscreen client is shown on, or `None`. Backends that
    /// drive adaptive sync engage it there.
    fn set_fullscreen_output(&mut self, _output: Option<OutputId>) {}

    /// Adaptive sync is engaged on some output: pace frames by content
    /// readiness rather than a fixed refresh deadline.
    fn adaptive_sync_active(&self) -> bool {
        false
    }

    /// What `output`'s primary plane would scan out of a fullscreen client,
    /// as last learned while presenting one there; `None` when the output
    /// cannot scan a client out (no planes, rotated, calibrated, HDR) or the
    /// backend has no planes at all. Fullscreen clients are told through
    /// their dma-buf feedback ([`crate::compositor::scanout_feedback`]).
    #[cfg(target_os = "linux")]
    fn scanout_plane(
        &self,
        _output: OutputId,
    ) -> Option<std::sync::Arc<crate::compositor::scanout_feedback::ScanoutPlane>> {
        None
    }

    /// The HDR signal an output is being driven with, or `None` for SDR.
    /// Clients learn it through `wp_color_management_output_v1`.
    fn output_hdr(&self, _output: OutputId) -> Option<OutputHdr> {
        None
    }

    /// Turn the built-in panel to `rotation` because the device was turned
    /// (`None` restores its configured transform). External monitors never
    /// follow. Default no-op for backends without a built-in panel.
    fn set_device_rotation(
        &mut self,
        _rotation: Option<settings_store::hardware::OutputTransform>,
    ) {
    }

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

    /// Re-read the persisted input, keyboard, accessibility and tablet
    /// preferences and apply them.
    ///
    /// Default no-op — only the DRM backend owns input devices. Called
    /// when one of those settings sections changes, so pointer speed and
    /// tap-to-click take effect without restarting the session.
    fn reload_input_settings(&mut self) {}

    /// `display`, `display_transaction` or `display_profiles` changed:
    /// advance the display transaction and re-apply the arrangement, scale
    /// and modes from the connector state the last hotplug probe found.
    ///
    /// Must not force-probe connectors: this runs on the input path, and a
    /// forced KMS probe of every connector (slow on DP-MST docks) stalls
    /// pointer and keyboard dispatch. Only a hotplug re-probes. Default
    /// no-op for backends that own no displays.
    fn reload_display_settings(&mut self) {}

    /// Show `image` as the pointer. Default no-op: a hosted backend's
    /// pointer belongs to the host and a headless one has none.
    fn set_pointer_image(&mut self, _image: crate::compositor::cursor::PointerImage) {}

    /// Hold the pointer still (a game locked it) or keep it inside a region
    /// (a game confined it). Relative motion keeps being reported either
    /// way. Default no-op for backends that own no pointer.
    fn set_pointer_constraint(&mut self, _constraint: PointerConstraintRegion) {}

    /// Move the pointer to global `(x, y)` without a physical motion, as a
    /// released pointer lock asks with its cursor position hint.
    fn warp_pointer(&mut self, _x: f64, _y: f64) {}

    /// Switch the physical keyboard to input source `index` without
    /// waiting for the settings poll. Default no-op: only the DRM backend
    /// translates raw keycodes itself.
    fn set_keyboard_source(&mut self, _index: usize) {}

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
    fn combined_geometry_spans_every_output_and_is_zero_for_none() {
        let single = [Output::new(OutputId(0), (1920, 1080), 1.0)];
        assert_eq!(
            combined_geometry(&single),
            (1920, 1080),
            "single output equals its size"
        );

        let side_by_side = [
            Output {
                id: OutputId(0),
                position: (0, 0),
                size: (1920, 1080),
                scale: 1.0,
                transform: settings_store::hardware::OutputTransform::Normal,
                color_profile: None,
                refresh_mhz: None,
                logical_position: (0, 0),
                identity: OutputIdentity::default(),
            },
            Output {
                id: OutputId(1),
                position: (1920, 0),
                size: (1280, 1024),
                scale: 1.0,
                transform: settings_store::hardware::OutputTransform::Normal,
                color_profile: None,
                refresh_mhz: None,
                logical_position: (1920, 0),
                identity: OutputIdentity::default(),
            },
        ];
        // Combined width = 1920 + 1280 = 3200; height = max(1080, 1024).
        assert_eq!(
            combined_geometry(&side_by_side),
            (3200, 1080),
            "side by side"
        );

        assert_eq!(combined_geometry(&[]), (0, 0), "empty");
    }

    #[test]
    fn tablet_positions_follow_the_tied_output() {
        // Tied to a 1920x1080 output right of a 2560-wide one.
        let output = (2560.0, 0.0, 1920.0, 1080.0);
        assert_eq!(map_tablet_position((0.0, 0.0), output), (2560.0, 0.0));
        assert_eq!(map_tablet_position((0.5, 0.5), output), (3520.0, 540.0));
        assert_eq!(map_tablet_position((1.0, 1.0), output), (4480.0, 1080.0));
        // Out-of-range samples from the tablet edge stay on that output.
        assert_eq!(map_tablet_position((1.2, -0.1), output), (4480.0, 0.0));
    }

    #[test]
    fn pointer_constraints_hold_or_bound_motion() {
        assert_eq!(
            PointerConstraintRegion::Free.constrain((1.0, 1.0), (5.0, 6.0)),
            Some((5.0, 6.0))
        );
        assert_eq!(
            PointerConstraintRegion::Locked.constrain((1.0, 1.0), (5.0, 6.0)),
            None
        );
        let confined = PointerConstraintRegion::Confined(vec![
            (0.0, 0.0, 100.0, 50.0),
            (200.0, 0.0, 300.0, 50.0),
        ]);
        assert_eq!(
            confined.constrain((10.0, 10.0), (20.0, 30.0)),
            Some((20.0, 30.0))
        );
        assert_eq!(
            confined.constrain((10.0, 10.0), (150.0, 80.0)),
            Some((99.0, 49.0))
        );
        assert_eq!(
            confined.constrain((250.0, 10.0), (-5.0, 10.0)),
            Some((200.0, 10.0))
        );
        assert_eq!(
            confined.constrain((10.0, 10.0), (210.0, 10.0)),
            Some((210.0, 10.0))
        );
        assert_eq!(
            PointerConstraintRegion::Confined(Vec::new()).constrain((1.0, 1.0), (2.0, 2.0)),
            None
        );
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
