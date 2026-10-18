//! Which input a motion follows. A keyboard shortcut lands its result at
//! once; a pointer or a finger gets the motion (HIG §9.7).

use crate::tokens::MotionSource;
use winit::event::WindowEvent;

/// The input `event` comes from, for a host that opens and closes surfaces
/// on later frames and needs to know what asked: `None` for events that are
/// not input.
#[must_use]
pub fn source_of(event: &WindowEvent) -> Option<MotionSource> {
    match event {
        WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_) => Some(MotionSource::Keyboard),
        WindowEvent::MouseInput { .. }
        | WindowEvent::MouseWheel { .. }
        | WindowEvent::CursorMoved { .. } => Some(MotionSource::Pointer),
        WindowEvent::Touch(_)
        | WindowEvent::PinchGesture { .. }
        | WindowEvent::PanGesture { .. }
        | WindowEvent::RotationGesture { .. } => Some(MotionSource::Gesture),
        _ => None,
    }
}
