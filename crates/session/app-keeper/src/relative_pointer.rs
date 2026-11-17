//! Relative pointer motion: `zwp_relative_pointer_manager_v1`.
//!
//! A game that reads the pointer relative to itself (no clamping at the screen edges) keeps its
//! relative pointers across a desktop restart: each is made again on the new desktop, and its
//! motion is passed on as it comes.

use std::collections::HashMap;

use smithay::reexports::wayland_server::backend::{ClientId, ObjectId};
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use wayland_client::{Connection, Proxy, QueueHandle};
use wayland_protocols::wp::relative_pointer::zv1::{
    client::{
        zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1,
        zwp_relative_pointer_v1::{self as client, ZwpRelativePointerV1},
    },
    server::{
        zwp_relative_pointer_manager_v1::{
            self as manager, ZwpRelativePointerManagerV1 as ServerManager,
        },
        zwp_relative_pointer_v1::{self as server, ZwpRelativePointerV1 as ServerPointer},
    },
};

use crate::state::State;

struct Pointer {
    pointer: ServerPointer,
    /// Its twin on the current desktop, while one is attached.
    up: Option<ZwpRelativePointerV1>,
}

/// The app's relative pointers.
pub(crate) struct RelativePointers {
    pointers: HashMap<ObjectId, Pointer>,
}

impl RelativePointers {
    pub(crate) fn new(dh: &DisplayHandle) -> Self {
        dh.create_global::<State, ServerManager, ()>(1, ());
        Self {
            pointers: HashMap::new(),
        }
    }
}

impl State {
    /// The desktop's relative pointer for the app's, made if there is a desktop.
    fn relative_pointer_up(&mut self, id: &ObjectId) -> Option<ZwpRelativePointerV1> {
        if let Some(pointer) = self.relative.pointers.get(id)
            && pointer.up.is_some()
        {
            return pointer.up.clone();
        }
        let up = self.up.as_ref()?;
        let (Some(manager), Some(pointer)) = (up.relative_pointer.clone(), up.pointer.clone())
        else {
            return None;
        };
        let twin = manager.get_relative_pointer(&pointer, &up.qh, ());
        self.relative.pointers.get_mut(id)?.up = Some(twin.clone());
        Some(twin)
    }

    /// Each app relative pointer is made again on a desktop that was just attached.
    pub(crate) fn desktop_relative_pointers(&mut self) {
        let ids: Vec<ObjectId> = self.relative.pointers.keys().cloned().collect();
        for id in ids {
            self.relative_pointer_up(&id);
        }
    }

    /// The desktop went away: its relative pointers are gone, the app's stay.
    pub(crate) fn hold_relative_pointers(&mut self) {
        for pointer in self.relative.pointers.values_mut() {
            pointer.up = None;
        }
    }
}

impl GlobalDispatch<ServerManager, ()> for State {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<ServerManager>,
        (): &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ServerManager, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &ServerManager,
        request: manager::Request,
        (): &(),
        _: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let manager::Request::GetRelativePointer { id, pointer: _ } = request {
            let pointer = data_init.init(id, ());
            let object = pointer.id();
            state
                .relative
                .pointers
                .insert(object.clone(), Pointer { pointer, up: None });
            state.relative_pointer_up(&object);
        }
    }
}

impl Dispatch<ServerPointer, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        pointer: &ServerPointer,
        request: server::Request,
        (): &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        if matches!(request, server::Request::Destroy) {
            state.relative.pointers.remove(&pointer.id());
        }
    }

    fn destroyed(state: &mut Self, _: ClientId, pointer: &ServerPointer, (): &()) {
        state.relative.pointers.remove(&pointer.id());
    }
}

impl wayland_client::Dispatch<ZwpRelativePointerManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwpRelativePointerManagerV1,
        _: <ZwpRelativePointerManagerV1 as Proxy>::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl wayland_client::Dispatch<ZwpRelativePointerV1, ()> for State {
    fn event(
        state: &mut Self,
        pointer: &ZwpRelativePointerV1,
        event: client::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let client::Event::RelativeMotion {
            utime_hi,
            utime_lo,
            dx,
            dy,
            dx_unaccel,
            dy_unaccel,
        } = event
        else {
            return;
        };
        let id = pointer.id().protocol_id();
        let Some(app) = state
            .relative
            .pointers
            .iter()
            .find(|(_, held)| {
                held.up
                    .as_ref()
                    .is_some_and(|twin| twin.id().protocol_id() == id)
            })
            .map(|(id, _)| id.clone())
        else {
            return;
        };
        if let Some(held) = state.relative.pointers.get(&app) {
            held.pointer
                .relative_motion(utime_hi, utime_lo, dx, dy, dx_unaccel, dy_unaccel);
        }
    }
}
