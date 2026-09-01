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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InteractionEnvironment {
    pub modality: InputModality,
    pub pointer_precision: PointerPrecision,
}

impl InteractionEnvironment {
    pub const POINTER: Self = Self {
        modality: InputModality::Pointer,
        pointer_precision: PointerPrecision::Fine,
    };

    pub const TOUCH: Self = Self {
        modality: InputModality::Touch,
        pointer_precision: PointerPrecision::Coarse,
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
