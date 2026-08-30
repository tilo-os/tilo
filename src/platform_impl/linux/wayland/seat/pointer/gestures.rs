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

                {
                    let mut inner = data.inner.lock().unwrap();
                    inner.window_id = Some(window_id);
                    inner.scale = 1.0;
                }

                state.events_sink.push_window_event(
                    WindowEvent::PinchGesture {
                        device_id,
                        delta: 0.0,
                        phase: TouchPhase::Started,
                    },
                    window_id,
                );
            },
            zwp_pointer_gesture_pinch_v1::Event::Update { scale, .. } => {
                let (window_id, delta) = {
                    let mut inner = data.inner.lock().unwrap();
                    match inner.window_id {
                        Some(window_id) => {
                            let delta = scale - inner.scale;
                            inner.scale = scale;
                            (window_id, delta)
                        },
                        // An update without a preceding begin; ignore.
                        None => return,
                    }
                };

                state.events_sink.push_window_event(
                    WindowEvent::PinchGesture {
                        device_id,
                        delta,
                        phase: TouchPhase::Moved,
                    },
                    window_id,
                );
            },
            zwp_pointer_gesture_pinch_v1::Event::End { cancelled, .. } => {
                let window_id = {
                    let mut inner = data.inner.lock().unwrap();
                    inner.scale = 1.0;
                    inner.window_id.take()
                };

                if let Some(window_id) = window_id {
                    let phase = if cancelled != 0 {
                        TouchPhase::Cancelled
                    } else {
                        TouchPhase::Ended
                    };
                    state.events_sink.push_window_event(
                        WindowEvent::PinchGesture { device_id, delta: 0.0, phase },
                        window_id,
                    );
                }
            },
            _ => {},
        }
    }
}

delegate_dispatch!(WinitState: [ZwpPointerGesturesV1: GlobalData] => PointerGesturesState);
delegate_dispatch!(WinitState: [ZwpPointerGesturePinchV1: GesturePinchData] => PointerGesturesState);
