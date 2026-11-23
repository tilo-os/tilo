//! Touchpad gestures: `zwp_pointer_gestures_v1`.
//!
//! Swipe, pinch and hold gestures the desktop hands to the keeper's pointer reach the app's
//! gesture objects for that pointer, on the same surface. A gesture object is made again on a
//! desktop that was just attached, so gestures keep working after a restart.

use std::collections::HashMap;

use smithay::reexports::wayland_server::backend::{ClientId, ObjectId};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use wayland_client::{Connection, Proxy, QueueHandle};
use wayland_protocols::wp::pointer_gestures::zv1::{
    client::{
        zwp_pointer_gesture_hold_v1::{self as client_hold, ZwpPointerGestureHoldV1},
        zwp_pointer_gesture_pinch_v1::{self as client_pinch, ZwpPointerGesturePinchV1},
        zwp_pointer_gesture_swipe_v1::{self as client_swipe, ZwpPointerGestureSwipeV1},
        zwp_pointer_gestures_v1::ZwpPointerGesturesV1,
    },
    server::{
        zwp_pointer_gesture_hold_v1::{self as hold, ZwpPointerGestureHoldV1 as ServerHold},
        zwp_pointer_gesture_pinch_v1::{self as pinch, ZwpPointerGesturePinchV1 as ServerPinch},
        zwp_pointer_gesture_swipe_v1::{self as swipe, ZwpPointerGestureSwipeV1 as ServerSwipe},
        zwp_pointer_gestures_v1::{self as server_manager, ZwpPointerGesturesV1 as ServerGestures},
    },
};

use crate::state::State;
use crate::upstream::SurfaceRef;

/// The `zwp_pointer_gestures_v1` version the app is offered and the desktop's is bound at.
const GESTURES_VERSION: u32 = 3;

enum App {
    Swipe(ServerSwipe),
    Pinch(ServerPinch),
    Hold(ServerHold),
}

enum Twin {
    Swipe(ZwpPointerGestureSwipeV1),
    Pinch(ZwpPointerGesturePinchV1),
    Hold(ZwpPointerGestureHoldV1),
}

struct Gesture {
    app: App,
    /// Its twin on the current desktop, while one is attached.
    up: Option<Twin>,
}

/// The app's gesture objects.
pub(crate) struct PointerGestures {
    gestures: HashMap<ObjectId, Gesture>,
}

impl PointerGestures {
    pub(crate) fn new(dh: &DisplayHandle) -> Self {
        dh.create_global::<State, ServerGestures, ()>(GESTURES_VERSION, ());
        Self {
            gestures: HashMap::new(),
        }
    }

    fn app_of(&self, twin: &Twin) -> Option<ObjectId> {
        self.gestures.iter().find_map(|(id, gesture)| {
            let matches = match (&gesture.up, twin) {
                (Some(Twin::Swipe(held)), Twin::Swipe(want)) => held.id() == want.id(),
                (Some(Twin::Pinch(held)), Twin::Pinch(want)) => held.id() == want.id(),
                (Some(Twin::Hold(held)), Twin::Hold(want)) => held.id() == want.id(),
                _ => false,
            };
            matches.then(|| id.clone())
        })
    }
}

impl State {
    fn gesture_up(&mut self, id: &ObjectId) {
        let Some(up) = self.up.as_ref() else {
            return;
        };
        let (Some(manager), Some(pointer)) = (up.gestures.clone(), up.pointer.clone()) else {
            return;
        };
        let Some(gesture) = self.gestures.gestures.get(id) else {
            return;
        };
        if gesture.up.is_some() {
            return;
        }
        let twin = match gesture.app {
            App::Swipe(_) => Twin::Swipe(manager.get_swipe_gesture(&pointer, &up.qh, ())),
            App::Pinch(_) => Twin::Pinch(manager.get_pinch_gesture(&pointer, &up.qh, ())),
            App::Hold(_) => Twin::Hold(manager.get_hold_gesture(&pointer, &up.qh, ())),
        };
        if let Some(gesture) = self.gestures.gestures.get_mut(id) {
            gesture.up = Some(twin);
        }
    }

    /// A desktop was attached: every gesture object is made again.
    pub(crate) fn desktop_pointer_gestures(&mut self) {
        let ids: Vec<ObjectId> = self.gestures.gestures.keys().cloned().collect();
        for id in ids {
            self.gesture_up(&id);
        }
    }

    /// The desktop went away: its gesture objects are gone, the app's stay.
    pub(crate) fn hold_pointer_gestures(&mut self) {
        for gesture in self.gestures.gestures.values_mut() {
            gesture.up = None;
        }
    }

    /// The app surface a desktop surface mirrors.
    fn gesture_surface(&self, surface: &c::wl_surface::WlSurface) -> Option<WlSurface> {
        let id = &surface.data::<SurfaceRef>()?.0;
        self.surfaces.get(id).map(|kept| kept.surface.clone())
    }
}

use wayland_client::protocol as c;

impl GlobalDispatch<ServerGestures, ()> for State {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<ServerGestures>,
        (): &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ServerGestures, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &ServerGestures,
        request: server_manager::Request,
        (): &(),
        _: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            server_manager::Request::GetSwipeGesture { id, pointer: _ } => {
                let object = data_init.init(id, ());
                let object_id = object.id();
                state.gestures.gestures.insert(
                    object_id.clone(),
                    Gesture {
                        app: App::Swipe(object),
                        up: None,
                    },
                );
                state.gesture_up(&object_id);
            }
            server_manager::Request::GetPinchGesture { id, pointer: _ } => {
                let object = data_init.init(id, ());
                let object_id = object.id();
                state.gestures.gestures.insert(
                    object_id.clone(),
                    Gesture {
                        app: App::Pinch(object),
                        up: None,
                    },
                );
                state.gesture_up(&object_id);
            }
            server_manager::Request::GetHoldGesture { id, pointer: _ } => {
                let object = data_init.init(id, ());
                let object_id = object.id();
                state.gestures.gestures.insert(
                    object_id.clone(),
                    Gesture {
                        app: App::Hold(object),
                        up: None,
                    },
                );
                state.gesture_up(&object_id);
            }
            _ => {}
        }
    }
}

macro_rules! gesture_dispatch {
    ($server:ty, $request:path, $destroy:path) => {
        impl Dispatch<$server, ()> for State {
            fn request(
                state: &mut Self,
                _: &Client,
                object: &$server,
                request: $request,
                (): &(),
                _: &DisplayHandle,
                _: &mut DataInit<'_, Self>,
            ) {
                if matches!(request, $destroy) {
                    state.gestures.gestures.remove(&object.id());
                }
            }

            fn destroyed(state: &mut Self, _: ClientId, object: &$server, (): &()) {
                state.gestures.gestures.remove(&object.id());
            }
        }
    };
}

gesture_dispatch!(ServerSwipe, swipe::Request, swipe::Request::Destroy);
gesture_dispatch!(ServerPinch, pinch::Request, pinch::Request::Destroy);
gesture_dispatch!(ServerHold, hold::Request, hold::Request::Destroy);

impl wayland_client::Dispatch<ZwpPointerGesturesV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwpPointerGesturesV1,
        _: <ZwpPointerGesturesV1 as Proxy>::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl wayland_client::Dispatch<ZwpPointerGestureSwipeV1, ()> for State {
    fn event(
        state: &mut Self,
        gesture: &ZwpPointerGestureSwipeV1,
        event: client_swipe::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let twin = Twin::Swipe(gesture.clone());
        let Some(app) = state.gestures.app_of(&twin) else {
            return;
        };
        let Some(gesture) = state.gestures.gestures.get(&app) else {
            return;
        };
        let App::Swipe(object) = &gesture.app else {
            return;
        };
        match event {
            client_swipe::Event::Begin {
                serial,
                time,
                surface,
                fingers,
            } => {
                if let Some(surface) = state.gesture_surface(&surface) {
                    object.begin(serial, time, &surface, fingers);
                }
            }
            client_swipe::Event::Update { time, dx, dy } => object.update(time, dx, dy),
            client_swipe::Event::End {
                serial,
                time,
                cancelled,
            } => object.end(serial, time, cancelled),
            _ => {}
        }
    }
}

impl wayland_client::Dispatch<ZwpPointerGesturePinchV1, ()> for State {
    fn event(
        state: &mut Self,
        gesture: &ZwpPointerGesturePinchV1,
        event: client_pinch::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let twin = Twin::Pinch(gesture.clone());
        let Some(app) = state.gestures.app_of(&twin) else {
            return;
        };
        let Some(gesture) = state.gestures.gestures.get(&app) else {
            return;
        };
        let App::Pinch(object) = &gesture.app else {
            return;
        };
        match event {
            client_pinch::Event::Begin {
                serial,
                time,
                surface,
                fingers,
            } => {
                if let Some(surface) = state.gesture_surface(&surface) {
                    object.begin(serial, time, &surface, fingers);
                }
            }
            client_pinch::Event::Update {
                time,
                dx,
                dy,
                scale,
                rotation,
            } => object.update(time, dx, dy, scale, rotation),
            client_pinch::Event::End {
                serial,
                time,
                cancelled,
            } => object.end(serial, time, cancelled),
            _ => {}
        }
    }
}

impl wayland_client::Dispatch<ZwpPointerGestureHoldV1, ()> for State {
    fn event(
        state: &mut Self,
        gesture: &ZwpPointerGestureHoldV1,
        event: client_hold::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let twin = Twin::Hold(gesture.clone());
        let Some(app) = state.gestures.app_of(&twin) else {
            return;
        };
        let Some(gesture) = state.gestures.gestures.get(&app) else {
            return;
        };
        let App::Hold(object) = &gesture.app else {
            return;
        };
        match event {
            client_hold::Event::Begin {
                serial,
                time,
                surface,
                fingers,
            } => {
                if let Some(surface) = state.gesture_surface(&surface) {
                    object.begin(serial, time, &surface, fingers);
                }
            }
            client_hold::Event::End {
                serial,
                time,
                cancelled,
            } => object.end(serial, time, cancelled),
            _ => {}
        }
    }
}
