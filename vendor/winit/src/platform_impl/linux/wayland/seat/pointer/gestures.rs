//! Touchpad pointer gestures.
//!
//! Binds `zwp_pointer_gestures_v1` and, for each pointer, creates a
//! `zwp_pointer_gesture_pinch_v1`. The compositor reports a two-finger pinch
//! as `begin`/`update`/`end` with a *cumulative* `scale` (1.0 at begin). We
//! translate that into the same *incremental* delta the macOS backend emits
//! for `WindowEvent::PinchGesture` (per-frame change in magnification), so
//! that applications get a single, platform-agnostic pinch event.

use std::ops::Deref;
use std::sync::Mutex;

use sctk::reexports::client::globals::{BindError, GlobalList};
use sctk::reexports::client::{delegate_dispatch, Connection, Dispatch, Proxy, QueueHandle};
use sctk::reexports::protocols::wp::pointer_gestures::zv1::client::{
    zwp_pointer_gesture_pinch_v1::{self, ZwpPointerGesturePinchV1},
    zwp_pointer_gestures_v1::ZwpPointerGesturesV1,
};

use sctk::compositor::SurfaceData;
use sctk::globals::GlobalData;

use crate::event::{TouchPhase, WindowEvent};
use crate::platform_impl::wayland::state::WinitState;
use crate::platform_impl::wayland::{self, DeviceId, WindowId};

/// Wrapper around the pointer gestures manager global.
pub struct PointerGesturesState {
    manager: ZwpPointerGesturesV1,
}

impl PointerGesturesState {
    /// Bind the pointer gestures manager.
    pub fn new(
        globals: &GlobalList,
        queue_handle: &QueueHandle<WinitState>,
    ) -> Result<Self, BindError> {
        // The pinch gesture is available since interface version 1; we accept
        // up to 3 (swipe/hold live at higher versions but we don't use them).
        let manager = globals.bind(queue_handle, 1..=3, GlobalData)?;
        Ok(Self { manager })
    }
}

impl Deref for PointerGesturesState {
    type Target = ZwpPointerGesturesV1;

    fn deref(&self) -> &Self::Target {
        &self.manager
    }
}

/// Per-pinch-object state used to convert the protocol's cumulative `scale`
/// into the incremental deltas `WindowEvent::PinchGesture` expects.
#[derive(Debug, Default)]
pub struct GesturePinchData {
    inner: Mutex<GesturePinchInner>,
}

#[derive(Debug, Default)]
struct GesturePinchInner {
    /// Window the active gesture targets, resolved from the `begin` surface.
    window_id: Option<WindowId>,
    /// Last cumulative scale the compositor reported (1.0 at `begin`).
    scale: f64,
}

impl GesturePinchInner {
    /// Start a gesture targeting `window`, resetting the cumulative baseline to
    /// the protocol's `begin` value of 1.0.
    fn begin(&mut self, window: WindowId) {
        self.window_id = Some(window);
        self.scale = 1.0;
    }

    /// Fold the protocol's *cumulative* `scale` (1.0 at `begin`) into the
    /// *incremental* per-frame delta `WindowEvent::PinchGesture` carries — the
    /// same shape the macOS backend emits — while advancing the baseline.
    /// Returns the target window and this frame's delta, or `None` when no
    /// gesture is active (an `update` with no preceding `begin`).
    fn update(&mut self, scale: f64) -> Option<(WindowId, f64)> {
        let window_id = self.window_id?;
        let delta = scale - self.scale;
        self.scale = scale;
        Some((window_id, delta))
    }

    /// End the active gesture, resetting the baseline and returning the target
    /// window if one was active.
    fn end(&mut self) -> Option<WindowId> {
        self.scale = 1.0;
        self.window_id.take()
    }
}

impl Dispatch<ZwpPointerGesturesV1, GlobalData, WinitState> for PointerGesturesState {
    fn event(
        _state: &mut WinitState,
        _proxy: &ZwpPointerGesturesV1,
        _event: <ZwpPointerGesturesV1 as Proxy>::Event,
        _data: &GlobalData,
        _conn: &Connection,
        _qhandle: &QueueHandle<WinitState>,
    ) {
        // The gestures manager has no events.
    }
}

impl Dispatch<ZwpPointerGesturePinchV1, GesturePinchData, WinitState> for PointerGesturesState {
    fn event(
        state: &mut WinitState,
        _proxy: &ZwpPointerGesturePinchV1,
        event: <ZwpPointerGesturePinchV1 as Proxy>::Event,
        data: &GesturePinchData,
        _conn: &Connection,
        _qhandle: &QueueHandle<WinitState>,
    ) {
        let device_id = crate::event::DeviceId(crate::platform_impl::DeviceId::Wayland(DeviceId));

        match event {
            zwp_pointer_gesture_pinch_v1::Event::Begin { surface, .. } => {
                // A subsurface (e.g. CSD decorations) reports its parent as the
                // window surface, matching the plain pointer event handling.
                let window_surface = match surface.data::<SurfaceData>() {
                    Some(surface_data) => surface_data.parent_surface().unwrap_or(&surface).clone(),
                    None => surface,
                };
                let window_id = wayland::make_wid(&window_surface);

                data.inner.lock().unwrap().begin(window_id);

                state.events_sink.push_window_event(
                    WindowEvent::PinchGesture {
                        device_id,
                        delta: 0.0,
                        phase: TouchPhase::Started,
                    },
                    window_id,
                );
            }
            zwp_pointer_gesture_pinch_v1::Event::Update { scale, .. } => {
                // An update without a preceding begin has no active gesture.
                let (window_id, delta) = match data.inner.lock().unwrap().update(scale) {
                    Some(update) => update,
                    None => return,
                };

                state.events_sink.push_window_event(
                    WindowEvent::PinchGesture {
                        device_id,
                        delta,
                        phase: TouchPhase::Moved,
                    },
                    window_id,
                );
            }
            zwp_pointer_gesture_pinch_v1::Event::End { cancelled, .. } => {
                let window_id = data.inner.lock().unwrap().end();

                if let Some(window_id) = window_id {
                    let phase = if cancelled != 0 {
                        TouchPhase::Cancelled
                    } else {
                        TouchPhase::Ended
                    };
                    state.events_sink.push_window_event(
                        WindowEvent::PinchGesture {
                            device_id,
                            delta: 0.0,
                            phase,
                        },
                        window_id,
                    );
                }
            }
            _ => {}
        }
    }
}

delegate_dispatch!(WinitState: [ZwpPointerGesturesV1: GlobalData] => PointerGesturesState);
delegate_dispatch!(WinitState: [ZwpPointerGesturePinchV1: GesturePinchData] => PointerGesturesState);

#[cfg(test)]
mod tests {
    use super::GesturePinchInner;
    use crate::platform_impl::wayland::WindowId;

    /// `zwp_pointer_gesture_pinch_v1` reports a *cumulative* scale (1.0 at
    /// `begin`), but `WindowEvent::PinchGesture` — matching the macOS backend —
    /// carries the *incremental* per-frame magnification change. This drives the
    /// exact fold the `Dispatch` handler runs and pins the sign/scale
    /// convention: begin/end deltas are 0, each update is the running
    /// difference (negative when pinching back), and the deltas telescope to
    /// `cumulative - 1.0`.
    #[test]
    fn cumulative_scale_folds_to_incremental_delta() {
        let window = WindowId::from(7);
        let mut pinch = GesturePinchInner::default();

        // An update before any begin has no active gesture: ignored.
        assert_eq!(pinch.update(1.5), None);

        // begin establishes the target window and the 1.0 baseline.
        pinch.begin(window);

        // First report still at the baseline -> zero incremental delta.
        assert_eq!(pinch.update(1.0), Some((window, 0.0)));

        // Pinch-open: cumulative 1.25 -> +0.25 this frame.
        let (w1, d1) = pinch.update(1.25).unwrap();
        assert_eq!(w1, window);
        assert!((d1 - 0.25).abs() < 1e-9, "expected +0.25, got {d1}");

        // Pinch-back: cumulative falls to 1.10 -> -0.15 this frame. Deltas are
        // signed, matching the macOS magnification convention.
        let (_, d2) = pinch.update(1.10).unwrap();
        assert!((d2 + 0.15).abs() < 1e-9, "expected -0.15, got {d2}");

        // The incremental deltas telescope back to (cumulative - 1.0):
        // 0.0 + 0.25 - 0.15 == 0.10 == 1.10 - 1.0.
        assert!(
            ((d1 + d2) - 0.10).abs() < 1e-9,
            "deltas must sum to cumulative - 1.0"
        );

        // end returns the window and clears state; a second end is inert.
        assert_eq!(pinch.end(), Some(window));
        assert_eq!(pinch.end(), None);

        // After end there is no active gesture again.
        assert_eq!(pinch.update(2.0), None);
    }
}
