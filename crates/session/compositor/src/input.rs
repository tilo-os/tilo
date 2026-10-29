//! Input as the compositor routes it, whatever device produced it.
//!
//! Backends translate their devices (libinput, a host window, a test or a
//! remote seat) into these events; the compositor routes each to the hosted
//! surface or client it belongs to.

use smithay::backend::input::{
    AbsolutePositionEvent, Axis, AxisSource, ButtonState, Device as _, GestureBeginEvent,
    GestureEndEvent, GesturePinchUpdateEvent, GestureSwipeUpdateEvent, InputBackend,
    InputEvent as DeviceEvent, KeyState, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent,
    PointerMotionEvent, ProximityState, Switch as DeviceSwitch, SwitchState, SwitchToggleEvent,
    TabletToolButtonEvent, TabletToolDescriptor, TabletToolEvent, TabletToolProximityEvent,
    TabletToolTipEvent, TabletToolTipState, TouchEvent, TouchSlot,
};
use smithay::utils::{Logical, Size};
use smithay::wayland::tablet_manager::TabletDescriptor;

/// The pointer button a pen's tip presses for apps that know no tablets.
pub const BTN_LEFT: u32 = 0x110;

/// One input event, in the logical layout.
#[derive(Debug, Clone, PartialEq)]
pub enum InputEvent {
    /// The pointer moved to an absolute logical position.
    PointerMotion { x: f64, y: f64 },
    /// The pointer moved by a logical offset.
    PointerMotionRelative { dx: f64, dy: f64 },
    /// A pointer button (evdev code, `BTN_LEFT` = `0x110`) changed state.
    PointerButton { button: u32, pressed: bool },
    /// Scrolling, in logical pixels.
    PointerAxis { horizontal: f64, vertical: f64 },
    /// A key (evdev code) changed state.
    Key { keycode: u32, pressed: bool },
    /// A finger touched the screen at a logical position. `slot` tells
    /// fingers apart until they lift.
    TouchDown { slot: u32, x: f64, y: f64 },
    /// A touching finger moved.
    TouchMotion { slot: u32, x: f64, y: f64 },
    /// A finger lifted.
    TouchUp { slot: u32 },
    /// The touches reported since the last frame belong together.
    TouchFrame,
    /// Every current touch is void (the device took it back): clients
    /// undo what the touches did.
    TouchCancel,
    /// A drawing tablet's tool (pen, eraser, ...) did something.
    Tablet(TabletEvent),
    /// A touchpad gesture.
    Gesture(GestureEvent),
    /// A switch (laptop lid, tablet mode) changed state.
    Switch { switch: Switch, on: bool },
}

/// A switch the compositor hears about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Switch {
    /// The laptop lid; on is closed.
    Lid,
    /// A convertible folded into tablet shape; on is tablet mode.
    TabletMode,
}

/// A touchpad gesture, as a stream: a begin, updates, an end.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GestureEvent {
    SwipeBegin {
        fingers: u32,
    },
    /// The fingers' centre moved by a logical offset.
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
    /// `scale` is absolute against the begin; `rotation` is degrees
    /// clockwise since the last update.
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

impl GestureEvent {
    /// The finger count, on the events that begin a gesture.
    pub const fn fingers(&self) -> Option<u32> {
        match self {
            Self::SwipeBegin { fingers }
            | Self::PinchBegin { fingers }
            | Self::HoldBegin { fingers } => Some(*fingers),
            _ => None,
        }
    }

    /// Whether this event ends its gesture.
    pub const fn ends(&self) -> bool {
        matches!(
            self,
            Self::SwipeEnd { .. } | Self::PinchEnd { .. } | Self::HoldEnd { .. }
        )
    }
}

/// What a tablet tool did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabletPhase {
    ProximityIn,
    ProximityOut,
    Motion,
    TipDown,
    TipUp,
    /// A tool button (evdev code, `BTN_STYLUS` = `0x14b`).
    Button {
        button: u32,
        pressed: bool,
    },
}

/// One tablet tool event, already mapped into the logical layout.
#[derive(Debug, Clone, PartialEq)]
pub struct TabletEvent {
    /// The tablet the tool is on.
    pub tablet: TabletDescriptor,
    /// The tool, with the axes it reports.
    pub tool: TabletToolDescriptor,
    pub phase: TabletPhase,
    /// Where the tool is, in the logical layout.
    pub x: f64,
    pub y: f64,
    /// `0.0 ..= 1.0`.
    pub pressure: f64,
    /// `0.0 ..= 1.0`.
    pub distance: f64,
    /// Degrees from the vertical on each axis.
    pub tilt: (f64, f64),
    pub rotation: f64,
    pub slider: f64,
    /// Degrees and clicks the tool's wheel turned.
    pub wheel: (f64, i32),
    /// Deliver the tool as a pointer even to apps that take tablets (an
    /// eraser the person mapped to a mouse button).
    pub as_pointer: bool,
    /// The pointer button the tip presses for apps that see a pointer;
    /// `None` clicks nothing.
    pub tip_button: Option<u32>,
}

impl InputEvent {
    /// A device event as the compositor routes it, whichever backend read
    /// it (libinput on a TTY, the host window when nested); absolute
    /// positions are mapped onto `extent`, the logical size they span.
    /// Events the compositor does not route yet give `None`.
    pub(crate) fn from_device<B: InputBackend>(
        event: DeviceEvent<B>,
        extent: Size<i32, Logical>,
    ) -> Option<Self> {
        match event {
            DeviceEvent::Keyboard { event } => Some(Self::Key {
                // xkb keycodes are evdev codes plus 8.
                keycode: event.key_code().raw().saturating_sub(8),
                pressed: event.state() == KeyState::Pressed,
            }),
            DeviceEvent::PointerMotion { event } => {
                let delta = event.delta();
                Some(Self::PointerMotionRelative {
                    dx: delta.x,
                    dy: delta.y,
                })
            }
            DeviceEvent::PointerMotionAbsolute { event } => {
                let at = event.position_transformed(extent);
                Some(Self::PointerMotion { x: at.x, y: at.y })
            }
            DeviceEvent::PointerButton { event } => Some(Self::PointerButton {
                button: event.button_code(),
                pressed: event.state() == ButtonState::Pressed,
            }),
            DeviceEvent::PointerAxis { event } => Some(Self::PointerAxis {
                horizontal: scroll_amount::<B>(&event, Axis::Horizontal),
                vertical: scroll_amount::<B>(&event, Axis::Vertical),
            }),
            DeviceEvent::TouchDown { event } => {
                let at = event.position_transformed(extent);
                Some(Self::TouchDown {
                    slot: touch_slot(event.slot()),
                    x: at.x,
                    y: at.y,
                })
            }
            DeviceEvent::TouchMotion { event } => {
                let at = event.position_transformed(extent);
                Some(Self::TouchMotion {
                    slot: touch_slot(event.slot()),
                    x: at.x,
                    y: at.y,
                })
            }
            DeviceEvent::TouchUp { event } => Some(Self::TouchUp {
                slot: touch_slot(event.slot()),
            }),
            DeviceEvent::TouchFrame { .. } => Some(Self::TouchFrame),
            DeviceEvent::TouchCancel { .. } => Some(Self::TouchCancel),
            other => Self::from_device_more(other, extent),
        }
    }

    /// The rest of [`Self::from_device`]: gestures, switches and tablets.
    fn from_device_more<B: InputBackend>(
        event: DeviceEvent<B>,
        extent: Size<i32, Logical>,
    ) -> Option<Self> {
        match event {
            DeviceEvent::GestureSwipeBegin { event } => {
                Some(Self::Gesture(GestureEvent::SwipeBegin {
                    fingers: event.fingers(),
                }))
            }
            DeviceEvent::GestureSwipeUpdate { event } => {
                Some(Self::Gesture(GestureEvent::SwipeUpdate {
                    dx: GestureSwipeUpdateEvent::delta_x(&event),
                    dy: GestureSwipeUpdateEvent::delta_y(&event),
                }))
            }
            DeviceEvent::GestureSwipeEnd { event } => Some(Self::Gesture(GestureEvent::SwipeEnd {
                cancelled: event.cancelled(),
            })),
            DeviceEvent::GesturePinchBegin { event } => {
                Some(Self::Gesture(GestureEvent::PinchBegin {
                    fingers: event.fingers(),
                }))
            }
            DeviceEvent::GesturePinchUpdate { event } => {
                Some(Self::Gesture(GestureEvent::PinchUpdate {
                    dx: GesturePinchUpdateEvent::delta_x(&event),
                    dy: GesturePinchUpdateEvent::delta_y(&event),
                    scale: event.scale(),
                    rotation: event.rotation(),
                }))
            }
            DeviceEvent::GesturePinchEnd { event } => Some(Self::Gesture(GestureEvent::PinchEnd {
                cancelled: event.cancelled(),
            })),
            DeviceEvent::GestureHoldBegin { event } => {
                Some(Self::Gesture(GestureEvent::HoldBegin {
                    fingers: event.fingers(),
                }))
            }
            DeviceEvent::GestureHoldEnd { event } => Some(Self::Gesture(GestureEvent::HoldEnd {
                cancelled: event.cancelled(),
            })),
            DeviceEvent::SwitchToggle { event } => Some(Self::Switch {
                switch: match event.switch()? {
                    DeviceSwitch::Lid => Switch::Lid,
                    DeviceSwitch::TabletMode => Switch::TabletMode,
                    _ => return None,
                },
                on: event.state() == SwitchState::On,
            }),
            DeviceEvent::TabletToolProximity { event } => {
                let phase = match event.state() {
                    ProximityState::In => TabletPhase::ProximityIn,
                    ProximityState::Out => TabletPhase::ProximityOut,
                };
                Some(Self::Tablet(TabletEvent::read::<B, _>(
                    &event, phase, extent,
                )))
            }
            DeviceEvent::TabletToolAxis { event } => Some(Self::Tablet(TabletEvent::read::<B, _>(
                &event,
                TabletPhase::Motion,
                extent,
            ))),
            DeviceEvent::TabletToolTip { event } => {
                let phase = match event.tip_state() {
                    TabletToolTipState::Down => TabletPhase::TipDown,
                    TabletToolTipState::Up => TabletPhase::TipUp,
                };
                Some(Self::Tablet(TabletEvent::read::<B, _>(
                    &event, phase, extent,
                )))
            }
            DeviceEvent::TabletToolButton { event } => {
                let phase = TabletPhase::Button {
                    button: event.button(),
                    pressed: event.button_state() == ButtonState::Pressed,
                };
                Some(Self::Tablet(TabletEvent::read::<B, _>(
                    &event, phase, extent,
                )))
            }
            _ => None,
        }
    }
}

impl TabletEvent {
    /// A tool event with its axes, the tablet's surface mapped onto
    /// `extent`.
    fn read<B: InputBackend, E: TabletToolEvent<B>>(
        event: &E,
        phase: TabletPhase,
        extent: Size<i32, Logical>,
    ) -> Self {
        let device = event.device();
        let at = event.position_transformed(extent);
        Self {
            tablet: TabletDescriptor {
                name: device.name(),
                usb_id: device.usb_id(),
                syspath: device.syspath(),
            },
            tool: event.tool(),
            phase,
            x: at.x,
            y: at.y,
            pressure: event.pressure(),
            distance: event.distance(),
            tilt: event.tilt(),
            rotation: event.rotation(),
            slider: event.slider_position(),
            wheel: (event.wheel_delta(), event.wheel_delta_discrete()),
            as_pointer: false,
            tip_button: Some(BTN_LEFT),
        }
    }
}

/// A touch slot as a plain number: single-touch devices have no slot
/// concept (libinput reports none), all one finger, so slot 0 is honest.
pub(crate) fn touch_slot(slot: TouchSlot) -> u32 {
    u32::try_from(i32::from(slot)).unwrap_or(0)
}

/// Logical pixels scrolled on `axis`: a wheel notch (120 in v120 units) is
/// [`WHEEL_STEP`] pixels, as clients expect; fingers and continuous devices
/// report pixels.
fn scroll_amount<B: InputBackend>(event: &B::PointerAxisEvent, axis: Axis) -> f64 {
    match event.source() {
        AxisSource::Wheel | AxisSource::WheelTilt => event.amount_v120(axis).map_or_else(
            || event.amount(axis).unwrap_or(0.0),
            |v120| v120 * WHEEL_STEP / 120.0,
        ),
        _ => event.amount(axis).unwrap_or(0.0),
    }
}

/// Logical pixels one wheel notch scrolls.
const WHEEL_STEP: f64 = 15.0;

/// A key as a hosted surface receives it: the compositor's keymap already
/// applied, so hosts need no keyboard state of their own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKey {
    /// The evdev code.
    pub keycode: u32,
    /// Pressed or released.
    pub pressed: bool,
    /// A press repeated while the key is held, not a new one.
    pub repeat: bool,
    /// The keysym with the active modifiers and layout applied.
    pub keysym: u32,
    /// The keysym of the key's first level (no Shift), for shortcuts.
    pub unmodified_keysym: u32,
    /// The character the key types, if it types one.
    pub text: Option<char>,
    /// The modifiers held as the key changed.
    pub modifiers: KeyModifiers,
}

/// Modifier state accompanying a [`HostKey`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools, reason = "one flag per modifier key")]
pub struct KeyModifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub logo: bool,
    pub caps_lock: bool,
}
