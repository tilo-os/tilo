//! Host-owned input capabilities shared by the retained widget tree.
//!
//! Apps describe semantic controls once. The host updates this environment
//! from platform events, and controls adapt their hit targets and hover-only
//! affordances without app code branching on raw `winit` events.

use vello::kurbo::Rect;

/// The kind of interaction most recently used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputModality {
    /// Mouse, trackpad, stylus hover, or another pointing device.
    #[default]
    Pointer,
    /// Direct manipulation on a touchscreen.
    Touch,
    /// Keyboard navigation or activation.
    Keyboard,
    /// A game controller, including a single Joy-Con used sideways.
    Controller,
}

/// Controller legend/layout used when rendering face buttons.
///
/// Actions bind to positions (`South`, `East`, …), not printed letters: the
/// south button is `A` on Xbox, `B` on Nintendo and `×` on `PlayStation` hardware.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControllerFamily {
    Generic,
    Xbox,
    PlayStation,
    Nintendo,
    JoyConLeft,
    JoyConRight,
    JoyConPair,
}

impl ControllerFamily {
    pub fn from_hid(vendor_id: Option<u16>, name: &str) -> Self {
        match vendor_id {
            Some(0x045e) => Self::Xbox,
            Some(0x054c) => Self::PlayStation,
            _ => Self::from_device_name(name),
        }
    }

    /// Classify the names exposed by HID/udev, SDL or a platform gamepad API.
    /// Unknown hardware remains positional/generic rather than guessing face
    /// letters incorrectly. Linux hid-nintendo names a lone half "Nintendo
    /// Switch Left Joy-Con" (with " (Grip)" in the charging grip); joycond
    /// names a pair "Nintendo Switch Combined Joy-Cons".
    pub fn from_device_name(name: &str) -> Self {
        let name = name.to_ascii_lowercase();
        if name.contains("joy-con (l)")
            || name.contains("left joy-con")
            || name.contains("joycon left")
        {
            Self::JoyConLeft
        } else if name.contains("joy-con (r)")
            || name.contains("right joy-con")
            || name.contains("joycon right")
        {
            Self::JoyConRight
        } else if name.contains("joy-con") || name.contains("joycon") {
            Self::JoyConPair
        } else if name.contains("nintendo") || name.contains("pro controller") {
            Self::Nintendo
        } else if name.contains("dualsense")
            || name.contains("dualshock")
            || name.contains("playstation")
        {
            Self::PlayStation
        } else if name.contains("xbox") || name.contains("xinput") {
            Self::Xbox
        } else {
            Self::Generic
        }
    }
}

/// Physical legends printed on the connected keyboard. Tilo may run on Linux
/// with an Apple keyboard attached, so this cannot be inferred from the OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyboardFamily {
    Pc,
    Apple,
}

impl KeyboardFamily {
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Apple
        } else {
            Self::Pc
        }
    }

    pub fn from_device_name(name: &str) -> Self {
        let name = name.to_ascii_lowercase();
        if name.contains("apple") || name.contains("magic keyboard") {
            Self::Apple
        } else {
            Self::Pc
        }
    }

    pub fn from_hid(vendor_id: Option<u16>, name: &str) -> Self {
        if vendor_id == Some(0x05ac) {
            Self::Apple
        } else {
            Self::from_device_name(name)
        }
    }
}

/// Devices known to be available in this session. This is deliberately
/// separate from [`InputModality`]: moving a mouse must not make a connected
/// touchscreen or controller disappear from shortcut help.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum InputDevice {
    Keyboard = 1 << 0,
    Pointer = 1 << 1,
    Touch = 1 << 2,
    Controller = 1 << 3,
}

/// A compact set of the input devices discovered for this session.
///
/// Keeping capability mutation behind [`Self::with`] prevents modality updates
/// from accidentally rebuilding the set and forgetting an unrelated device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputAvailability(u8);

impl InputAvailability {
    pub const NONE: Self = Self(0);

    #[must_use]
    pub const fn with(self, device: InputDevice) -> Self {
        Self(self.0 | device as u8)
    }

    pub const fn contains(self, device: InputDevice) -> bool {
        self.0 & device as u8 != 0
    }

    /// Every device present in either set.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl Default for InputAvailability {
    fn default() -> Self {
        Self::NONE
            .with(InputDevice::Keyboard)
            .with(InputDevice::Pointer)
    }
}

/// Accuracy of the active pointing device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PointerPrecision {
    /// Mouse, trackpad, or another accurate pointer.
    #[default]
    Fine,
    /// Finger-sized direct manipulation target.
    Coarse,
}

/// Runtime interaction information propagated by the host to every widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InteractionEnvironment {
    pub modality: InputModality,
    pub pointer_precision: PointerPrecision,
    pub available: InputAvailability,
    pub controller: Option<ControllerFamily>,
    pub keyboard_family: KeyboardFamily,
}

impl Default for InteractionEnvironment {
    fn default() -> Self {
        Self {
            modality: InputModality::default(),
            pointer_precision: PointerPrecision::default(),
            available: InputAvailability::default(),
            controller: None,
            keyboard_family: KeyboardFamily::current(),
        }
    }
}

impl InteractionEnvironment {
    pub const POINTER: Self = Self {
        modality: InputModality::Pointer,
        pointer_precision: PointerPrecision::Fine,
        available: InputAvailability::NONE
            .with(InputDevice::Keyboard)
            .with(InputDevice::Pointer),
        controller: None,
        keyboard_family: KeyboardFamily::Pc,
    };

    pub const TOUCH: Self = Self {
        modality: InputModality::Touch,
        pointer_precision: PointerPrecision::Coarse,
        available: InputAvailability::NONE
            .with(InputDevice::Keyboard)
            .with(InputDevice::Touch),
        controller: None,
        keyboard_family: KeyboardFamily::Pc,
    };

    /// Minimum square target controls should expose in logical pixels.
    pub const fn minimum_target(self) -> f32 {
        match self.pointer_precision {
            PointerPrecision::Fine => 32.0,
            PointerPrecision::Coarse => 48.0,
        }
    }

    pub fn minimum_target_rect(self, visual: Rect) -> Rect {
        let side = f64::from(self.minimum_target());
        Rect::from_center_size(
            visual.center(),
            (visual.width().max(side), visual.height().max(side)),
        )
    }

    /// Hover affordances have no meaning for a coarse direct pointer.
    pub const fn supports_hover(self) -> bool {
        matches!(self.pointer_precision, PointerPrecision::Fine)
    }

    /// Input language to lead with in shortcut help: the one the person used
    /// last. Typing after touch leads with keyboard shortcuts, touching after
    /// typing leads with gestures, and a controller leads only while it is
    /// still connected. Pointer use carries no instructions of its own, so it
    /// defers to keyboard hints when a keyboard exists and to gestures on a
    /// touch-only device.
    pub const fn preferred_hint_modality(self) -> InputModality {
        match self.modality {
            InputModality::Controller if self.available.contains(InputDevice::Controller) => {
                InputModality::Controller
            }
            InputModality::Touch => InputModality::Touch,
            InputModality::Keyboard => InputModality::Keyboard,
            InputModality::Pointer | InputModality::Controller => {
                if self.available.contains(InputDevice::Keyboard) {
                    InputModality::Keyboard
                } else if self.available.contains(InputDevice::Touch)
                    && !self.available.contains(InputDevice::Pointer)
                {
                    InputModality::Touch
                } else {
                    InputModality::Pointer
                }
            }
        }
    }

    /// Minimum vertical extent for adjacent full-width collection rows.
    /// Fine pointers can distinguish compact rows; direct touch cannot.
    pub const fn minimum_collection_row(self) -> f32 {
        match self.pointer_precision {
            PointerPrecision::Fine => 1.0,
            PointerPrecision::Coarse => 48.0,
        }
    }

    /// Preserve pointer capability while recording keyboard navigation.
    #[must_use]
    pub const fn with_keyboard_modality(self) -> Self {
        Self {
            modality: InputModality::Keyboard,
            available: self.available.with(InputDevice::Keyboard),
            ..self
        }
    }

    /// Record pointer use while preserving every previously discovered device.
    #[must_use]
    pub const fn with_pointer_modality(self) -> Self {
        Self {
            modality: InputModality::Pointer,
            pointer_precision: PointerPrecision::Fine,
            available: self.available.with(InputDevice::Pointer),
            ..self
        }
    }

    /// Record direct touch while preserving keyboard, mouse and controller
    /// availability for multi-device systems such as convertibles.
    #[must_use]
    pub const fn with_touch_modality(self) -> Self {
        Self {
            modality: InputModality::Touch,
            pointer_precision: PointerPrecision::Coarse,
            available: self.available.with(InputDevice::Touch),
            ..self
        }
    }

    /// Record a controller event and retain its physical legend family.
    #[must_use]
    pub const fn with_controller(self, family: ControllerFamily) -> Self {
        Self {
            modality: InputModality::Controller,
            available: self.available.with(InputDevice::Controller),
            controller: Some(family),
            ..self
        }
    }
}

/// The navigation a mouse side button asks for: Back and Forward go back and
/// forward through whatever history the view under them owns, as in every
/// desktop browser and file manager.
pub(crate) fn mouse_nav_direction(
    event: &winit::event::WindowEvent,
) -> Option<crate::gesture::NavDirection> {
    use crate::gesture::NavDirection;
    use winit::event::{ElementState, MouseButton, WindowEvent};
    match event {
        WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Back,
            ..
        } => Some(NavDirection::Back),
        WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Forward,
            ..
        } => Some(NavDirection::Forward),
        _ => None,
    }
}

/// Android's Back key as the Escape the toolkit dismisses with; what Escape
/// leaves unconsumed falls through to back navigation, then to closing.
#[cfg(any(feature = "native", test))]
pub(crate) fn back_key_as_escape(
    event: &winit::event::WindowEvent,
) -> Option<winit::event::WindowEvent> {
    use winit::keyboard::{Key, NamedKey};
    match event {
        winit::event::WindowEvent::KeyboardInput { event: key, .. }
            if key.logical_key == Key::Named(NamedKey::BrowserBack) =>
        {
            Some(crate::synthetic_input::key_input(
                &Key::Named(NamedKey::Escape),
                key.state,
            ))
        }
        _ => None,
    }
}

/// What a contact means to the widget where it begins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TouchClaim {
    /// Ordinary content: a tap clicks, a drag scrolls, a hold is the
    /// secondary click.
    #[default]
    Pointer,
    /// A drag moves something (a canvas item, a text selection): past the
    /// tap's slop it is a left-button drag; a tap and a hold are as for
    /// [`Self::Pointer`] ([`Widget::touch_claim`](crate::widget::Widget::touch_claim)).
    Drag,
    /// A held item can be picked up and moved (a page in a list that also
    /// scrolls): a drag scrolls as for [`Self::Pointer`], but a still hold
    /// lifts the item, a left press where the finger rests, and a drag after
    /// it moves the item. A hold lifted where it began is the secondary
    /// click, made before the left button lifts: a widget ends the pick-up
    /// there, as for a mouse's right press during a drag.
    HoldDrag,
    /// A sideways drag moves something (a switch's knob) while a column of
    /// such things still scrolls: the contact's direction is read once, as
    /// it passes the tap's slop. Mostly across, it is a drag as for
    /// [`Self::Drag`]; mostly up or down, it scrolls as for
    /// [`Self::Pointer`]. A tap and a hold are as for [`Self::Pointer`].
    DragAcross,
    /// The widget reads the raw contact stream itself
    /// ([`Widget::claims_raw_touch`](crate::widget::Widget::claims_raw_touch)).
    Raw,
}

/// Pointer events synthesised from direct touch, so ordinary widgets work on a
/// touchscreen without interpreting contact streams themselves.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TouchPointerEvent {
    Move(winit::dpi::PhysicalPosition<f64>),
    Press(winit::event::MouseButton),
    Release(winit::event::MouseButton),
    Scroll {
        delta: winit::dpi::PhysicalPosition<f64>,
        phase: winit::event::TouchPhase,
    },
    /// Incremental magnification, with `WindowEvent::PinchGesture`'s
    /// meaning: the content scale is multiplied by `1.0 + delta`.
    Pinch {
        delta: f64,
        phase: winit::event::TouchPhase,
    },
}

impl TouchPointerEvent {
    /// The window event every widget already understands.
    pub fn window_event(self, device_id: winit::event::DeviceId) -> winit::event::WindowEvent {
        use winit::event::{ElementState, MouseScrollDelta, WindowEvent};
        match self {
            Self::Move(position) => WindowEvent::CursorMoved {
                device_id,
                position,
            },
            Self::Press(button) => WindowEvent::MouseInput {
                device_id,
                state: ElementState::Pressed,
                button,
            },
            Self::Release(button) => WindowEvent::MouseInput {
                device_id,
                state: ElementState::Released,
                button,
            },
            Self::Scroll { delta, phase } => WindowEvent::MouseWheel {
                device_id,
                delta: MouseScrollDelta::PixelDelta(delta),
                phase,
            },
            Self::Pinch { delta, phase } => WindowEvent::PinchGesture {
                device_id,
                delta,
                phase,
            },
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ActiveTouch {
    id: u64,
    start: winit::dpi::PhysicalPosition<f64>,
    last: winit::dpi::PhysicalPosition<f64>,
    started_at: crate::time::Instant,
    /// Travelled past the tap's slop: scrolling, or dragging when `drag`.
    scrolling: bool,
    long_pressed: bool,
    /// A drag of this contact is a pointer drag ([`TouchClaim::Drag`]).
    drag: bool,
    /// Whether it drags is read from its direction at the slop
    /// ([`TouchClaim::DragAcross`]).
    across: bool,
    /// A hold picks up what is under the finger ([`TouchClaim::HoldDrag`]).
    hold_drag: bool,
    /// The hold has picked it up: the left button is down where it began.
    lifted: bool,
}

/// Two or more fingers on ordinary content: pinch zooms, a joint drag pans.
#[derive(Debug, Clone, Copy, Default)]
struct MultiTouch {
    pinching: bool,
    panning: bool,
    scale: f64,
}

/// Converts direct touch into the toolkit's ordinary pointer contract.
///
/// * One finger: a tap is a left click, a drag scrolls, and a still 500 ms
///   hold is a secondary click (the context menu). Where the widget moves
///   things by dragging ([`TouchClaim::Drag`]) a drag is a left-button drag
///   instead, pressed where the finger landed once it passes the slop. Where
///   a held item can be moved ([`TouchClaim::HoldDrag`]) the hold presses
///   the left button and a drag after it is a left-button drag. Press is deliberately
///   delayed until touch-end: many controls activate on pointer-down, so
///   emitting it earlier would toggle a checkbox before a scroll or edge
///   swipe had enough movement to cancel the tap.
/// * Two fingers: [`TouchGestureArena`](crate::gesture::TouchGestureArena)
///   arbitrates a pinch (`PinchGesture`, anchored at the fingers' centre)
///   from a pan (pixel scroll), so every pinch-aware view zooms on a
///   touchscreen exactly as on a touchpad.
/// * A contact that begins on a widget claiming raw touch
///   ([`Widget::claims_raw_touch`](crate::widget::Widget::claims_raw_touch))
///   produces no pointer events at all and never cancels, or is cancelled by,
///   another finger; that widget handles its `WindowEvent::Touch` stream
///   itself.
#[derive(Default)]
pub struct TouchPointerEmulator {
    active: Option<ActiveTouch>,
    multi: Option<MultiTouch>,
    arena: crate::gesture::TouchGestureArena,
    raw: std::collections::HashSet<u64>,
    device_id: Option<winit::event::DeviceId>,
}

impl TouchPointerEmulator {
    const TAP_SLOP: f64 = 16.0;
    pub const LONG_PRESS: crate::time::Duration = crate::time::Duration::from_millis(500);
    const CANCEL_POINT: winit::dpi::PhysicalPosition<f64> =
        winit::dpi::PhysicalPosition::new(-1.0, -1.0);

    /// Translate one contact update. `claims_raw` is asked only when a
    /// contact begins, with its physical position.
    pub fn events(
        &mut self,
        touch: &winit::event::Touch,
        claims_raw: impl FnOnce(winit::dpi::PhysicalPosition<f64>) -> bool,
    ) -> Vec<TouchPointerEvent> {
        self.events_claimed(touch, |at| {
            if claims_raw(at) {
                TouchClaim::Raw
            } else {
                TouchClaim::Pointer
            }
        })
    }

    /// Translate one contact update. `claim` is asked only when a contact
    /// begins, with its physical position.
    pub fn events_claimed(
        &mut self,
        touch: &winit::event::Touch,
        claim: impl FnOnce(winit::dpi::PhysicalPosition<f64>) -> TouchClaim,
    ) -> Vec<TouchPointerEvent> {
        use crate::gesture::GesturePhase;
        use winit::event::TouchPhase;
        self.device_id = Some(touch.device_id);
        let claim = if touch.phase == TouchPhase::Started {
            claim(touch.location)
        } else {
            TouchClaim::Pointer
        };
        if claim == TouchClaim::Raw {
            self.raw.insert(touch.id);
            return Vec::new();
        }
        if self.raw.contains(&touch.id) {
            if matches!(touch.phase, TouchPhase::Ended | TouchPhase::Cancelled) {
                self.raw.remove(&touch.id);
            }
            return Vec::new();
        }
        let point = vello::kurbo::Point::new(touch.location.x, touch.location.y);
        let phase = match touch.phase {
            TouchPhase::Started => GesturePhase::Begin,
            TouchPhase::Moved => GesturePhase::Update,
            TouchPhase::Ended => GesturePhase::End,
            TouchPhase::Cancelled => GesturePhase::Cancel,
        };
        let gestures = self.arena.on_touch(touch.id, point, phase);
        if touch.phase == TouchPhase::Started {
            return self.begin(touch, claim);
        }
        if let Some(multi) = self.multi {
            return self.multi_touch(multi, touch.phase, &gestures);
        }
        self.single_touch(touch)
    }

    fn begin(&mut self, touch: &winit::event::Touch, claim: TouchClaim) -> Vec<TouchPointerEvent> {
        if self.multi.is_some() {
            return Vec::new();
        }
        let Some(first) = self.active.take() else {
            self.active = Some(ActiveTouch {
                id: touch.id,
                start: touch.location,
                last: touch.location,
                started_at: crate::time::now(),
                scrolling: false,
                long_pressed: false,
                drag: claim == TouchClaim::Drag,
                across: claim == TouchClaim::DragAcross,
                hold_drag: claim == TouchClaim::HoldDrag,
                lifted: false,
            });
            return Vec::new();
        };
        // A second finger turns the contact into a two-finger gesture; the
        // first finger's pending tap, hold or scroll is withdrawn.
        self.multi = Some(MultiTouch {
            scale: 1.0,
            ..MultiTouch::default()
        });
        if first.long_pressed {
            Vec::new()
        } else if first.lifted || (first.scrolling && first.drag) {
            // A drag in progress ends where the finger is, as a lifted
            // button would.
            Self::drag_end(first.last)
        } else if first.scrolling {
            vec![TouchPointerEvent::Scroll {
                delta: winit::dpi::PhysicalPosition::new(0.0, 0.0),
                phase: winit::event::TouchPhase::Cancelled,
            }]
        } else {
            vec![TouchPointerEvent::Move(Self::CANCEL_POINT)]
        }
    }

    fn multi_touch(
        &mut self,
        mut multi: MultiTouch,
        phase: winit::event::TouchPhase,
        gestures: &[crate::gesture::TouchGestureEvent],
    ) -> Vec<TouchPointerEvent> {
        use crate::gesture::TouchGestureEvent;
        use winit::event::TouchPhase;
        let mut out = Vec::new();
        match phase {
            TouchPhase::Moved => {
                for gesture in gestures {
                    match *gesture {
                        TouchGestureEvent::Pinch { center, scale, .. } if scale > 0.0 => {
                            out.push(TouchPointerEvent::Move(winit::dpi::PhysicalPosition::new(
                                center.x, center.y,
                            )));
                            out.push(TouchPointerEvent::Pinch {
                                delta: scale / multi.scale - 1.0,
                                phase: if multi.pinching {
                                    TouchPhase::Moved
                                } else {
                                    TouchPhase::Started
                                },
                            });
                            multi.pinching = true;
                            multi.scale = scale;
                        }
                        TouchGestureEvent::Pan { fingers, delta }
                            if fingers >= 2 && !multi.pinching =>
                        {
                            if let Some(center) = self.arena.center() {
                                out.push(TouchPointerEvent::Move(
                                    winit::dpi::PhysicalPosition::new(center.x, center.y),
                                ));
                            }
                            out.push(TouchPointerEvent::Scroll {
                                delta: winit::dpi::PhysicalPosition::new(delta.0, delta.1),
                                phase: TouchPhase::Moved,
                            });
                            multi.panning = true;
                        }
                        _ => {}
                    }
                }
                self.multi = Some(multi);
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                // The first lift ends the two-finger gesture; the remaining
                // finger stays inert until every contact is up.
                if multi.pinching {
                    out.push(TouchPointerEvent::Pinch { delta: 0.0, phase });
                } else if multi.panning {
                    out.push(TouchPointerEvent::Scroll {
                        delta: winit::dpi::PhysicalPosition::new(0.0, 0.0),
                        phase,
                    });
                }
                if multi.pinching || multi.panning {
                    out.push(TouchPointerEvent::Move(Self::CANCEL_POINT));
                }
                self.multi = (self.arena.contact_count() > 0).then_some(MultiTouch {
                    pinching: false,
                    panning: false,
                    scale: 1.0,
                });
            }
            TouchPhase::Started => {}
        }
        out
    }

    fn single_touch(&mut self, touch: &winit::event::Touch) -> Vec<TouchPointerEvent> {
        use winit::event::TouchPhase;
        let Some(active) = self.active else {
            return Vec::new();
        };
        if active.id != touch.id {
            return Vec::new();
        }
        match touch.phase {
            TouchPhase::Started => Vec::new(),
            TouchPhase::Moved => {
                if active.long_pressed {
                    return Vec::new();
                }
                let total_x = touch.location.x - active.start.x;
                let total_y = touch.location.y - active.start.y;
                if !active.scrolling && total_x.hypot(total_y) <= Self::TAP_SLOP {
                    return Vec::new();
                }
                // Past the slop, a contact that drags only across goes the
                // way it heads: across, a drag; otherwise a scroll.
                let active = if active.across && !active.scrolling {
                    ActiveTouch {
                        drag: total_x.abs() > total_y.abs(),
                        ..active
                    }
                } else {
                    active
                };
                if active.lifted {
                    // The button went down at the hold: the item follows.
                    self.active = Some(ActiveTouch {
                        last: touch.location,
                        scrolling: true,
                        ..active
                    });
                    vec![TouchPointerEvent::Move(touch.location)]
                } else if active.drag {
                    use winit::event::MouseButton;
                    self.active = Some(ActiveTouch {
                        last: touch.location,
                        scrolling: true,
                        ..active
                    });
                    if active.scrolling {
                        vec![TouchPointerEvent::Move(touch.location)]
                    } else {
                        vec![
                            TouchPointerEvent::Move(active.start),
                            TouchPointerEvent::Press(MouseButton::Left),
                            TouchPointerEvent::Move(touch.location),
                        ]
                    }
                } else {
                    let delta = winit::dpi::PhysicalPosition::new(
                        touch.location.x - active.last.x,
                        touch.location.y - active.last.y,
                    );
                    self.active = Some(ActiveTouch {
                        last: touch.location,
                        scrolling: true,
                        ..active
                    });
                    vec![
                        TouchPointerEvent::Move(active.start),
                        TouchPointerEvent::Scroll {
                            delta,
                            phase: TouchPhase::Moved,
                        },
                    ]
                }
            }
            TouchPhase::Ended => {
                self.active = None;
                if active.long_pressed {
                    Vec::new()
                } else if active.lifted && !active.scrolling {
                    Self::put_back(touch.location)
                } else if active.lifted || (active.scrolling && active.drag) {
                    Self::drag_end(touch.location)
                } else if active.scrolling {
                    vec![TouchPointerEvent::Scroll {
                        delta: winit::dpi::PhysicalPosition::new(0.0, 0.0),
                        phase: TouchPhase::Ended,
                    }]
                } else {
                    use winit::event::MouseButton;
                    vec![
                        TouchPointerEvent::Move(touch.location),
                        TouchPointerEvent::Press(MouseButton::Left),
                        TouchPointerEvent::Release(MouseButton::Left),
                        TouchPointerEvent::Move(Self::CANCEL_POINT),
                    ]
                }
            }
            TouchPhase::Cancelled => {
                self.active = None;
                if active.long_pressed {
                    Vec::new()
                } else if active.lifted || (active.scrolling && active.drag) {
                    Self::drag_end(active.last)
                } else if active.scrolling {
                    vec![TouchPointerEvent::Scroll {
                        delta: winit::dpi::PhysicalPosition::new(0.0, 0.0),
                        phase: TouchPhase::Cancelled,
                    }]
                } else {
                    vec![TouchPointerEvent::Move(Self::CANCEL_POINT)]
                }
            }
        }
    }

    /// The end of a pointer drag at `at`: the button lifts there and the
    /// pointer leaves, as after a tap.
    fn drag_end(at: winit::dpi::PhysicalPosition<f64>) -> Vec<TouchPointerEvent> {
        vec![
            TouchPointerEvent::Move(at),
            TouchPointerEvent::Release(winit::event::MouseButton::Left),
            TouchPointerEvent::Move(Self::CANCEL_POINT),
        ]
    }

    /// An item a hold picked up, put down where it was: the hold is the
    /// secondary click there, made while the left button is still down so
    /// the widget ends the pick-up instead of reading a click, and then the
    /// button lifts and the pointer leaves.
    fn put_back(at: winit::dpi::PhysicalPosition<f64>) -> Vec<TouchPointerEvent> {
        use winit::event::MouseButton;
        vec![
            TouchPointerEvent::Move(at),
            TouchPointerEvent::Press(MouseButton::Right),
            TouchPointerEvent::Release(MouseButton::Right),
            TouchPointerEvent::Release(MouseButton::Left),
            TouchPointerEvent::Move(Self::CANCEL_POINT),
        ]
    }

    /// Whether every gesture begun has reached its target: no finger is
    /// down whose tap, drag, pinch or raw contact is still to land. A
    /// finger held into a secondary click has landed, and one that went
    /// on to scroll has no target to land on. A host keeps what is on
    /// screen laid out as it was until then, so a touch lands on what the
    /// person saw when it began.
    pub fn settled(&self) -> bool {
        self.raw.is_empty()
            && self.multi.is_none()
            && self.active.is_none_or(|active| {
                active.long_pressed || (active.scrolling && !active.drag && !active.lifted)
            })
    }

    /// When a still single-finger hold becomes a long press, if one is armed.
    pub fn long_press_deadline(&self) -> Option<crate::time::Instant> {
        self.active
            .filter(|active| !active.scrolling && !active.long_pressed && !active.lifted)
            .map(|active| active.started_at + Self::LONG_PRESS)
    }

    /// Fire a long press whose deadline has passed: a secondary click where
    /// the finger rests, after which the finger's lift does nothing; or,
    /// where a held item can be moved, the left press that picks it up.
    pub fn poll(
        &mut self,
        now: crate::time::Instant,
    ) -> Option<(winit::event::DeviceId, Vec<TouchPointerEvent>)> {
        use winit::event::MouseButton;
        let deadline = self.long_press_deadline()?;
        if now < deadline {
            return None;
        }
        let active = self.active.as_mut()?;
        let at = active.last;
        if active.hold_drag {
            active.lifted = true;
            return Some((
                self.device_id?,
                vec![
                    TouchPointerEvent::Move(at),
                    TouchPointerEvent::Press(MouseButton::Left),
                ],
            ));
        }
        active.long_pressed = true;
        Some((
            self.device_id?,
            vec![
                TouchPointerEvent::Move(at),
                TouchPointerEvent::Press(MouseButton::Right),
                TouchPointerEvent::Release(MouseButton::Right),
            ],
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modality_tracking_keeps_discovered_devices_and_hint_priority() {
        {
            let env = InteractionEnvironment::TOUCH.with_keyboard_modality();
            assert_eq!(env.modality, InputModality::Keyboard);
            assert_eq!(env.pointer_precision, PointerPrecision::Coarse);
            assert_eq!(env.minimum_target(), 48.0);
            assert!(!env.supports_hover());
            assert!(env.available.contains(InputDevice::Touch));
            assert!(env.available.contains(InputDevice::Keyboard));
        }

        {
            let env = InteractionEnvironment::POINTER
                .with_touch_modality()
                .with_controller(ControllerFamily::JoyConPair)
                .with_pointer_modality();
            assert_eq!(env.modality, InputModality::Pointer);
            assert!(env.available.contains(InputDevice::Pointer));
            assert!(env.available.contains(InputDevice::Touch));
            assert!(env.available.contains(InputDevice::Controller));
            assert_eq!(env.controller, Some(ControllerFamily::JoyConPair));
        }

        {
            let all = InteractionEnvironment::POINTER
                .with_touch_modality()
                .with_controller(ControllerFamily::Xbox);
            assert_eq!(all.preferred_hint_modality(), InputModality::Controller);
            let typed = all.with_keyboard_modality();
            assert_eq!(typed.preferred_hint_modality(), InputModality::Keyboard);
            let touched_after_typing = typed.with_touch_modality();
            assert_eq!(
                touched_after_typing.preferred_hint_modality(),
                InputModality::Touch,
                "a connected keyboard must not hide gesture help from someone using touch"
            );
            assert_eq!(
                touched_after_typing
                    .with_keyboard_modality()
                    .preferred_hint_modality(),
                InputModality::Keyboard
            );
        }

        {
            let desktop = InteractionEnvironment {
                available: InputAvailability::NONE
                    .with(InputDevice::Keyboard)
                    .with(InputDevice::Pointer),
                ..InteractionEnvironment::POINTER
            };
            assert_eq!(desktop.preferred_hint_modality(), InputModality::Keyboard);

            let touch_only = InteractionEnvironment {
                modality: InputModality::Pointer,
                available: InputAvailability::NONE.with(InputDevice::Touch),
                ..InteractionEnvironment::TOUCH
            };
            assert_eq!(touch_only.preferred_hint_modality(), InputModality::Touch);

            let mouse_only = InteractionEnvironment {
                available: InputAvailability::NONE.with(InputDevice::Pointer),
                ..InteractionEnvironment::POINTER
            };
            assert_eq!(mouse_only.preferred_hint_modality(), InputModality::Pointer);

            let nothing_known = InteractionEnvironment {
                available: InputAvailability::NONE,
                ..InteractionEnvironment::POINTER
            };
            assert_eq!(
                nothing_known.preferred_hint_modality(),
                InputModality::Pointer
            );
        }

        {
            let unplugged = InteractionEnvironment {
                modality: InputModality::Controller,
                available: InputAvailability::NONE
                    .with(InputDevice::Keyboard)
                    .with(InputDevice::Pointer),
                controller: Some(ControllerFamily::Xbox),
                ..InteractionEnvironment::POINTER
            };
            assert_eq!(unplugged.preferred_hint_modality(), InputModality::Keyboard);
        }

        {
            let available = InputAvailability::NONE
                .with(InputDevice::Touch)
                .with(InputDevice::Controller)
                .with(InputDevice::Touch);
            assert!(!available.contains(InputDevice::Keyboard));
            assert!(!available.contains(InputDevice::Pointer));
            assert!(available.contains(InputDevice::Touch));
            assert!(available.contains(InputDevice::Controller));
        }

        {
            assert_eq!(
                ControllerFamily::from_device_name("Nintendo Switch Joy-Con (L)"),
                ControllerFamily::JoyConLeft
            );
            assert_eq!(
                ControllerFamily::from_hid(Some(0x057e), "Nintendo Switch Left Joy-Con"),
                ControllerFamily::JoyConLeft
            );
            assert_eq!(
                ControllerFamily::from_hid(Some(0x057e), "Nintendo Switch Right Joy-Con (Grip)"),
                ControllerFamily::JoyConRight
            );
            assert_eq!(
                ControllerFamily::from_hid(Some(0x057e), "Nintendo Switch Combined Joy-Cons"),
                ControllerFamily::JoyConPair
            );
            assert_eq!(
                ControllerFamily::from_device_name("Sony DualSense Wireless Controller"),
                ControllerFamily::PlayStation
            );
            assert_eq!(
                ControllerFamily::from_hid(Some(0x054c), "Wireless Controller"),
                ControllerFamily::PlayStation
            );
            assert_eq!(
                ControllerFamily::from_device_name("Wireless Controller"),
                ControllerFamily::Generic
            );
            assert_eq!(
                KeyboardFamily::from_device_name("Apple Inc. Magic Keyboard"),
                KeyboardFamily::Apple
            );
        }
    }

    fn touch(phase: winit::event::TouchPhase, id: u64, x: f64, y: f64) -> winit::event::Touch {
        winit::event::Touch {
            device_id: winit::event::DeviceId::dummy(),
            phase,
            location: winit::dpi::PhysicalPosition::new(x, y),
            force: None,
            id,
        }
    }

    fn feed(
        emulator: &mut TouchPointerEmulator,
        phase: winit::event::TouchPhase,
        id: u64,
        x: f64,
        y: f64,
    ) -> Vec<TouchPointerEvent> {
        emulator.events(&touch(phase, id, x, y), |_| false)
    }

    #[test]
    fn a_tap_maps_to_press_and_release_only_after_touch_end() {
        use TouchPointerEvent::{Move, Press, Release};
        use winit::event::{MouseButton, TouchPhase};
        let mut emulator = TouchPointerEmulator::default();
        assert!(feed(&mut emulator, TouchPhase::Started, 7, 10.0, 20.0).is_empty());
        assert_eq!(
            feed(&mut emulator, TouchPhase::Ended, 7, 11.0, 20.0),
            vec![
                Move((11.0, 20.0).into()),
                Press(MouseButton::Left),
                Release(MouseButton::Left),
                Move((-1.0, -1.0).into()),
            ]
        );
    }

    #[test]
    fn movement_scrolls_and_lift_ends_the_scroll() {
        use TouchPointerEvent::{Move, Scroll};
        use winit::event::TouchPhase;
        let mut emulator = TouchPointerEmulator::default();
        assert!(feed(&mut emulator, TouchPhase::Started, 1, 40.0, 40.0).is_empty());
        assert_eq!(
            feed(&mut emulator, TouchPhase::Moved, 1, 80.0, 40.0),
            vec![
                Move((40.0, 40.0).into()),
                Scroll {
                    delta: (40.0, 0.0).into(),
                    phase: TouchPhase::Moved,
                }
            ]
        );
        assert_eq!(
            feed(&mut emulator, TouchPhase::Ended, 1, 80.0, 40.0),
            vec![Scroll {
                delta: (0.0, 0.0).into(),
                phase: TouchPhase::Ended,
            }]
        );
        assert_eq!(emulator.long_press_deadline(), None);
    }

    #[test]
    fn a_second_finger_withdraws_the_first_fingers_tap() {
        use winit::event::TouchPhase;
        let mut emulator = TouchPointerEmulator::default();
        assert!(feed(&mut emulator, TouchPhase::Started, 2, 5.0, 5.0).is_empty());
        assert_eq!(
            feed(&mut emulator, TouchPhase::Started, 3, 80.0, 5.0),
            vec![TouchPointerEvent::Move((-1.0, -1.0).into())]
        );
        assert!(feed(&mut emulator, TouchPhase::Ended, 2, 5.0, 5.0).is_empty());
        assert!(
            feed(&mut emulator, TouchPhase::Ended, 3, 80.0, 5.0).is_empty(),
            "no finger of a withdrawn two-finger contact clicks"
        );
    }

    #[test]
    fn spreading_two_fingers_pinches_about_their_centre() {
        use winit::event::TouchPhase;
        let mut emulator = TouchPointerEmulator::default();
        feed(&mut emulator, TouchPhase::Started, 1, 100.0, 100.0);
        feed(&mut emulator, TouchPhase::Started, 2, 200.0, 100.0);
        let mut scale = 1.0;
        let mut phases = Vec::new();
        for x in [220.0, 240.0, 260.0] {
            for event in feed(&mut emulator, TouchPhase::Moved, 2, x, 100.0) {
                if let TouchPointerEvent::Pinch { delta, phase } = event {
                    scale *= 1.0 + delta;
                    phases.push(phase);
                }
            }
        }
        assert!((scale - 1.6).abs() < 1e-9, "{scale}");
        assert_eq!(phases.first(), Some(&TouchPhase::Started));
        let end = feed(&mut emulator, TouchPhase::Ended, 2, 260.0, 100.0);
        assert!(end.contains(&TouchPointerEvent::Pinch {
            delta: 0.0,
            phase: TouchPhase::Ended,
        }));
        assert!(feed(&mut emulator, TouchPhase::Ended, 1, 100.0, 100.0).is_empty());
    }

    #[test]
    fn two_fingers_moving_together_scroll() {
        use winit::event::TouchPhase;
        let mut emulator = TouchPointerEmulator::default();
        feed(&mut emulator, TouchPhase::Started, 1, 100.0, 300.0);
        feed(&mut emulator, TouchPhase::Started, 2, 200.0, 300.0);
        feed(&mut emulator, TouchPhase::Moved, 1, 100.0, 260.0);
        let events = feed(&mut emulator, TouchPhase::Moved, 2, 200.0, 260.0);
        assert!(events.contains(&TouchPointerEvent::Scroll {
            delta: (0.0, -20.0).into(),
            phase: TouchPhase::Moved,
        }));
        assert!(
            feed(&mut emulator, TouchPhase::Ended, 1, 100.0, 260.0).contains(
                &TouchPointerEvent::Scroll {
                    delta: (0.0, 0.0).into(),
                    phase: TouchPhase::Ended,
                }
            )
        );
    }

    #[test]
    fn a_still_hold_becomes_a_secondary_click_and_its_lift_is_inert() {
        use TouchPointerEvent::{Move, Press, Release};
        use winit::event::{MouseButton, TouchPhase};
        let mut emulator = TouchPointerEmulator::default();
        feed(&mut emulator, TouchPhase::Started, 4, 50.0, 60.0);
        feed(&mut emulator, TouchPhase::Moved, 4, 54.0, 60.0);
        let deadline = emulator.long_press_deadline().expect("armed");
        let early = deadline
            .checked_sub(crate::time::Duration::from_millis(1))
            .expect("a deadline in the future");
        assert!(emulator.poll(early).is_none(), "not before the deadline");
        let (_, events) = emulator
            .poll(deadline)
            .expect("the hold fires at its deadline");
        assert_eq!(
            events,
            vec![
                Move((50.0, 60.0).into()),
                Press(MouseButton::Right),
                Release(MouseButton::Right),
            ]
        );
        assert!(emulator.poll(deadline).is_none(), "fires once");
        assert!(feed(&mut emulator, TouchPhase::Moved, 4, 90.0, 60.0).is_empty());
        assert!(feed(&mut emulator, TouchPhase::Ended, 4, 90.0, 60.0).is_empty());
    }

    fn feed_drag(
        emulator: &mut TouchPointerEmulator,
        phase: winit::event::TouchPhase,
        id: u64,
        x: f64,
        y: f64,
    ) -> Vec<TouchPointerEvent> {
        emulator.events_claimed(&touch(phase, id, x, y), |_| TouchClaim::Drag)
    }

    #[test]
    fn where_a_drag_moves_things_a_finger_drags_taps_and_holds() {
        use TouchPointerEvent::{Move, Press, Release};
        use winit::event::{MouseButton, TouchPhase};
        let gone = Move((-1.0, -1.0).into());
        {
            let mut emulator = TouchPointerEmulator::default();
            assert!(feed_drag(&mut emulator, TouchPhase::Started, 1, 50.0, 60.0).is_empty());
            assert!(
                feed_drag(&mut emulator, TouchPhase::Moved, 1, 54.0, 60.0).is_empty(),
                "within the slop nothing moves yet"
            );
            assert_eq!(
                feed_drag(&mut emulator, TouchPhase::Moved, 1, 90.0, 60.0),
                vec![
                    Move((50.0, 60.0).into()),
                    Press(MouseButton::Left),
                    Move((90.0, 60.0).into())
                ]
            );
            assert_eq!(emulator.long_press_deadline(), None, "a drag is no hold");
            assert_eq!(
                feed_drag(&mut emulator, TouchPhase::Moved, 1, 120.0, 70.0),
                vec![Move((120.0, 70.0).into())]
            );
            assert_eq!(
                feed_drag(&mut emulator, TouchPhase::Ended, 1, 121.0, 70.0),
                vec![Move((121.0, 70.0).into()), Release(MouseButton::Left), gone]
            );
        }
        {
            let mut emulator = TouchPointerEmulator::default();
            feed_drag(&mut emulator, TouchPhase::Started, 2, 10.0, 20.0);
            assert_eq!(
                feed_drag(&mut emulator, TouchPhase::Ended, 2, 11.0, 20.0),
                vec![
                    Move((11.0, 20.0).into()),
                    Press(MouseButton::Left),
                    Release(MouseButton::Left),
                    gone
                ]
            );
        }
        {
            let mut emulator = TouchPointerEmulator::default();
            feed_drag(&mut emulator, TouchPhase::Started, 3, 50.0, 60.0);
            let deadline = emulator.long_press_deadline().expect("armed");
            let (_, events) = emulator.poll(deadline).expect("the hold fires");
            assert_eq!(
                events,
                vec![
                    Move((50.0, 60.0).into()),
                    Press(MouseButton::Right),
                    Release(MouseButton::Right)
                ]
            );
            assert!(feed_drag(&mut emulator, TouchPhase::Moved, 3, 90.0, 60.0).is_empty());
            assert!(feed_drag(&mut emulator, TouchPhase::Ended, 3, 90.0, 60.0).is_empty());
        }
        {
            let mut emulator = TouchPointerEmulator::default();
            feed_drag(&mut emulator, TouchPhase::Started, 4, 50.0, 60.0);
            feed_drag(&mut emulator, TouchPhase::Moved, 4, 90.0, 60.0);
            assert_eq!(
                feed_drag(&mut emulator, TouchPhase::Started, 5, 200.0, 60.0),
                vec![Move((90.0, 60.0).into()), Release(MouseButton::Left), gone]
            );
        }
    }

    /// Where a drag moves things only across (a switch's knob), the
    /// direction at the slop decides: across drags, up or down scrolls.
    #[test]
    fn where_a_drag_goes_only_across_a_vertical_swipe_still_scrolls() {
        use TouchPointerEvent::{Move, Press, Release, Scroll};
        use winit::event::{MouseButton, TouchPhase};
        let feed_across = |emulator: &mut TouchPointerEmulator, phase, x: f64, y: f64| {
            emulator.events_claimed(&touch(phase, 1, x, y), |_| TouchClaim::DragAcross)
        };
        let gone = Move((-1.0, -1.0).into());
        let mut emulator = TouchPointerEmulator::default();
        assert!(feed_across(&mut emulator, TouchPhase::Started, 50.0, 60.0).is_empty());
        assert!(
            feed_across(&mut emulator, TouchPhase::Moved, 54.0, 66.0).is_empty(),
            "within the slop nothing is decided"
        );
        assert_eq!(
            feed_across(&mut emulator, TouchPhase::Moved, 58.0, 90.0),
            vec![
                Move((50.0, 60.0).into()),
                Scroll {
                    delta: (8.0, 30.0).into(),
                    phase: TouchPhase::Moved
                }
            ],
            "mostly down: a scroll"
        );
        assert_eq!(
            feed_across(&mut emulator, TouchPhase::Moved, 100.0, 92.0),
            vec![
                Move((50.0, 60.0).into()),
                Scroll {
                    delta: (42.0, 2.0).into(),
                    phase: TouchPhase::Moved
                }
            ],
            "once scrolling, it stays a scroll whichever way it turns"
        );
        assert_eq!(
            feed_across(&mut emulator, TouchPhase::Ended, 100.0, 92.0),
            vec![Scroll {
                delta: (0.0, 0.0).into(),
                phase: TouchPhase::Ended
            }]
        );

        assert!(feed_across(&mut emulator, TouchPhase::Started, 50.0, 60.0).is_empty());
        assert_eq!(
            feed_across(&mut emulator, TouchPhase::Moved, 70.0, 66.0),
            vec![
                Move((50.0, 60.0).into()),
                Press(MouseButton::Left),
                Move((70.0, 66.0).into())
            ],
            "mostly across: a drag from where the finger landed"
        );
        assert_eq!(
            feed_across(&mut emulator, TouchPhase::Moved, 72.0, 120.0),
            vec![Move((72.0, 120.0).into())],
            "once dragging, it stays a drag"
        );
        assert_eq!(
            feed_across(&mut emulator, TouchPhase::Ended, 72.0, 120.0),
            vec![Move((72.0, 120.0).into()), Release(MouseButton::Left), gone]
        );

        assert!(feed_across(&mut emulator, TouchPhase::Started, 10.0, 20.0).is_empty());
        assert_eq!(
            feed_across(&mut emulator, TouchPhase::Ended, 11.0, 21.0),
            vec![
                Move((11.0, 21.0).into()),
                Press(MouseButton::Left),
                Release(MouseButton::Left),
                gone
            ],
            "a tap clicks"
        );
    }

    #[test]
    fn a_held_item_is_picked_up_moved_and_put_back() {
        use TouchPointerEvent::{Move, Press, Release, Scroll};
        use winit::event::{MouseButton, TouchPhase};
        let gone = Move((-1.0, -1.0).into());
        let feed_held = |emulator: &mut TouchPointerEmulator, phase, id, x, y| {
            emulator.events_claimed(&touch(phase, id, x, y), |_| TouchClaim::HoldDrag)
        };
        {
            let mut emulator = TouchPointerEmulator::default();
            assert!(feed_held(&mut emulator, TouchPhase::Started, 1, 50.0, 60.0).is_empty());
            let deadline = emulator.long_press_deadline().expect("armed");
            let (_, events) = emulator.poll(deadline).expect("the hold picks it up");
            assert_eq!(
                events,
                vec![Move((50.0, 60.0).into()), Press(MouseButton::Left)]
            );
            assert!(emulator.poll(deadline).is_none(), "picked up once");
            assert!(
                feed_held(&mut emulator, TouchPhase::Moved, 1, 52.0, 60.0).is_empty(),
                "within the slop it stays put"
            );
            assert_eq!(
                feed_held(&mut emulator, TouchPhase::Moved, 1, 50.0, 120.0),
                vec![Move((50.0, 120.0).into())]
            );
            assert_eq!(
                feed_held(&mut emulator, TouchPhase::Ended, 1, 50.0, 130.0),
                vec![Move((50.0, 130.0).into()), Release(MouseButton::Left), gone]
            );
        }
        {
            let mut emulator = TouchPointerEmulator::default();
            feed_held(&mut emulator, TouchPhase::Started, 2, 50.0, 60.0);
            let deadline = emulator.long_press_deadline().expect("armed");
            emulator.poll(deadline).expect("the hold picks it up");
            assert_eq!(
                feed_held(&mut emulator, TouchPhase::Ended, 2, 51.0, 60.0),
                vec![
                    Move((51.0, 60.0).into()),
                    Press(MouseButton::Right),
                    Release(MouseButton::Right),
                    Release(MouseButton::Left),
                    gone
                ]
            );
        }
        {
            let mut emulator = TouchPointerEmulator::default();
            feed_held(&mut emulator, TouchPhase::Started, 3, 50.0, 60.0);
            let events = feed_held(&mut emulator, TouchPhase::Moved, 3, 50.0, 100.0);
            assert!(
                matches!(events.as_slice(), [Move(_), Scroll { .. }]),
                "{events:?}"
            );
            assert_eq!(emulator.long_press_deadline(), None, "a swipe is no hold");
        }
        {
            let mut emulator = TouchPointerEmulator::default();
            feed_held(&mut emulator, TouchPhase::Started, 4, 50.0, 60.0);
            let deadline = emulator.long_press_deadline().expect("armed");
            emulator.poll(deadline).expect("the hold picks it up");
            assert_eq!(
                feed_held(&mut emulator, TouchPhase::Started, 5, 200.0, 60.0),
                vec![Move((50.0, 60.0).into()), Release(MouseButton::Left), gone]
            );
        }
    }

    #[test]
    fn moving_past_the_slop_disarms_the_hold() {
        use winit::event::TouchPhase;
        let mut emulator = TouchPointerEmulator::default();
        feed(&mut emulator, TouchPhase::Started, 5, 50.0, 60.0);
        feed(&mut emulator, TouchPhase::Moved, 5, 50.0, 100.0);
        assert_eq!(emulator.long_press_deadline(), None);
    }

    #[test]
    fn raw_touch_contacts_overlap_without_cancelling_each_other() {
        use winit::event::TouchPhase;
        let mut emulator = TouchPointerEmulator::default();
        for (phase, id, x) in [
            (TouchPhase::Started, 1, 10.0),
            (TouchPhase::Started, 2, 60.0),
            (TouchPhase::Ended, 1, 10.0),
            (TouchPhase::Ended, 2, 60.0),
        ] {
            assert!(
                emulator
                    .events(&touch(phase, id, x, 10.0), |_| true)
                    .is_empty()
            );
        }
        assert!(emulator.raw.is_empty());
        // A raw contact does not turn an ordinary finger into a two-finger
        // gesture: the ordinary tap still clicks.
        emulator.events(&touch(TouchPhase::Started, 3, 10.0, 10.0), |_| true);
        assert!(feed(&mut emulator, TouchPhase::Started, 4, 300.0, 300.0).is_empty());
        assert_eq!(
            feed(&mut emulator, TouchPhase::Ended, 4, 300.0, 300.0).len(),
            4,
            "move, press, release, leave"
        );
    }

    #[test]
    fn a_gesture_settles_once_it_has_reached_its_target() {
        use winit::event::TouchPhase;
        let mut emulator = TouchPointerEmulator::default();
        assert!(emulator.settled(), "no finger down");

        feed(&mut emulator, TouchPhase::Started, 1, 10.0, 10.0);
        assert!(!emulator.settled(), "a tap lands at the lift");
        feed(&mut emulator, TouchPhase::Ended, 1, 10.0, 10.0);
        assert!(emulator.settled());

        feed(&mut emulator, TouchPhase::Started, 2, 10.0, 10.0);
        let deadline = emulator.long_press_deadline().expect("armed");
        emulator.poll(deadline).expect("the hold fires");
        assert!(
            emulator.settled(),
            "a hold has landed its secondary click; the lift does nothing"
        );
        feed(&mut emulator, TouchPhase::Ended, 2, 10.0, 10.0);

        feed(&mut emulator, TouchPhase::Started, 6, 10.0, 100.0);
        feed(&mut emulator, TouchPhase::Moved, 6, 10.0, 90.0);
        assert!(!emulator.settled(), "within the slop it may still tap");
        feed(&mut emulator, TouchPhase::Moved, 6, 10.0, 60.0);
        assert!(emulator.settled(), "a scroll has no target to land on");
        feed(&mut emulator, TouchPhase::Ended, 6, 10.0, 60.0);

        emulator.events_claimed(&touch(TouchPhase::Started, 7, 10.0, 100.0), |_| {
            TouchClaim::Drag
        });
        emulator.events_claimed(&touch(TouchPhase::Moved, 7, 60.0, 100.0), |_| {
            TouchClaim::Drag
        });
        assert!(!emulator.settled(), "a drag keeps what it picked up");
        emulator.events_claimed(&touch(TouchPhase::Ended, 7, 60.0, 100.0), |_| {
            TouchClaim::Drag
        });
        assert!(emulator.settled());

        feed(&mut emulator, TouchPhase::Started, 3, 100.0, 100.0);
        feed(&mut emulator, TouchPhase::Started, 4, 200.0, 100.0);
        feed(&mut emulator, TouchPhase::Ended, 4, 200.0, 100.0);
        assert!(!emulator.settled(), "one finger of a pinch is still down");
        feed(&mut emulator, TouchPhase::Ended, 3, 100.0, 100.0);
        assert!(emulator.settled());

        emulator.events(&touch(TouchPhase::Started, 5, 10.0, 10.0), |_| true);
        assert!(!emulator.settled(), "a raw contact is read until it lifts");
        emulator.events(&touch(TouchPhase::Ended, 5, 10.0, 10.0), |_| true);
        assert!(emulator.settled());
    }

    #[test]
    fn android_back_key_dispatches_as_escape_in_both_phases() {
        use winit::event::{ElementState, WindowEvent};
        use winit::keyboard::{Key, NamedKey};
        for state in [ElementState::Pressed, ElementState::Released] {
            let back = crate::synthetic_input::key_input(&Key::Named(NamedKey::BrowserBack), state);
            let Some(WindowEvent::KeyboardInput { event, .. }) = back_key_as_escape(&back) else {
                panic!("Back must become a key event");
            };
            assert_eq!(event.logical_key, Key::Named(NamedKey::Escape));
            assert_eq!(event.state, state);
        }
        let escape =
            crate::synthetic_input::key_input(&Key::Named(NamedKey::Escape), ElementState::Pressed);
        assert!(back_key_as_escape(&escape).is_none());
    }
}
