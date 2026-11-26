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
