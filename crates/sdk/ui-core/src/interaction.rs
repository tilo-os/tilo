//! Host-owned input capabilities shared by the retained widget tree.
//!
//! Apps describe semantic controls once. The host updates this environment
//! from platform events, and controls adapt their hit targets and hover-only
//! affordances without app code branching on raw `winit` events.

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
    /// letters incorrectly.
    pub fn from_device_name(name: &str) -> Self {
        let name = name.to_ascii_lowercase();
        if name.contains("joy-con (l)") || name.contains("joycon left") {
            Self::JoyConLeft
        } else if name.contains("joy-con (r)") || name.contains("joycon right") {
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

/// Physical legends printed on the connected keyboard. qOS may run on Linux
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
#[allow(clippy::struct_excessive_bools)]
pub struct InputAvailability {
    pub keyboard: bool,
    pub pointer: bool,
    pub touch: bool,
    pub controller: bool,
}

impl Default for InputAvailability {
    fn default() -> Self {
        Self {
            keyboard: true,
            pointer: true,
            touch: false,
            controller: false,
        }
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
        available: InputAvailability {
            keyboard: true,
            pointer: true,
            touch: false,
            controller: false,
        },
        controller: None,
        keyboard_family: KeyboardFamily::Pc,
    };

    pub const TOUCH: Self = Self {
        modality: InputModality::Touch,
        pointer_precision: PointerPrecision::Coarse,
        available: InputAvailability {
            keyboard: true,
            pointer: false,
            touch: true,
            controller: false,
        },
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

    /// Hover affordances have no meaning for a coarse direct pointer.
    pub const fn supports_hover(self) -> bool {
        matches!(self.pointer_precision, PointerPrecision::Fine)
    }

    /// Input language to lead with in shortcut help. A controller takes over
    /// only after the person actually used it; otherwise a physical keyboard
    /// is more efficient than touch, with touch as the no-keyboard fallback.
    /// Pointer instructions accompany keyboard hints rather than becoming a
    /// separate primary language.
    pub const fn preferred_hint_modality(self) -> InputModality {
        if matches!(self.modality, InputModality::Controller) && self.available.controller {
            InputModality::Controller
        } else if self.available.keyboard {
            InputModality::Keyboard
        } else if self.available.touch {
            InputModality::Touch
        } else {
            InputModality::Pointer
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
            available: InputAvailability {
                keyboard: true,
                ..self.available
            },
            ..self
        }
    }

    /// Record pointer use while preserving every previously discovered device.
    #[must_use]
    pub const fn with_pointer_modality(self) -> Self {
        Self {
            modality: InputModality::Pointer,
            pointer_precision: PointerPrecision::Fine,
            available: InputAvailability {
                pointer: true,
                ..self.available
            },
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
            available: InputAvailability {
                touch: true,
                ..self.available
            },
            ..self
        }
    }

    /// Record a controller event and retain its physical legend family.
    #[must_use]
    pub const fn with_controller(self, family: ControllerFamily) -> Self {
        Self {
            modality: InputModality::Controller,
            available: InputAvailability {
                controller: true,
                ..self.available
            },
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

/// Pointer events produced once a direct-touch tap is confirmed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum TouchPointerEvent {
    Move(winit::dpi::PhysicalPosition<f64>),
    Press,
    Release,
    Scroll {
        delta: winit::dpi::PhysicalPosition<f64>,
        phase: winit::event::TouchPhase,
    },
}

#[derive(Debug, Clone, Copy)]
struct ActiveTouch {
    id: u64,
    start: winit::dpi::PhysicalPosition<f64>,
    last: winit::dpi::PhysicalPosition<f64>,
    scrolling: bool,
}

/// Converts a confirmed single-finger tap into the toolkit's ordinary pointer
/// contract. Press is deliberately delayed until touch-end: many controls
/// activate on pointer-down, so emitting it earlier would toggle a checkbox
/// before a scroll or edge swipe had enough movement to cancel the tap.
#[derive(Debug, Default)]
pub(crate) struct TouchPointerEmulator {
    active: Option<ActiveTouch>,
}

impl TouchPointerEmulator {
    const TAP_SLOP: f64 = 16.0;
    const CANCEL_POINT: winit::dpi::PhysicalPosition<f64> =
        winit::dpi::PhysicalPosition::new(-1.0, -1.0);

    pub(crate) fn events(&mut self, touch: &winit::event::Touch) -> Vec<TouchPointerEvent> {
        use winit::event::TouchPhase;
        match touch.phase {
            TouchPhase::Started => {
                if self.active.is_some() {
                    self.active = None;
                    return Vec::new();
                }
                self.active = Some(ActiveTouch {
                    id: touch.id,
                    start: touch.location,
                    last: touch.location,
                    scrolling: false,
                });
                Vec::new()
            }
            TouchPhase::Moved => {
                let Some(active) = self.active else {
                    return Vec::new();
                };
                if active.id != touch.id {
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
                let Some(active) = self.active else {
                    return Vec::new();
                };
                if active.id != touch.id {
                    return Vec::new();
                }
                self.active = None;
                if active.scrolling {
                    vec![TouchPointerEvent::Scroll {
                        delta: winit::dpi::PhysicalPosition::new(0.0, 0.0),
                        phase: TouchPhase::Ended,
                    }]
                } else {
                    vec![
                        TouchPointerEvent::Move(touch.location),
                        TouchPointerEvent::Press,
                        TouchPointerEvent::Release,
                        TouchPointerEvent::Move(Self::CANCEL_POINT),
                    ]
                }
            }
            TouchPhase::Cancelled => {
                let Some(active) = self.active else {
                    return Vec::new();
                };
                if active.id != touch.id {
                    return Vec::new();
                }
                self.active = None;
                if active.scrolling {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_after_touch_preserves_coarse_pointer_capability() {
        let env = InteractionEnvironment::TOUCH.with_keyboard_modality();
        assert_eq!(env.modality, InputModality::Keyboard);
        assert_eq!(env.pointer_precision, PointerPrecision::Coarse);
        assert_eq!(env.minimum_target(), 48.0);
        assert!(!env.supports_hover());
        assert!(env.available.touch);
        assert!(env.available.keyboard);
    }

    #[test]
    fn modality_changes_do_not_erase_discovered_devices() {
        let env = InteractionEnvironment::POINTER
            .with_touch_modality()
            .with_controller(ControllerFamily::JoyConPair)
            .with_pointer_modality();
        assert_eq!(env.modality, InputModality::Pointer);
        assert!(env.available.pointer);
        assert!(env.available.touch);
        assert!(env.available.controller);
        assert_eq!(env.controller, Some(ControllerFamily::JoyConPair));
    }

    #[test]
    fn hint_priority_is_used_controller_then_keyboard_then_touch() {
        let all = InteractionEnvironment::POINTER
            .with_touch_modality()
            .with_controller(ControllerFamily::Xbox);
        assert_eq!(all.preferred_hint_modality(), InputModality::Controller);
        let keyboard_last = all.with_keyboard_modality();
        assert_eq!(
            keyboard_last.preferred_hint_modality(),
            InputModality::Keyboard
        );
        let touch_only = InteractionEnvironment {
            modality: InputModality::Touch,
            available: InputAvailability {
                keyboard: false,
                pointer: false,
                touch: true,
                controller: false,
            },
            ..InteractionEnvironment::TOUCH
        };
        assert_eq!(touch_only.preferred_hint_modality(), InputModality::Touch);
    }

    #[test]
    fn hid_names_select_legends_without_affecting_semantic_positions() {
        assert_eq!(
            ControllerFamily::from_device_name("Nintendo Switch Joy-Con (L)"),
            ControllerFamily::JoyConLeft
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

    fn touch(phase: winit::event::TouchPhase, id: u64, x: f64, y: f64) -> winit::event::Touch {
        winit::event::Touch {
            device_id: winit::event::DeviceId::dummy(),
            phase,
            location: winit::dpi::PhysicalPosition::new(x, y),
            force: None,
            id,
        }
    }

    #[test]
    fn a_tap_maps_to_press_and_release_only_after_touch_end() {
        use TouchPointerEvent::{Move, Press, Release};
        let mut emulator = TouchPointerEmulator::default();
        assert!(
            emulator
                .events(&touch(winit::event::TouchPhase::Started, 7, 10.0, 20.0))
                .is_empty()
        );
        assert_eq!(
            emulator.events(&touch(winit::event::TouchPhase::Ended, 7, 11.0, 20.0)),
            vec![
                Move((11.0, 20.0).into()),
                Press,
                Release,
                Move((-1.0, -1.0).into()),
            ]
        );
    }

    #[test]
    fn movement_or_a_second_finger_cancels_without_releasing_inside() {
        use TouchPointerEvent::{Move, Scroll};
        let mut emulator = TouchPointerEmulator::default();
        assert!(
            emulator
                .events(&touch(winit::event::TouchPhase::Started, 1, 40.0, 40.0))
                .is_empty()
        );
        assert_eq!(
            emulator.events(&touch(winit::event::TouchPhase::Moved, 1, 80.0, 40.0)),
            vec![
                Move((40.0, 40.0).into()),
                Scroll {
                    delta: (40.0, 0.0).into(),
                    phase: winit::event::TouchPhase::Moved,
                }
            ]
        );
        assert_eq!(
            emulator.events(&touch(winit::event::TouchPhase::Ended, 1, 80.0, 40.0)),
            vec![Scroll {
                delta: (0.0, 0.0).into(),
                phase: winit::event::TouchPhase::Ended,
            }]
        );

        assert!(
            emulator
                .events(&touch(winit::event::TouchPhase::Started, 2, 5.0, 5.0))
                .is_empty()
        );
        assert!(
            emulator
                .events(&touch(winit::event::TouchPhase::Started, 3, 8.0, 8.0))
                .is_empty()
        );
    }
}
