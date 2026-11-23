//! Pointer constraints: `zwp_pointer_constraints_v1`.
//!
//! A game's pointer lock (the pointer stops at the window and motion comes relative) and a
//! window's pointer confinement are passed to the desktop for the app's own surface, and the
//! desktop's answers come back. While no desktop is attached the constraint is unlocked to the
//! app; a persistent one is asked for again on the new desktop, a oneshot one is spent.

use std::collections::HashMap;

use smithay::reexports::wayland_server::backend::{ClientId, ObjectId};
use smithay::reexports::wayland_server::protocol::wl_region::WlRegion;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use smithay::wayland::compositor::{RectangleKind, get_region_attributes};
use wayland_client::protocol as c;
use wayland_client::{Connection, Proxy, QueueHandle, WEnum};
use wayland_protocols::wp::pointer_constraints::zv1::{
    client::{
        zwp_confined_pointer_v1::{self as client_confined, ZwpConfinedPointerV1},
        zwp_locked_pointer_v1::{self as client_locked, ZwpLockedPointerV1},
        zwp_pointer_constraints_v1::{self as client_manager, ZwpPointerConstraintsV1},
    },
    server::{
        zwp_confined_pointer_v1::{self as confined, ZwpConfinedPointerV1 as ServerConfined},
        zwp_locked_pointer_v1::{self as locked, ZwpLockedPointerV1 as ServerLocked},
        zwp_pointer_constraints_v1::{self as manager, ZwpPointerConstraintsV1 as ServerManager},
    },
};

use crate::state::State;
use crate::surfaces::Rect;

/// The regions of a `wl_region` the app set, in the desktop's form.
fn region_rects(region: &WlRegion) -> Vec<(bool, Rect)> {
    get_region_attributes(region)
        .rects
        .iter()
        .map(|(kind, rect)| {
            (
                matches!(kind, RectangleKind::Add),
                (rect.loc.x, rect.loc.y, rect.size.w, rect.size.h),
            )
        })
        .collect()
}

/// One constraint of the app.
struct Constraint {
    /// Whether the protocol asks it to stay until the object is destroyed.
    persistent: bool,
    /// The app surface it holds the pointer on.
    surface: ObjectId,
    region: Option<Vec<(bool, Rect)>>,
    hint: Option<(f64, f64)>,
    /// Its twin on the current desktop, while one is attached.
    up: Option<Twin>,
}

enum Twin {
    Locked(ZwpLockedPointerV1),
    Confined(ZwpConfinedPointerV1),
}

enum App {
    Locked(ServerLocked),
    Confined(ServerConfined),
}

/// The app's constraints, keyed by their objects.
pub(crate) struct PointerConstraints {
    constraints: HashMap<ObjectId, (App, Constraint)>,
}

impl PointerConstraints {
    pub(crate) fn new(dh: &DisplayHandle) -> Self {
        dh.create_global::<State, ServerManager, ()>(1, ());
        Self {
            constraints: HashMap::new(),
        }
    }

    fn up_of(&self, up: u32) -> Option<ObjectId> {
        self.constraints.iter().find_map(|(id, (_, held))| {
            let matches = match &held.up {
                Some(Twin::Locked(twin)) => twin.id().protocol_id() == up,
                Some(Twin::Confined(twin)) => twin.id().protocol_id() == up,
                None => false,
            };
            matches.then(|| id.clone())
        })
    }
}

impl State {
    /// The desktop's region for the constraint's held region.
    fn constraint_region(
        &self,
        region: Option<&Vec<(bool, Rect)>>,
    ) -> Option<c::wl_region::WlRegion> {
        self.up.as_ref()?.region(region.map(Vec::as_slice))
    }

    /// Make the constraint on the current desktop, if it has the surface and a pointer.
    fn constraint_up(&mut self, id: &ObjectId) {
        let Some(up) = self.up.as_ref() else {
            return;
        };
        let (Some(manager), Some(pointer)) = (up.pointer_constraints.clone(), up.pointer.clone())
        else {
            return;
        };
        let Some((_, held)) = self.pointer_constraints.constraints.get(id) else {
            return;
        };
        if held.up.is_some() {
            return;
        }
        let surface = held.surface.clone();
        let region = held.region.clone();
        let hint = held.hint;
        let persistent = held.persistent;
        let Some(surface) = self
            .surfaces
            .get(&surface)
            .and_then(|kept| kept.up.as_ref())
            .map(|up| up.surface.clone())
        else {
            return;
        };
        let region_up = self.constraint_region(region.as_ref());
        let lifetime = if persistent {
            client_manager::Lifetime::Persistent
        } else {
            client_manager::Lifetime::Oneshot
        };
        let twin = match self
            .pointer_constraints
            .constraints
            .get(id)
            .map(|(app, _)| app)
        {
            Some(App::Locked(_)) => {
                let locked = manager.lock_pointer(
                    &surface,
                    &pointer,
                    region_up.as_ref(),
                    lifetime,
                    &up.qh,
                    (),
                );
                if let Some((x, y)) = hint {
                    locked.set_cursor_position_hint(x, y);
                }
                Twin::Locked(locked)
            }
            Some(App::Confined(_)) => Twin::Confined(manager.confine_pointer(
                &surface,
                &pointer,
                region_up.as_ref(),
                lifetime,
                &up.qh,
                (),
            )),
            None => return,
        };
        if let Some(region) = region_up {
            region.destroy();
        }
        if let Some((_, held)) = self.pointer_constraints.constraints.get_mut(id) {
            held.up = Some(twin);
        }
    }

    /// A desktop was attached: the persistent constraints are asked for again.
    pub(crate) fn desktop_pointer_constraints(&mut self) {
        let ids: Vec<ObjectId> = self
            .pointer_constraints
            .constraints
            .iter()
            .filter(|(_, (_, held))| held.persistent)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            self.constraint_up(&id);
        }
    }

    /// The desktop went away: the app is told its pointer is free again.
    pub(crate) fn hold_pointer_constraints(&mut self) {
        for (app, held) in self.pointer_constraints.constraints.values_mut() {
            if let Some(twin) = held.up.take() {
                match (app, twin) {
                    (App::Locked(object), Twin::Locked(twin)) => {
                        object.unlocked();
                        twin.destroy();
                    }
                    (App::Confined(object), Twin::Confined(twin)) => {
                        object.unconfined();
                        twin.destroy();
                    }
                    (_, twin) => match twin {
                        Twin::Locked(twin) => twin.destroy(),
                        Twin::Confined(twin) => twin.destroy(),
                    },
                }
            }
        }
    }

    fn set_constraint_region(&mut self, id: &ObjectId, region: Option<&WlRegion>) {
        let Some(held) = self
            .pointer_constraints
            .constraints
            .get_mut(id)
            .map(|(_, held)| held)
        else {
            return;
        };
        held.region = region.map(region_rects);
        let rects = held.region.clone();
        let region_up = if held.up.is_some() {
            self.constraint_region(rects.as_ref())
        } else {
            None
        };
        let Some((_, held)) = self.pointer_constraints.constraints.get(id) else {
            return;
        };
        if let Some(Twin::Locked(twin)) = &held.up {
            twin.set_region(region_up.as_ref());
        } else if let Some(Twin::Confined(twin)) = &held.up {
            twin.set_region(region_up.as_ref());
        }
        if let Some(region) = region_up {
            region.destroy();
        }
    }

    fn constraint_destroyed(&mut self, id: &ObjectId) {
        if let Some((_, held)) = self.pointer_constraints.constraints.remove(id) {
            match held.up {
                Some(Twin::Locked(twin)) => twin.destroy(),
                Some(Twin::Confined(twin)) => twin.destroy(),
                None => {}
            }
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
        match request {
            manager::Request::LockPointer {
                id,
                surface,
                pointer: _,
                region,
                lifetime: WEnum::Value(lifetime),
            } => {
                let object = data_init.init(id, ());
                let object_id = object.id();
                state.pointer_constraints.constraints.insert(
                    object_id.clone(),
                    (
                        App::Locked(object),
                        Constraint {
                            persistent: lifetime == manager::Lifetime::Persistent,
                            surface: surface.id(),
                            region: region.as_ref().map(region_rects),
                            hint: None,
                            up: None,
                        },
                    ),
                );
                state.constraint_up(&object_id);
            }
            manager::Request::ConfinePointer {
                id,
                surface,
                pointer: _,
                region,
                lifetime: WEnum::Value(lifetime),
            } => {
                let object = data_init.init(id, ());
                let object_id = object.id();
                state.pointer_constraints.constraints.insert(
                    object_id.clone(),
                    (
                        App::Confined(object),
                        Constraint {
                            persistent: lifetime == manager::Lifetime::Persistent,
                            surface: surface.id(),
                            region: region.as_ref().map(region_rects),
                            hint: None,
                            up: None,
                        },
                    ),
                );
                state.constraint_up(&object_id);
            }
            _ => {}
        }
    }
}

impl Dispatch<ServerLocked, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        object: &ServerLocked,
        request: locked::Request,
        (): &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        match request {
            locked::Request::Destroy => state.constraint_destroyed(&object.id()),
            locked::Request::SetRegion { region } => {
                state.set_constraint_region(&object.id(), region.as_ref());
            }
            locked::Request::SetCursorPositionHint {
                surface_x,
                surface_y,
            } => {
                if let Some((_, held)) = state.pointer_constraints.constraints.get_mut(&object.id())
                {
                    held.hint = Some((surface_x, surface_y));
                }
                if let Some((_, held)) = state.pointer_constraints.constraints.get(&object.id())
                    && let Some(Twin::Locked(twin)) = &held.up
                {
                    twin.set_cursor_position_hint(surface_x, surface_y);
                }
            }
            _ => {}
        }
    }

    fn destroyed(state: &mut Self, _: ClientId, object: &ServerLocked, (): &()) {
        state.pointer_constraints.constraints.remove(&object.id());
    }
}

impl Dispatch<ServerConfined, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        object: &ServerConfined,
        request: confined::Request,
        (): &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        match request {
            confined::Request::Destroy => state.constraint_destroyed(&object.id()),
            confined::Request::SetRegion { region } => {
                state.set_constraint_region(&object.id(), region.as_ref());
            }
            _ => {}
        }
    }

    fn destroyed(state: &mut Self, _: ClientId, object: &ServerConfined, (): &()) {
        state.pointer_constraints.constraints.remove(&object.id());
    }
}

impl wayland_client::Dispatch<ZwpPointerConstraintsV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwpPointerConstraintsV1,
        _: <ZwpPointerConstraintsV1 as Proxy>::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl wayland_client::Dispatch<ZwpLockedPointerV1, ()> for State {
    fn event(
        state: &mut Self,
        pointer: &ZwpLockedPointerV1,
        event: client_locked::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let id = pointer.id().protocol_id();
        let Some(app) = state.pointer_constraints.up_of(id) else {
            return;
        };
        match event {
            client_locked::Event::Locked => {
                if let Some((App::Locked(object), _)) =
                    state.pointer_constraints.constraints.get(&app)
                {
                    object.locked();
                }
            }
            client_locked::Event::Unlocked => {
                if let Some((App::Locked(object), _)) =
                    state.pointer_constraints.constraints.get(&app)
                {
                    object.unlocked();
                }
            }
            _ => {}
        }
    }
}

impl wayland_client::Dispatch<ZwpConfinedPointerV1, ()> for State {
    fn event(
        state: &mut Self,
        pointer: &ZwpConfinedPointerV1,
        event: client_confined::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let id = pointer.id().protocol_id();
        let Some(app) = state.pointer_constraints.up_of(id) else {
            return;
        };
        match event {
            client_confined::Event::Confined => {
                if let Some((App::Confined(object), _)) =
                    state.pointer_constraints.constraints.get(&app)
                {
                    object.confined();
                }
            }
            client_confined::Event::Unconfined => {
                if let Some((App::Confined(object), _)) =
                    state.pointer_constraints.constraints.get(&app)
                {
                    object.unconfined();
                }
            }
            _ => {}
        }
    }
}
