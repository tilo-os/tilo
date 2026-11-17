//! The clipboard and drag-and-drop: `wl_data_device_manager`.
//!
//! Both directions are forwarded, and no payload is read or copied here: the app's `receive`
//! hands its descriptor to the desktop's offer, and a desktop `send` hands its descriptor to the
//! app's source. A source the app creates has a twin on the desktop, and a drag runs on the
//! desktop like any other, including one over another window of the same app.
//!
//! Across a desktop restart the app's own selection is kept (`owner`): its source object and its
//! formats survive, and it is offered to the new desktop as soon as that desktop attaches. The
//! compositor takes it from the connection the session handed this keeper within a short while
//! after the attach, when nothing else owns the selection yet, with the time the app's copy was
//! made as the request's serial; a newer copy from anywhere else replaces it. The first keyboard
//! focus remains a second way to offer it, for a desktop whose window it is. A selection or drag
//! the old desktop had offered is cancelled to the app when it goes away.
//!
//! Text is offered under every name it goes by, both ways (`tilo_x11_transfer`): the X11 server
//! of a kept launch passes its programs' names through as they are, so an old program's
//! `UTF8_STRING` copy is offered to the desktop as `text/plain;charset=utf-8` and `text/plain`
//! too, and a desktop copy offered only as `text/plain` reaches X11 programs under their names.
//! A request for one name is passed on as the best name the other side offered.

use std::collections::HashMap;
use std::os::fd::AsFd as _;

use smithay::reexports::wayland_server::backend::{ClientId, ObjectId};
use smithay::reexports::wayland_server::protocol::{
    wl_data_device::{self, WlDataDevice},
    wl_data_device_manager::{self, DndAction, WlDataDeviceManager},
    wl_data_offer::{self, WlDataOffer},
    wl_data_source::{self, WlDataSource},
    wl_surface::WlSurface,
};
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use wayland_client::protocol as c;
use wayland_client::{Connection, Proxy as _, QueueHandle, WEnum, event_created_child};

use tilo_x11_transfer::{names_added, pick_format, with_text_names};

use crate::state::State;
use crate::surfaces::Role;
use crate::upstream::SurfaceRef;

/// The name a request for `wanted` is passed on as, out of the names the other side `offered`.
pub(crate) fn served_as(wanted: String, offered: &[String]) -> String {
    pick_format(&wanted, offered).unwrap_or(wanted)
}

/// The `wl_data_device_manager` version the app is offered and the desktop's is bound at.
const MANAGER_VERSION: u32 = 3;

/// Milliseconds of the machine's monotonic clock, in the low 32 bits: when the app's source was
/// set. Two keepers' claims meet within seconds of each other, and the wrap every 49 days is the
/// one a `Serial` already orders.
pub(crate) fn now_ms() -> u32 {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: the pointer names the timespec the call fills.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &raw mut now) } != 0 {
        return 0;
    }
    let seconds = u32::try_from(now.tv_sec).unwrap_or(0);
    let millis = u32::try_from(now.tv_nsec / 1_000_000).unwrap_or(0);
    seconds.wrapping_mul(1000).wrapping_add(millis)
}

/// A data source the app created.
struct Source {
    resource: WlDataSource,
    mimes: Vec<String>,
    actions: u32,
    /// Its twin on the current desktop, while one is attached.
    up: Option<c::wl_data_source::WlDataSource>,
}

/// An offer made to the app, mirroring one the desktop made.
struct Offer {
    resource: WlDataOffer,
    device: WlDataDevice,
    /// The desktop's offer, by protocol id.
    up: u32,
    /// Whether it is the drag's offer rather than the clipboard's.
    drag: bool,
}

/// An offer the desktop made.
struct UpOffer {
    offer: c::wl_data_offer::WlDataOffer,
    mimes: Vec<String>,
    /// The actions a drag's source allows, as `wl_data_device_manager.dnd_action` bits.
    source_actions: u32,
    /// The offers made to the app from it.
    apps: Vec<ObjectId>,
}

impl UpOffer {
    fn new(offer: c::wl_data_offer::WlDataOffer) -> Self {
        Self {
            offer,
            mimes: Vec::new(),
            source_actions: 0,
            apps: Vec::new(),
        }
    }
}

/// The clipboard and drag-and-drop state of both sides.
pub(crate) struct DataDevice {
    sources: HashMap<ObjectId, Source>,
    devices: HashMap<ObjectId, WlDataDevice>,
    offers: HashMap<ObjectId, Offer>,
    up_offers: HashMap<u32, UpOffer>,
    /// The desktop's offer that is the clipboard now.
    clipboard: Option<u32>,
    /// The app connection holding keyboard focus: who is offered the clipboard.
    focus: Option<ClientId>,
    /// The app's source that owns the clipboard, kept to offer it again after a restart.
    owner: Option<ObjectId>,
    /// When that source was set, in milliseconds of the machine's monotonic clock: the time the
    /// new desktop compares two keepers' claims by.
    owner_at: u32,
    /// The app's source of the drag that runs now.
    drag: Option<ObjectId>,
    /// The app surface the desktop's drag entered, and the app devices told of it.
    entered: Option<(ObjectId, Vec<ObjectId>)>,
    /// The desktop's drag was dropped on the app: its offers live on until the app, done
    /// reading, destroys them.
    dropped: bool,
}

impl DataDevice {
    pub(crate) fn new(dh: &DisplayHandle) -> Self {
        dh.create_global::<State, WlDataDeviceManager, ()>(MANAGER_VERSION, ());
        Self {
            sources: HashMap::new(),
            devices: HashMap::new(),
            offers: HashMap::new(),
            up_offers: HashMap::new(),
            clipboard: None,
            focus: None,
            owner: None,
            owner_at: 0,
            drag: None,
            entered: None,
            dropped: false,
        }
    }

    fn devices_of(&self, client: &ClientId) -> Vec<(ObjectId, WlDataDevice)> {
        self.devices
            .iter()
            .filter(|(_, device)| device.client().map(|c| c.id()).as_ref() == Some(client))
            .map(|(id, device)| (id.clone(), device.clone()))
            .collect()
    }

    /// The app source whose twin on the desktop is `up`.
    fn app_source_of(&self, up: u32) -> Option<ObjectId> {
        self.sources.iter().find_map(|(app, source)| {
            source
                .up
                .as_ref()
                .is_some_and(|twin| twin.id().protocol_id() == up)
                .then(|| app.clone())
        })
    }
}

impl State {
    /// The app is offered the clipboard it can see now, on every device of `client`.
    fn offer_clipboard_to(&mut self, client: &ClientId) {
        let Some(clipboard) = self.data_device.clipboard else {
            return;
        };
        let Some(up) = self.data_device.up_offers.get(&clipboard) else {
            return;
        };
        let mimes = up.mimes.clone();
        let devices = self.data_device.devices_of(client);
        let dh = self.dh.clone();
        for (_, device) in devices {
            let Some(owner) = device.client() else {
                continue;
            };
            let version = device.version().min(MANAGER_VERSION);
            let Ok(offer) = owner.create_resource::<WlDataOffer, _, State>(&dh, version, ()) else {
                continue;
            };
            device.data_offer(&offer);
            for mime in with_text_names(&mimes) {
                offer.offer(mime);
            }
            device.selection(Some(&offer));
            let id = offer.id();
            self.data_device.offers.insert(
                id.clone(),
                Offer {
                    resource: offer,
                    device,
                    up: clipboard,
                    drag: false,
                },
            );
            if let Some(up) = self.data_device.up_offers.get_mut(&clipboard) {
                up.apps.push(id);
            }
        }
    }

    /// The app's clipboard offers are taken back: it is told there is no selection.
    fn clear_clipboard_offers(&mut self) {
        let ids: Vec<ObjectId> = self
            .data_device
            .offers
            .iter()
            .filter(|(_, offer)| !offer.drag)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            self.forget_offer(&id, true);
        }
    }

    /// The app's drag offers are taken back.
    fn clear_drag_offers(&mut self) {
        let ids: Vec<ObjectId> = self
            .data_device
            .offers
            .iter()
            .filter(|(_, offer)| offer.drag)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            self.forget_offer(&id, false);
        }
    }

    fn forget_offer(&mut self, id: &ObjectId, unselect: bool) {
        let Some(offer) = self.data_device.offers.remove(id) else {
            return;
        };
        if let Some(up) = self.data_device.up_offers.get_mut(&offer.up) {
            up.apps.retain(|app| app != id);
        }
        if unselect && let Some(device) = self.data_device.devices.get(&offer.device.id()) {
            device.selection(None);
        }
        // The offer itself is the app's to destroy: a server-side destruction would free its
        // id for the next offer while the app still holds that object.
        let unused = self
            .data_device
            .up_offers
            .get(&offer.up)
            .is_some_and(|up| up.apps.is_empty())
            && self.data_device.clipboard != Some(offer.up);
        if unused
            && let Some(up) = self.data_device.up_offers.remove(&offer.up)
            && up.offer.is_alive()
        {
            up.offer.destroy();
        }
    }

    /// The app has keyboard focus on `focus`: it is offered the clipboard, and a selection it
    /// owned before the desktop restarted is offered to the desktop again.
    pub(crate) fn selection_focus_changed(&mut self) {
        let focus = self.seat.focused_client();
        if self.data_device.focus != focus {
            self.data_device.focus.clone_from(&focus);
            self.clear_clipboard_offers();
            if let Some(client) = focus {
                self.offer_clipboard_to(&client);
            }
        }
        self.reoffer_owner();
    }

    /// Offer the app's own selection to the desktop again: a clipboard a kept app owned must
    /// still paste after the desktop restarted. The desktop takes it from the client holding
    /// focus, with the first serial that focus arrived with.
    fn reoffer_owner(&mut self) {
        self.offer_owner(false, None);
    }

    /// Offer the app's selection to the desktop as soon as it attaches, before the app's window
    /// has the focus: the compositor takes it from a session-handed connection for a short while
    /// after it arrives, when nothing else owns the selection yet, and a keeper whose app copied
    /// later wins over one whose app copied earlier.
    pub(crate) fn reassert_owner(&mut self) {
        let at = self.data_device.owner_at;
        self.offer_owner(false, Some(at));
    }

    /// Offer the app's selection to the desktop: `force` replaces the twin already there (the app
    /// set a new selection), otherwise a twin that lives is left alone (a focus change). `claim`
    /// is the time the app's copy was made, when this is the first offer after an attach.
    fn offer_owner(&mut self, force: bool, claim: Option<u32>) {
        let Some(owner) = self.data_device.owner.clone() else {
            return;
        };
        let Some(source) = self.data_device.sources.get(&owner) else {
            self.data_device.owner = None;
            return;
        };
        let (has_twin, mimes, actions) =
            (source.up.is_some(), source.mimes.clone(), source.actions);
        if (claim.is_none() && self.data_device.focus.is_none()) || (has_twin && !force) {
            return;
        }
        if force
            && let Some(source) = self.data_device.sources.get_mut(&owner)
            && let Some(twin) = source.up.take()
        {
            twin.destroy();
        }
        let actions = c::wl_data_device_manager::DndAction::from_bits_truncate(actions);
        let serial = claim.unwrap_or_else(|| self.seat.last_serial());
        let Some(up) = self.up.as_ref() else {
            return;
        };
        let (Some(manager), Some(device)) = (&up.data_device_manager, &up.data_device) else {
            return;
        };
        let twin = manager.create_data_source(&up.qh, ());
        for mime in with_text_names(&mimes) {
            twin.offer(mime);
        }
        twin.set_actions(actions);
        device.set_selection(Some(&twin), serial);
        if let Some(source) = self.data_device.sources.get_mut(&owner) {
            source.up = Some(twin);
        }
    }

    /// The desktop's globals are bound: the keeper's own data device is made for its seat.
    pub(crate) fn desktop_data_device(&mut self) {
        let Some(up) = self.up.as_ref() else {
            return;
        };
        let (Some(manager), Some(seat)) = (up.data_device_manager.clone(), up.seat.clone()) else {
            return;
        };
        let qh = up.qh.clone();
        let device = manager.get_data_device(&seat, &qh, ());
        if let Some(up) = self.up.as_mut() {
            up.data_device = Some(device);
        }
    }

    /// The desktop went away: what the app held from it is cancelled, and its own selection is
    /// kept for the next desktop.
    pub(crate) fn hold_data_device(&mut self) {
        self.clear_clipboard_offers();
        self.clear_drag_offers();
        if let Some((surface, devices)) = self.data_device.entered.take() {
            let app = self.surfaces.get(&surface).map(|kept| kept.surface.clone());
            if app.is_some() {
                for id in devices {
                    if let Some(device) = self.data_device.devices.get(&id) {
                        device.leave();
                    }
                }
            }
        }
        if let Some(drag) = self.data_device.drag.clone()
            && let Some(source) = self.data_device.sources.get(&drag)
        {
            source.resource.cancelled();
        }
        self.data_device.drag = None;
        for source in self.data_device.sources.values_mut() {
            source.up = None;
        }
        for (_, up) in self.data_device.up_offers.drain() {
            if up.offer.is_alive() {
                up.offer.destroy();
            }
        }
        self.data_device.clipboard = None;
    }

    fn app_selected(&mut self, source: Option<WlDataSource>, serial: u32) {
        self.clear_clipboard_offers();
        let Some(source) = source else {
            if let Some(owner) = self.data_device.owner.take()
                && let Some(held) = self.data_device.sources.get_mut(&owner)
                && let Some(twin) = held.up.take()
            {
                twin.destroy();
            }
            if let Some(device) = self.up.as_ref().and_then(|up| up.data_device.as_ref()) {
                device.set_selection(None, serial);
            }
            return;
        };
        let id = source.id();
        if !self.data_device.sources.contains_key(&id) {
            return;
        }
        self.data_device.owner = Some(id);
        self.data_device.owner_at = now_ms();
        self.offer_owner(true, None);
    }

    fn app_started_drag(
        &mut self,
        source: Option<WlDataSource>,
        origin: &WlSurface,
        icon: Option<WlSurface>,
        serial: u32,
    ) {
        let Some(up) = self.up.as_ref() else {
            if let Some(source) = source {
                source.cancelled();
            }
            return;
        };
        let twin = source.as_ref().and_then(|source| {
            let id = source.id();
            let (mimes, actions) = self
                .data_device
                .sources
                .get(&id)
                .map(|held| (held.mimes.clone(), held.actions))
                .unwrap_or_default();
            let manager = up.data_device_manager.as_ref()?;
            let twin = manager.create_data_source(&up.qh, ());
            for mime in with_text_names(&mimes) {
                twin.offer(mime);
            }
            twin.set_actions(c::wl_data_device_manager::DndAction::from_bits_truncate(
                actions,
            ));
            if let Some(held) = self.data_device.sources.get_mut(&id) {
                held.up = Some(twin.clone());
            }
            Some(twin)
        });
        let Some(origin) = self.app_up_surface(origin) else {
            if let Some(source) = source {
                source.cancelled();
            }
            return;
        };
        let icon = icon.and_then(|icon| {
            if let Some(kept) = self.surfaces.get_mut(&icon.id())
                && matches!(kept.role, Role::None)
            {
                kept.role = Role::DragIcon;
            }
            self.app_up_surface(&icon)
        });
        let Some(up) = self.up.as_ref() else {
            return;
        };
        let Some(device) = &up.data_device else {
            return;
        };
        device.start_drag(twin.as_ref(), &origin, icon.as_ref(), serial);
        self.data_device.drag = source.map(|source| source.id());
    }

    /// The desktop's surface mirroring `surface`, made and pushed if it is not yet.
    fn app_up_surface(&mut self, surface: &WlSurface) -> Option<c::wl_surface::WlSurface> {
        let id = surface.id();
        self.ensure_up(&id);
        self.push(&id);
        self.surfaces
            .get(&id)
            .and_then(|kept| kept.up.as_ref())
            .map(|up| up.surface.clone())
    }

    /// The desktop's selection changed: the app is offered it, or told it is gone.
    fn upstream_selection(&mut self, offer: Option<c::wl_data_offer::WlDataOffer>) {
        self.clear_clipboard_offers();
        let Some(offer) = offer else {
            let old = self.data_device.clipboard.take();
            if let Some(old) = old
                && self
                    .data_device
                    .up_offers
                    .get(&old)
                    .is_some_and(|up| up.apps.is_empty())
                && let Some(up) = self.data_device.up_offers.remove(&old)
                && up.offer.is_alive()
            {
                up.offer.destroy();
            }
            return;
        };
        let id = offer.id().protocol_id();
        self.data_device
            .up_offers
            .entry(id)
            .or_insert_with(|| UpOffer::new(offer));
        self.data_device.clipboard = Some(id);
        if let Some(client) = self.data_device.focus.clone() {
            self.offer_clipboard_to(&client);
        }
    }

    /// The desktop's drag entered one of the app's surfaces.
    fn upstream_enter(
        &mut self,
        serial: u32,
        surface: &c::wl_surface::WlSurface,
        x: f64,
        y: f64,
        offer: Option<c::wl_data_offer::WlDataOffer>,
    ) {
        let Some(id) = surface.data::<SurfaceRef>().map(|r| r.0.clone()) else {
            return;
        };
        let Some(app) = self.surfaces.get(&id).map(|kept| kept.surface.clone()) else {
            return;
        };
        if self.xdnd.is_server(&app) {
            let mimes = offer
                .as_ref()
                .and_then(|offer| self.data_device.up_offers.remove(&offer.id().protocol_id()))
                .map(|up| (up.mimes, up.source_actions))
                .unwrap_or_default();
            if self.x11_drag_enter(serial, &app, (x, y), offer.clone(), mimes) {
                return;
            }
        }
        let Some(client) = app.client().map(|c| c.id()) else {
            return;
        };
        let mut devices = Vec::new();
        if let Some(offer) = offer {
            let up = offer.id().protocol_id();
            self.data_device
                .up_offers
                .entry(up)
                .or_insert_with(|| UpOffer::new(offer));
            let Some(mimes) = self
                .data_device
                .up_offers
                .get(&up)
                .map(|up| up.mimes.clone())
            else {
                return;
            };
            let dh = self.dh.clone();
            for (_, device) in self.data_device.devices_of(&client) {
                let Some(owner) = device.client() else {
                    continue;
                };
                let version = device.version().min(MANAGER_VERSION);
                let Ok(app_offer) =
                    owner.create_resource::<WlDataOffer, _, State>(&dh, version, ())
                else {
                    continue;
                };
                device.data_offer(&app_offer);
                for mime in with_text_names(&mimes) {
                    app_offer.offer(mime);
                }
                device.enter(serial, &app, x, y, Some(&app_offer));
                let offer_id = app_offer.id();
                self.data_device.offers.insert(
                    offer_id.clone(),
                    Offer {
                        resource: app_offer,
                        device: device.clone(),
                        up,
                        drag: true,
                    },
                );
                if let Some(up) = self.data_device.up_offers.get_mut(&up) {
                    up.apps.push(offer_id);
                }
                devices.push(device.id());
            }
        } else {
            for (id, device) in self.data_device.devices_of(&client) {
                device.enter(serial, &app, x, y, None);
                devices.push(id);
            }
        }
        self.data_device.entered = Some((id, devices));
        self.data_device.dropped = false;
    }

    fn upstream_leave(&mut self) {
        if self.x11_drag_leave() {
            return;
        }
        let Some((id, devices)) = self.data_device.entered.take() else {
            return;
        };
        // Destroying a dropped offer before the app finished would cancel the drop at its
        // source, and the app could not read it any more.
        if !std::mem::take(&mut self.data_device.dropped) {
            self.clear_drag_offers();
        }
        if !self.surfaces.contains_key(&id) {
            return;
        }
        for device in devices {
            if let Some(device) = self.data_device.devices.get(&device) {
                device.leave();
            }
        }
    }

    fn upstream_drag(&mut self, at: SurfacePoint) {
        let carried = match at {
            SurfacePoint::Motion { time, x, y } => self.x11_drag_motion(time, (x, y)),
            SurfacePoint::Drop => self.x11_drag_drop(),
        };
        if carried {
            return;
        }
        if matches!(at, SurfacePoint::Drop) {
            self.data_device.dropped = true;
        }
        let Some((id, devices)) = self.data_device.entered.clone() else {
            return;
        };
        if !self.surfaces.contains_key(&id) {
            return;
        }
        for device in devices {
            if let Some(device) = self.data_device.devices.get(&device) {
                match at {
                    SurfacePoint::Motion { time, x, y } => device.motion(time, x, y),
                    SurfacePoint::Drop => device.drop(),
                }
            }
        }
    }

    fn upstream_offer_actions(
        &self,
        id: u32,
        actions: WEnum<c::wl_data_device_manager::DndAction>,
        action: bool,
    ) {
        let WEnum::Value(actions) = actions else {
            return;
        };
        let Some(up) = self.data_device.up_offers.get(&id) else {
            return;
        };
        let apps = up.apps.clone();
        for app in apps {
            let Some(offer) = self.data_device.offers.get(&app) else {
                continue;
            };
            if offer.resource.version() < 3 {
                continue;
            }
            let actions = DndAction::from_bits_truncate(actions.bits());
            if action {
                offer.resource.action(actions);
            } else {
                offer.resource.source_actions(actions);
            }
        }
    }
}

impl Dispatch<WlDataDeviceManager, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &WlDataDeviceManager,
        request: wl_data_device_manager::Request,
        (): &(),
        _: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wl_data_device_manager::Request::CreateDataSource { id } => {
                let source = data_init.init(id, ());
                state.data_device.sources.insert(
                    source.id(),
                    Source {
                        resource: source,
                        mimes: Vec::new(),
                        actions: 0,
                        up: None,
                    },
                );
            }
            wl_data_device_manager::Request::GetDataDevice { id, seat: _ } => {
                let device = data_init.init(id, ());
                state
                    .data_device
                    .devices
                    .insert(device.id(), device.clone());
                if let Some(client) = device.client().map(|c| c.id())
                    && state.data_device.focus.as_ref() == Some(&client)
                {
                    state.offer_clipboard_to(&client);
                }
            }
            _ => {}
        }
    }
}

impl GlobalDispatch<WlDataDeviceManager, ()> for State {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<WlDataDeviceManager>,
        (): &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<WlDataDevice, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        device: &WlDataDevice,
        request: wl_data_device::Request,
        (): &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        match request {
            wl_data_device::Request::SetSelection { source, serial } => {
                state.app_selected(source, serial);
            }
            wl_data_device::Request::StartDrag {
                source,
                origin,
                icon,
                serial,
            } => state.app_started_drag(source, &origin, icon, serial),
            wl_data_device::Request::Release => {
                state.data_device.devices.remove(&device.id());
            }
            _ => {}
        }
    }

    fn destroyed(state: &mut Self, _: ClientId, device: &WlDataDevice, (): &()) {
        state.data_device.devices.remove(&device.id());
        let offers: Vec<ObjectId> = state
            .data_device
            .offers
            .iter()
            .filter(|(_, offer)| offer.device.id() == device.id())
            .map(|(id, _)| id.clone())
            .collect();
        for id in offers {
            state.forget_offer(&id, false);
        }
    }
}

impl Dispatch<WlDataSource, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        source: &WlDataSource,
        request: wl_data_source::Request,
        (): &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        let id = source.id();
        match request {
            wl_data_source::Request::Offer { mime_type } => {
                let twin = state
                    .data_device
                    .sources
                    .get(&id)
                    .and_then(|held| held.up.clone());
                if let Some(held) = state.data_device.sources.get_mut(&id) {
                    if let Some(twin) = twin {
                        for name in names_added(&held.mimes, &mime_type) {
                            twin.offer(name);
                        }
                    }
                    held.mimes.push(mime_type);
                }
            }
            wl_data_source::Request::SetActions { dnd_actions } => {
                let WEnum::Value(actions) = dnd_actions else {
                    return;
                };
                let twin = state
                    .data_device
                    .sources
                    .get(&id)
                    .and_then(|held| held.up.clone());
                if let Some(twin) = twin {
                    twin.set_actions(c::wl_data_device_manager::DndAction::from_bits_truncate(
                        actions.bits(),
                    ));
                }
                if let Some(held) = state.data_device.sources.get_mut(&id) {
                    held.actions = actions.bits();
                }
            }
            wl_data_source::Request::Destroy => {
                if let Some(held) = state.data_device.sources.remove(&id)
                    && let Some(twin) = held.up
                {
                    twin.destroy();
                }
                if state.data_device.owner == Some(id.clone()) {
                    state.data_device.owner = None;
                }
                if state.data_device.drag == Some(id) {
                    state.data_device.drag = None;
                }
            }
            _ => {}
        }
    }

    fn destroyed(state: &mut Self, _: ClientId, source: &WlDataSource, (): &()) {
        let id = source.id();
        state.data_device.sources.remove(&id);
        if state.data_device.owner == Some(id.clone()) {
            state.data_device.owner = None;
        }
        if state.data_device.drag == Some(id) {
            state.data_device.drag = None;
        }
    }
}

impl Dispatch<WlDataOffer, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        offer: &WlDataOffer,
        request: wl_data_offer::Request,
        (): &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        let id = offer.id();
        let Some(held) = state.data_device.offers.get(&id) else {
            return;
        };
        let Some((up, mimes)) = state
            .data_device
            .up_offers
            .get(&held.up)
            .map(|up| (up.offer.clone(), up.mimes.clone()))
        else {
            return;
        };
        match request {
            wl_data_offer::Request::Accept { serial, mime_type } => {
                up.accept(serial, mime_type.map(|mime| served_as(mime, &mimes)));
            }
            wl_data_offer::Request::Receive { mime_type, fd } => {
                up.receive(served_as(mime_type, &mimes), fd.as_fd());
            }
            wl_data_offer::Request::Destroy => state.forget_offer(&id, false),
            wl_data_offer::Request::Finish => {
                if up.version() >= 3 {
                    up.finish();
                }
            }
            wl_data_offer::Request::SetActions {
                dnd_actions,
                preferred_action,
            } => {
                let (WEnum::Value(actions), WEnum::Value(preferred)) =
                    (dnd_actions, preferred_action)
                else {
                    return;
                };
                if up.version() >= 3 {
                    up.set_actions(
                        c::wl_data_device_manager::DndAction::from_bits_truncate(actions.bits()),
                        c::wl_data_device_manager::DndAction::from_bits_truncate(preferred.bits()),
                    );
                }
            }
            _ => {}
        }
    }

    fn destroyed(state: &mut Self, _: ClientId, offer: &WlDataOffer, (): &()) {
        state.forget_offer(&offer.id(), false);
    }
}

/// Where a drag the desktop runs is, or its end.
#[derive(Clone, Copy)]
enum SurfacePoint {
    Motion { time: u32, x: f64, y: f64 },
    Drop,
}

impl wayland_client::Dispatch<c::wl_data_device_manager::WlDataDeviceManager, ()> for State {
    fn event(
        _: &mut Self,
        _: &c::wl_data_device_manager::WlDataDeviceManager,
        _: c::wl_data_device_manager::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl wayland_client::Dispatch<c::wl_data_device::WlDataDevice, ()> for State {
    fn event(
        state: &mut Self,
        _: &c::wl_data_device::WlDataDevice,
        event: c::wl_data_device::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use c::wl_data_device::Event;
        match event {
            Event::DataOffer { id } => {
                state
                    .data_device
                    .up_offers
                    .insert(id.id().protocol_id(), UpOffer::new(id));
            }
            Event::Selection { id } => state.upstream_selection(id),
            Event::Enter {
                serial,
                surface,
                x,
                y,
                id,
            } => state.upstream_enter(serial, &surface, x, y, id),
            Event::Leave => state.upstream_leave(),
            Event::Motion { time, x, y } => {
                state.upstream_drag(SurfacePoint::Motion { time, x, y });
            }
            Event::Drop => state.upstream_drag(SurfacePoint::Drop),
            _ => {}
        }
    }

    event_created_child!(State, c::wl_data_device::WlDataDevice, [
        c::wl_data_device::EVT_DATA_OFFER_OPCODE => (c::wl_data_offer::WlDataOffer, ()),
        c::wl_data_device::EVT_ENTER_OPCODE => (c::wl_data_offer::WlDataOffer, ()),
    ]);
}

impl wayland_client::Dispatch<c::wl_data_offer::WlDataOffer, ()> for State {
    fn event(
        state: &mut Self,
        offer: &c::wl_data_offer::WlDataOffer,
        event: c::wl_data_offer::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use c::wl_data_offer::Event;
        let id = offer.id().protocol_id();
        match event {
            Event::Offer { mime_type } => {
                let Some(up) = state.data_device.up_offers.get_mut(&id) else {
                    return;
                };
                let added = names_added(&up.mimes, &mime_type);
                up.mimes.push(mime_type);
                let apps = up.apps.clone();
                for app in apps {
                    if let Some(offer) = state.data_device.offers.get(&app) {
                        for name in &added {
                            offer.resource.offer(name.clone());
                        }
                    }
                }
            }
            Event::SourceActions { source_actions } => {
                if let WEnum::Value(actions) = source_actions {
                    if state.x11_drag_offer_actions(offer, actions.bits(), false) {
                        return;
                    }
                    if let Some(up) = state.data_device.up_offers.get_mut(&id) {
                        up.source_actions = actions.bits();
                    }
                }
                state.upstream_offer_actions(id, source_actions, false);
            }
            Event::Action { dnd_action } => {
                if let WEnum::Value(action) = dnd_action
                    && state.x11_drag_offer_actions(offer, action.bits(), true)
                {
                    return;
                }
                state.upstream_offer_actions(id, dnd_action, true);
            }
            _ => {}
        }
    }
}

impl wayland_client::Dispatch<c::wl_data_source::WlDataSource, ()> for State {
    fn event(
        state: &mut Self,
        source: &c::wl_data_source::WlDataSource,
        event: c::wl_data_source::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use c::wl_data_source::Event;
        let Some(event) = state.x11_drag_source_event(source, event) else {
            return;
        };
        let Some(app) = state.data_device.app_source_of(source.id().protocol_id()) else {
            return;
        };
        let Some((source, mimes)) = state
            .data_device
            .sources
            .get(&app)
            .map(|held| (held.resource.clone(), held.mimes.clone()))
        else {
            return;
        };
        match event {
            Event::Send { mime_type, fd } => source.send(served_as(mime_type, &mimes), fd.as_fd()),
            Event::Cancelled => {
                source.cancelled();
                if state.data_device.owner == Some(app.clone()) {
                    state.data_device.owner = None;
                }
                if state.data_device.drag == Some(app) {
                    state.data_device.drag = None;
                    state.clear_drag_offers();
                }
            }
            Event::DndDropPerformed => {
                if source.version() >= 3 {
                    source.dnd_drop_performed();
                }
            }
            Event::DndFinished => {
                if source.version() >= 3 {
                    source.dnd_finished();
                }
            }
            Event::Action {
                dnd_action: WEnum::Value(action),
            } if source.version() >= 3 => {
                source.action(DndAction::from_bits_truncate(action.bits()));
            }
            _ => {}
        }
    }
}
