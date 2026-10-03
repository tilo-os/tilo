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

    pub fn horizontal_target_rect(self, visual: Rect) -> Rect {
        let height = visual.height().max(f64::from(self.minimum_target()));
        Rect::from_center_size(visual.center(), (visual.width(), height))
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

    /// Override the legends without changing the current modality. Hardware
    /// discovery uses this for Apple USB/Bluetooth keyboards on non-Mac hosts.
    #[must_use]
    pub const fn with_keyboard_family(self, family: KeyboardFamily) -> Self {
        Self {
            keyboard_family: family,
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
            Some(crate::synthetic_input::key_event(
                &Key::Named(NamedKey::Escape),
                key.state,
            ))
        }
        _ => None,
    }
}

/// Pointer events synthesised from direct touch, so ordinary widgets work on a
/// touchscreen without interpreting contact streams themselves.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum TouchPointerEvent {
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
    pub(crate) fn window_event(
        self,
        device_id: winit::event::DeviceId,
    ) -> winit::event::WindowEvent {
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
    scrolling: bool,
    long_pressed: bool,
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
///   hold is a secondary click (the context menu). Press is deliberately
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
pub(crate) struct TouchPointerEmulator {
    active: Option<ActiveTouch>,
    multi: Option<MultiTouch>,
    arena: crate::gesture::TouchGestureArena,
    raw: std::collections::HashSet<u64>,
    device_id: Option<winit::event::DeviceId>,
}

impl TouchPointerEmulator {
    const TAP_SLOP: f64 = 16.0;
    pub(crate) const LONG_PRESS: crate::time::Duration = crate::time::Duration::from_millis(500);
    const CANCEL_POINT: winit::dpi::PhysicalPosition<f64> =
        winit::dpi::PhysicalPosition::new(-1.0, -1.0);

    /// Translate one contact update. `claims_raw` is asked only when a
    /// contact begins, with its physical position.
    pub(crate) fn events(
        &mut self,
        touch: &winit::event::Touch,
        claims_raw: impl FnOnce(winit::dpi::PhysicalPosition<f64>) -> bool,
    ) -> Vec<TouchPointerEvent> {
        use crate::gesture::GesturePhase;
        use winit::event::TouchPhase;
        self.device_id = Some(touch.device_id);
        if touch.phase == TouchPhase::Started && claims_raw(touch.location) {
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
            return self.begin(touch);
        }
        if let Some(multi) = self.multi {
            return self.multi_touch(multi, touch.phase, &gestures);
        }
        self.single_touch(touch)
    }

    fn begin(&mut self, touch: &winit::event::Touch) -> Vec<TouchPointerEvent> {
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
                    Vec::new()
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

    /// When a still single-finger hold becomes a long press, if one is armed.
    pub(crate) fn long_press_deadline(&self) -> Option<crate::time::Instant> {
        self.active
            .filter(|active| !active.scrolling && !active.long_pressed)
            .map(|active| active.started_at + Self::LONG_PRESS)
    }

    /// Fire a long press whose deadline has passed: a secondary click where
    /// the finger rests. The finger's eventual lift then does nothing.
    pub(crate) fn poll(
        &mut self,
        now: crate::time::Instant,
    ) -> Option<(winit::event::DeviceId, Vec<TouchPointerEvent>)> {
        use winit::event::MouseButton;
        let deadline = self.long_press_deadline()?;
        if now < deadline {
            return None;
        }
        let active = self.active.as_mut()?;
        active.long_pressed = true;
        let at = active.last;
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
        // Case `keyboard_after_touch_preserves_coarse_pointer_capability`.
        {
            let env = InteractionEnvironment::TOUCH.with_keyboard_modality();
            assert_eq!(env.modality, InputModality::Keyboard);
            assert_eq!(env.pointer_precision, PointerPrecision::Coarse);
            assert_eq!(env.minimum_target(), 48.0);
            assert!(!env.supports_hover());
            assert!(env.available.contains(InputDevice::Touch));
            assert!(env.available.contains(InputDevice::Keyboard));
        }

        // Case `modality_changes_do_not_erase_discovered_devices`.
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

        // Case `the_last_used_input_leads_shortcut_help`.
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

        // Case `pointer_use_defers_to_keyboard_or_touch_by_available_devices`.
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

        // Case `a_disconnected_controller_no_longer_leads_help`.
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

        // Case `availability_is_an_idempotent_set_of_independent_devices`.
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

        // Case `hid_names_select_legends_without_affecting_semantic_positions`.
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
    fn android_back_key_dispatches_as_escape_in_both_phases() {
        use winit::event::{ElementState, WindowEvent};
        use winit::keyboard::{Key, NamedKey};
        for state in [ElementState::Pressed, ElementState::Released] {
            let back = crate::synthetic_input::key_event(&Key::Named(NamedKey::BrowserBack), state);
            let Some(WindowEvent::KeyboardInput { event, .. }) = back_key_as_escape(&back) else {
                panic!("Back must become a key event");
            };
            assert_eq!(event.logical_key, Key::Named(NamedKey::Escape));
            assert_eq!(event.state, state);
        }
        let escape =
            crate::synthetic_input::key_event(&Key::Named(NamedKey::Escape), ElementState::Pressed);
        assert!(back_key_as_escape(&escape).is_none());
    }
}
