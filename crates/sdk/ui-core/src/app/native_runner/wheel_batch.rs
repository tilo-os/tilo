//! Wheel motion the platform reported in one wake, handed on as one event.
//!
//! A high-resolution wheel or a touchpad can report several deltas between
//! two frames. Each routed on its own pays the whole input path (hit-testing,
//! the widgets' handlers, the frame's bookkeeping) for motion the next frame
//! draws once. Consecutive wheel deltas for the same window and device, of
//! the same kind and in the middle of a gesture, are summed into one event;
//! anything else (another event, another window, a gesture's start or end,
//! the loop going idle) hands the sum on first, so the order of input is
//! never changed.

use winit::event::{MouseScrollDelta, TouchPhase, WindowEvent};
use winit::window::WindowId;

/// The wheel deltas waiting to be handed on.
#[derive(Debug, Default)]
pub(super) struct WheelBatch {
    pending: Option<(WindowId, WindowEvent)>,
}

impl WheelBatch {
    /// Take `event` for `window`: the events to route now, in order. A wheel
    /// delta that continues the one waiting joins it and routes nothing yet.
    pub(super) fn push(
        &mut self,
        window: WindowId,
        event: WindowEvent,
    ) -> Vec<(WindowId, WindowEvent)> {
        if !joins(&event) {
            let mut out: Vec<_> = self.pending.take().into_iter().collect();
            out.push((window, event));
            return out;
        }
        if let Some((pending_window, pending)) = &mut self.pending
            && *pending_window == window
            && merge(pending, &event)
        {
            return Vec::new();
        }
        self.pending.replace((window, event)).into_iter().collect()
    }

    /// The loop is about to wait: whatever waits goes now.
    pub(super) fn flush(&mut self) -> Option<(WindowId, WindowEvent)> {
        self.pending.take()
    }
}

/// A wheel delta inside a gesture (or a detented wheel's notch), which a
/// following one may be summed with.
fn joins(event: &WindowEvent) -> bool {
    matches!(
        event,
        WindowEvent::MouseWheel {
            phase: TouchPhase::Moved,
            ..
        }
    )
}

/// Add `next` to `into` when both come from the same device as the same
/// kind of delta; returns whether it did.
fn merge(into: &mut WindowEvent, next: &WindowEvent) -> bool {
    let (
        WindowEvent::MouseWheel {
            device_id,
            delta,
            phase: TouchPhase::Moved,
        },
        WindowEvent::MouseWheel {
            device_id: next_device,
            delta: next_delta,
            phase: TouchPhase::Moved,
        },
    ) = (into, next)
    else {
        return false;
    };
    if device_id != next_device {
        return false;
    }
    match (delta, next_delta) {
        (MouseScrollDelta::LineDelta(x, y), MouseScrollDelta::LineDelta(nx, ny)) => {
            *x += nx;
            *y += ny;
            true
        }
        (MouseScrollDelta::PixelDelta(at), MouseScrollDelta::PixelDelta(next)) => {
            at.x += next.x;
            at.y += next.y;
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::dpi::PhysicalPosition;
    use winit::event::DeviceId;

    fn window() -> WindowId {
        WindowId::dummy()
    }

    fn pixels(dy: f64, phase: TouchPhase) -> WindowEvent {
        WindowEvent::MouseWheel {
            device_id: DeviceId::dummy(),
            delta: MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, dy)),
            phase,
        }
    }

    fn lines(dy: f32) -> WindowEvent {
        WindowEvent::MouseWheel {
            device_id: DeviceId::dummy(),
            delta: MouseScrollDelta::LineDelta(0.0, dy),
            phase: TouchPhase::Moved,
        }
    }

    fn dy(event: &WindowEvent) -> f64 {
        match event {
            WindowEvent::MouseWheel {
                delta: MouseScrollDelta::PixelDelta(at),
                ..
            } => at.y,
            WindowEvent::MouseWheel {
                delta: MouseScrollDelta::LineDelta(_, y),
                ..
            } => f64::from(*y),
            other => panic!("not a wheel: {other:?}"),
        }
    }

    #[test]
    fn deltas_in_one_wake_are_handed_on_as_their_sum() {
        let mut batch = WheelBatch::default();
        assert!(
            batch
                .push(window(), pixels(-3.0, TouchPhase::Moved))
                .is_empty()
        );
        assert!(
            batch
                .push(window(), pixels(-4.0, TouchPhase::Moved))
                .is_empty()
        );
        assert!(
            batch
                .push(window(), pixels(-5.0, TouchPhase::Moved))
                .is_empty()
        );
        let (_, sum) = batch.flush().expect("the sum waits for the wake's end");
        assert!((dy(&sum) + 12.0).abs() < 1e-9);
        assert!(batch.flush().is_none());

        assert!(batch.push(window(), lines(-1.0)).is_empty());
        assert!(batch.push(window(), lines(-1.0)).is_empty());
        assert!((dy(&batch.flush().unwrap().1) + 2.0).abs() < 1e-9);
    }

    #[test]
    fn another_event_or_kind_hands_the_sum_on_first_and_keeps_the_order() {
        let mut batch = WheelBatch::default();
        batch.push(window(), pixels(-3.0, TouchPhase::Moved));
        batch.push(window(), pixels(-4.0, TouchPhase::Moved));
        let routed = batch.push(window(), pixels(0.0, TouchPhase::Ended));
        assert_eq!(routed.len(), 2, "{routed:?}");
        assert!((dy(&routed[0].1) + 7.0).abs() < 1e-9);
        assert!(matches!(
            routed[1].1,
            WindowEvent::MouseWheel {
                phase: TouchPhase::Ended,
                ..
            }
        ));
        assert!(batch.flush().is_none());

        batch.push(window(), pixels(-3.0, TouchPhase::Moved));
        let routed = batch.push(window(), lines(-1.0));
        assert_eq!(routed.len(), 1, "a line delta does not join pixels");
        assert!((dy(&routed[0].1) + 3.0).abs() < 1e-9);
        assert!((dy(&batch.flush().unwrap().1) + 1.0).abs() < 1e-9);

        batch.push(window(), lines(-1.0));
        let routed = batch.push(window(), WindowEvent::Focused(false));
        assert_eq!(routed.len(), 2);
        assert!(matches!(routed[1].1, WindowEvent::Focused(false)));
    }

    #[test]
    fn a_gesture_start_is_never_summed() {
        let mut batch = WheelBatch::default();
        let routed = batch.push(window(), pixels(-2.0, TouchPhase::Started));
        assert_eq!(routed.len(), 1, "a start routes at once");
        assert!(
            batch
                .push(window(), pixels(-3.0, TouchPhase::Moved))
                .is_empty()
        );
    }
}
