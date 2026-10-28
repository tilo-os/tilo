//! Opt-in benchmark input admission, separate from backend hit-test support.
use winit::event::WindowEvent;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum EventOrigin {
    Platform,
    Injected,
}

/// The live callback and injected input share this dispatch boundary.
pub(super) fn dispatch(
    isolated: bool,
    origin: EventOrigin,
    event: WindowEvent,
    deliver: impl FnOnce(WindowEvent),
) -> bool {
    // Keep focus, visibility, scale, resize, redraw and close notifications:
    // those are real lifecycle state, not replaceable scripted input.
    if isolated
        && origin == EventOrigin::Platform
        && matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::CursorEntered { .. }
                | WindowEvent::CursorLeft { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::PenSample { .. }
                | WindowEvent::MouseWheel { .. }
                | WindowEvent::KeyboardInput { .. }
                | WindowEvent::ModifiersChanged(_)
                | WindowEvent::KeyboardLayoutChanged(_)
                | WindowEvent::Ime(_)
                | WindowEvent::Touch(_)
                | WindowEvent::PinchGesture { .. }
                | WindowEvent::PanGesture { .. }
                | WindowEvent::DoubleTapGesture { .. }
                | WindowEvent::RotationGesture { .. }
                | WindowEvent::TouchpadPressure { .. }
                | WindowEvent::AxisMotion { .. }
                | WindowEvent::DroppedFile(_)
                | WindowEvent::HoveredFile(_)
                | WindowEvent::HoveredFileCancelled
        )
    {
        return false;
    }
    deliver(event);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        headless::FrameHarness,
        widget::{Column, Scrollable, Text, TextInput, Widget},
    };
    use std::{cell::Cell, rc::Rc};
    use winit::event::{DeviceId, ElementState};

    fn driver(root: Box<dyn Widget>) -> FrameHarness {
        FrameHarness::new_with_controlled_inputs(
            crate::State::new(),
            root,
            (400, 300),
            crate::Theme::dark(),
            crate::font_catalog::fixture_renderer(),
            crate::permissions::UserId("isolation-test".into()),
        )
    }

    use crate::synthetic_input::cursor_moved;

    fn wheel() -> WindowEvent {
        crate::synthetic_input::mouse_wheel(0.0, -3.0)
    }

    #[test]
    fn native_input_isolation_keeps_scripted_input_and_lifecycle_flowing() {
        // phase: native_input_isolation_preserves_scripted_scroll_against_platform_pointer
        {
            let offset = Rc::new(Cell::new((0.0, 0.0)));
            let mut column = Column::new().with_intrinsic_height();
            for index in 0..100 {
                column = column.push(Box::new(Text::new(&format!("row {index}"))));
            }
            let mut driver = driver(Box::new(
                Scrollable::new_vertical()
                    .with_persisted_offset(offset.clone())
                    .with_child(Box::new(column)),
            ));
            assert!(dispatch(
                true,
                EventOrigin::Injected,
                cursor_moved(100.0, 100.0),
                |event| driver.send(&event)
            ));
            assert!(!dispatch(
                true,
                EventOrigin::Platform,
                cursor_moved(500.0, 500.0),
                |event| driver.send(&event)
            ));
            assert!(dispatch(true, EventOrigin::Injected, wheel(), |event| {
                driver.send(&event)
            }));
            let moved = offset.get().1;
            assert!(moved > 0.0);
            assert!(!dispatch(true, EventOrigin::Platform, wheel(), |event| {
                driver.send(&event)
            }));
            assert_eq!(offset.get().1, moved);
            // The normal route still admits physical input; outside hover blocks wheel.
            assert!(dispatch(
                false,
                EventOrigin::Platform,
                cursor_moved(500.0, 500.0),
                |event| driver.send(&event)
            ));
            dispatch(false, EventOrigin::Injected, wheel(), |event| {
                driver.send(&event)
            });
            assert_eq!(offset.get().1, moved);
        }
        // phase: native_input_isolation_keeps_injected_text_and_rejects_platform_text
        {
            let text = crate::LocalState::new(String::new());
            let mut driver = driver(Box::new(TextInput::from_state(&text).with_id("query")));
            driver.click_named("query").unwrap();
            let key = || {
                crate::synthetic_input::key_input(
                    &winit::keyboard::Key::Character("x".into()),
                    ElementState::Pressed,
                )
            };
            assert!(!dispatch(true, EventOrigin::Platform, key(), |event| {
                driver.send(&event)
            }));
            assert_eq!(text.with(Clone::clone), "");
            assert!(dispatch(true, EventOrigin::Injected, key(), |event| driver
                .send(&event)));
            assert_eq!(text.with(Clone::clone), "x");
            assert!(dispatch(false, EventOrigin::Platform, key(), |event| {
                driver.send(&event)
            }));
            assert_eq!(text.with(Clone::clone), "xx");
        }
        // phase: native_input_isolation_leaves_lifecycle_and_window_management_flowing
        {
            let mut delivered = Vec::new();
            for event in [
                WindowEvent::CloseRequested,
                WindowEvent::RedrawRequested,
                WindowEvent::Resized(winit::dpi::PhysicalSize::new(800, 600)),
                WindowEvent::Focused(false),
                WindowEvent::Occluded(true),
                WindowEvent::Destroyed,
            ] {
                assert!(dispatch(true, EventOrigin::Platform, event, |event| {
                    delivered.push(event)
                }));
            }
            assert_eq!(delivered.len(), 6);
            for event in [
                WindowEvent::ModifiersChanged(winit::event::Modifiers::default()),
                WindowEvent::Ime(winit::event::Ime::Commit("physical".into())),
                WindowEvent::CursorLeft {
                    device_id: DeviceId::dummy(),
                },
            ] {
                assert!(!dispatch(true, EventOrigin::Platform, event, |_| panic!(
                    "platform input escaped isolation"
                )));
            }
        }
    }
}
