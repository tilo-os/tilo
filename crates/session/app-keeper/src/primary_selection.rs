//! The primary selection: `zwp_primary_selection_device_manager_v1`.
//!
//! Select-then-middle-click, forwarded like the clipboard and with the same rule across a
//! restart: the app's own primary selection is kept and offered to the new desktop as soon as it
//! attaches, before the app's window has the focus. No payload is read here; descriptors are
//! passed as they are.

use std::collections::HashMap;
use std::os::fd::AsFd as _;

use smithay::reexports::wayland_server::backend::{ClientId, ObjectId};
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use wayland_client::{Connection, Proxy, QueueHandle, event_created_child};
use wayland_protocols::wp::primary_selection::zv1::{
    client::{
        zwp_primary_selection_device_manager_v1::ZwpPrimarySelectionDeviceManagerV1,
        zwp_primary_selection_device_v1::{self as client_device, ZwpPrimarySelectionDeviceV1},
        zwp_primary_selection_offer_v1::{self as client_offer, ZwpPrimarySelectionOfferV1},
        zwp_primary_selection_source_v1::{self as client_source, ZwpPrimarySelectionSourceV1},
    },
    server::{
        zwp_primary_selection_device_manager_v1::{
            self as manager, ZwpPrimarySelectionDeviceManagerV1 as ServerManager,
        },
        zwp_primary_selection_device_v1::{
            self as device, ZwpPrimarySelectionDeviceV1 as ServerDevice,
        },
        zwp_primary_selection_offer_v1::{
            self as offer, ZwpPrimarySelectionOfferV1 as ServerOffer,
        },
        zwp_primary_selection_source_v1::{
            self as source, ZwpPrimarySelectionSourceV1 as ServerSource,
        },
    },
};

use tilo_x11_transfer::{names_added, with_text_names};

use crate::data_device::served_as;
use crate::state::State;

/// A primary selection source the app created.
struct Source {
    resource: ServerSource,
    mimes: Vec<String>,
    /// Its twin on the current desktop, while one is attached.
    up: Option<ZwpPrimarySelectionSourceV1>,
}

/// An offer made to the app, mirroring one the desktop made.
struct Offer {
    resource: ServerOffer,
    device: ServerDevice,
    /// The desktop's offer, by protocol id.
    up: u32,
}

/// An offer the desktop made.
struct UpOffer {
    offer: ZwpPrimarySelectionOfferV1,
    mimes: Vec<String>,
    apps: Vec<ObjectId>,
}

/// The primary selection's state on both sides.
pub(crate) struct PrimarySelection {
    sources: HashMap<ObjectId, Source>,
    devices: HashMap<ObjectId, ServerDevice>,
    offers: HashMap<ObjectId, Offer>,
    up_offers: HashMap<u32, UpOffer>,
    selection: Option<u32>,
    focus: Option<ClientId>,
    owner: Option<ObjectId>,
    /// When that source was set, in milliseconds of the machine's monotonic clock: the time the
    /// new desktop compares two keepers' claims by.
    owner_at: u32,
}

impl PrimarySelection {
    pub(crate) fn new(dh: &DisplayHandle) -> Self {
        dh.create_global::<State, ServerManager, ()>(1, ());
        Self {
            sources: HashMap::new(),
            devices: HashMap::new(),
            offers: HashMap::new(),
            up_offers: HashMap::new(),
            selection: None,
            focus: None,
            owner: None,
            owner_at: 0,
        }
    }

    fn devices_of(&self, client: &ClientId) -> Vec<ServerDevice> {
        self.devices
            .values()
            .filter(|device| device.client().map(|c| c.id()).as_ref() == Some(client))
            .cloned()
            .collect()
    }

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
    fn primary_offer_to(&mut self, client: &ClientId) {
        let Some(selection) = self.primary.selection else {
            return;
        };
        let Some(up) = self.primary.up_offers.get(&selection) else {
            return;
        };
        let mimes = up.mimes.clone();
        let devices = self.primary.devices_of(client);
        let dh = self.dh.clone();
        for device in devices {
            let Some(owner) = device.client() else {
                continue;
            };
            let Ok(offer) = owner.create_resource::<ServerOffer, _, State>(&dh, 1, ()) else {
                continue;
            };
            device.data_offer(&offer);
            for mime in with_text_names(&mimes) {
                offer.offer(mime);
            }
            device.selection(Some(&offer));
            let id = offer.id();
            self.primary.offers.insert(
                id.clone(),
                Offer {
                    resource: offer,
                    device,
                    up: selection,
                },
            );
            if let Some(up) = self.primary.up_offers.get_mut(&selection) {
                up.apps.push(id);
            }
        }
    }

    fn primary_clear_offers(&mut self) {
        let ids: Vec<ObjectId> = self.primary.offers.keys().cloned().collect();
        for id in ids {
            self.primary_forget_offer(&id, true);
        }
    }

    fn primary_forget_offer(&mut self, id: &ObjectId, unselect: bool) {
        let Some(offer) = self.primary.offers.remove(id) else {
            return;
        };
        if let Some(up) = self.primary.up_offers.get_mut(&offer.up) {
            up.apps.retain(|app| app != id);
        }
        if unselect && let Some(device) = self.primary.devices.get(&offer.device.id()) {
            device.selection(None);
        }
        // The offer itself is the app's to destroy: a server-side destruction would free its
        // id for the next offer while the app still holds that object.
        let unused = self
            .primary
            .up_offers
            .get(&offer.up)
            .is_some_and(|up| up.apps.is_empty())
            && self.primary.selection != Some(offer.up);
        if unused
            && let Some(up) = self.primary.up_offers.remove(&offer.up)
            && up.offer.is_alive()
        {
            up.offer.destroy();
        }
    }

    /// The app's keyboard focus changed: it is offered the primary selection, and a selection it
    /// owned before the desktop restarted is offered again with the first serial focus brings.
    pub(crate) fn primary_focus_changed(&mut self) {
        let focus = self.seat.focused_client();
        if self.primary.focus != focus {
            self.primary.focus.clone_from(&focus);
            self.primary_clear_offers();
            if let Some(client) = focus {
                self.primary_offer_to(&client);
            }
        }
        self.primary_reoffer_owner();
    }

    fn primary_reoffer_owner(&mut self) {
        self.primary_offer_owner(false, None);
    }

    /// Offer the app's primary selection as soon as the desktop attaches, before the app's window
    /// has the focus: the compositor takes it from a session-handed connection for a short while
    /// after it arrives, when nothing else owns the selection yet.
    pub(crate) fn primary_reassert_owner(&mut self) {
        let at = self.primary.owner_at;
        self.primary_offer_owner(false, Some(at));
    }

    /// Offer the app's selection to the desktop: `force` replaces the twin already there. `claim`
    /// is the time the app's selection was made, when this is the first offer after an attach.
    fn primary_offer_owner(&mut self, force: bool, claim: Option<u32>) {
        let Some(owner) = self.primary.owner.clone() else {
            return;
        };
        let Some(source) = self.primary.sources.get(&owner) else {
            self.primary.owner = None;
            return;
        };
        let (has_twin, mimes) = (source.up.is_some(), source.mimes.clone());
        if (claim.is_none() && self.primary.focus.is_none()) || (has_twin && !force) {
            return;
        }
        if force
            && let Some(source) = self.primary.sources.get_mut(&owner)
            && let Some(twin) = source.up.take()
        {
            twin.destroy();
        }
        let serial = claim.unwrap_or_else(|| self.seat.last_serial());
        let Some(up) = self.up.as_ref() else {
            return;
        };
        let (Some(manager), Some(device)) = (&up.primary_selection, &up.primary_selection_device)
        else {
            return;
        };
        let twin = manager.create_source(&up.qh, ());
        for mime in with_text_names(&mimes) {
            twin.offer(mime);
        }
        device.set_selection(Some(&twin), serial);
        if let Some(source) = self.primary.sources.get_mut(&owner) {
            source.up = Some(twin);
        }
    }

    /// The desktop's globals are bound: the keeper's own primary device is made for its seat.
    pub(crate) fn desktop_primary_selection(&mut self) {
        let Some(up) = self.up.as_ref() else {
            return;
        };
        let (Some(manager), Some(seat)) = (up.primary_selection.clone(), up.seat.clone()) else {
            return;
        };
        let qh = up.qh.clone();
        let device = manager.get_device(&seat, &qh, ());
        if let Some(up) = self.up.as_mut() {
            up.primary_selection_device = Some(device);
        }
    }

    /// The desktop went away: its offers are cancelled to the app, its own selection is kept.
    pub(crate) fn hold_primary_selection(&mut self) {
        self.primary_clear_offers();
        for source in self.primary.sources.values_mut() {
            source.up = None;
        }
        for (_, up) in self.primary.up_offers.drain() {
            if up.offer.is_alive() {
                up.offer.destroy();
            }
        }
        self.primary.selection = None;
    }

    fn primary_selected(&mut self, source: Option<ServerSource>, serial: u32) {
        self.primary_clear_offers();
        let Some(source) = source else {
            if let Some(owner) = self.primary.owner.take()
                && let Some(held) = self.primary.sources.get_mut(&owner)
                && let Some(twin) = held.up.take()
            {
                twin.destroy();
            }
            if let Some(device) = self
                .up
                .as_ref()
                .and_then(|up| up.primary_selection_device.as_ref())
            {
                device.set_selection(None, serial);
            }
            return;
        };
        let id = source.id();
        if !self.primary.sources.contains_key(&id) {
            return;
        }
        self.primary.owner = Some(id);
        self.primary.owner_at = crate::data_device::now_ms();
        self.primary_offer_owner(true, None);
    }

    fn upstream_primary_selection(&mut self, offer: Option<ZwpPrimarySelectionOfferV1>) {
        self.primary_clear_offers();
        let Some(offer) = offer else {
            let old = self.primary.selection.take();
            if let Some(old) = old
                && self
                    .primary
                    .up_offers
                    .get(&old)
                    .is_some_and(|up| up.apps.is_empty())
                && let Some(up) = self.primary.up_offers.remove(&old)
                && up.offer.is_alive()
            {
                up.offer.destroy();
            }
            return;
        };
        let id = offer.id().protocol_id();
        self.primary.up_offers.entry(id).or_insert_with(|| UpOffer {
            offer,
            mimes: Vec::new(),
            apps: Vec::new(),
        });
        self.primary.selection = Some(id);
        if let Some(client) = self.primary.focus.clone() {
            self.primary_offer_to(&client);
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
            manager::Request::CreateSource { id } => {
                let source = data_init.init(id, ());
                state.primary.sources.insert(
                    source.id(),
                    Source {
                        resource: source,
                        mimes: Vec::new(),
                        up: None,
                    },
                );
            }
            manager::Request::GetDevice { id, seat: _ } => {
                let device = data_init.init(id, ());
                state.primary.devices.insert(device.id(), device.clone());
                if let Some(client) = device.client().map(|c| c.id())
                    && state.primary.focus.as_ref() == Some(&client)
                {
                    state.primary_offer_to(&client);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<ServerDevice, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        device: &ServerDevice,
        request: device::Request,
        (): &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        match request {
            device::Request::SetSelection { source, serial } => {
                state.primary_selected(source, serial);
            }
            device::Request::Destroy => {
                state.primary.devices.remove(&device.id());
            }
            _ => {}
        }
    }

    fn destroyed(state: &mut Self, _: ClientId, device: &ServerDevice, (): &()) {
        state.primary.devices.remove(&device.id());
        let offers: Vec<ObjectId> = state
            .primary
            .offers
            .iter()
            .filter(|(_, offer)| offer.device.id() == device.id())
            .map(|(id, _)| id.clone())
            .collect();
        for id in offers {
            state.primary_forget_offer(&id, false);
        }
    }
}

impl Dispatch<ServerSource, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        source: &ServerSource,
        request: source::Request,
        (): &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        let id = source.id();
        match request {
            source::Request::Offer { mime_type } => {
                let twin = state
                    .primary
                    .sources
                    .get(&id)
                    .and_then(|held| held.up.clone());
                if let Some(held) = state.primary.sources.get_mut(&id) {
                    if let Some(twin) = twin {
                        for name in names_added(&held.mimes, &mime_type) {
                            twin.offer(name);
                        }
                    }
                    held.mimes.push(mime_type);
                }
            }
            source::Request::Destroy => {
                if let Some(held) = state.primary.sources.remove(&id)
                    && let Some(twin) = held.up
                {
                    twin.destroy();
                }
                if state.primary.owner == Some(id) {
                    state.primary.owner = None;
                }
            }
            _ => {}
        }
    }

    fn destroyed(state: &mut Self, _: ClientId, source: &ServerSource, (): &()) {
        let id = source.id();
        state.primary.sources.remove(&id);
        if state.primary.owner == Some(id) {
            state.primary.owner = None;
        }
    }
}

impl Dispatch<ServerOffer, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        offer: &ServerOffer,
        request: offer::Request,
        (): &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        let id = offer.id();
        let Some(held) = state.primary.offers.get(&id) else {
            return;
        };
        let Some((up, mimes)) = state
            .primary
            .up_offers
            .get(&held.up)
            .map(|up| (up.offer.clone(), up.mimes.clone()))
        else {
            return;
        };
        match request {
            offer::Request::Receive { mime_type, fd } => {
                up.receive(served_as(mime_type, &mimes), fd.as_fd());
            }
            offer::Request::Destroy => state.primary_forget_offer(&id, false),
            _ => {}
        }
    }

    fn destroyed(state: &mut Self, _: ClientId, offer: &ServerOffer, (): &()) {
        state.primary_forget_offer(&offer.id(), false);
    }
}

impl wayland_client::Dispatch<ZwpPrimarySelectionDeviceManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwpPrimarySelectionDeviceManagerV1,
        _: <ZwpPrimarySelectionDeviceManagerV1 as Proxy>::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl wayland_client::Dispatch<ZwpPrimarySelectionDeviceV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwpPrimarySelectionDeviceV1,
        event: client_device::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            client_device::Event::DataOffer { offer } => {
                state.primary.up_offers.insert(
                    offer.id().protocol_id(),
                    UpOffer {
                        offer,
                        mimes: Vec::new(),
                        apps: Vec::new(),
                    },
                );
            }
            client_device::Event::Selection { id } => state.upstream_primary_selection(id),
            _ => {}
        }
    }

    event_created_child!(State, ZwpPrimarySelectionDeviceV1, [
        client_device::EVT_DATA_OFFER_OPCODE => (ZwpPrimarySelectionOfferV1, ()),
    ]);
}

impl wayland_client::Dispatch<ZwpPrimarySelectionOfferV1, ()> for State {
    fn event(
        state: &mut Self,
        offer: &ZwpPrimarySelectionOfferV1,
        event: client_offer::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let id = offer.id().protocol_id();
        if let client_offer::Event::Offer { mime_type } = event {
            let Some(up) = state.primary.up_offers.get_mut(&id) else {
                return;
            };
            let added = names_added(&up.mimes, &mime_type);
            up.mimes.push(mime_type);
            let apps = up.apps.clone();
            for app in apps {
                if let Some(offer) = state.primary.offers.get(&app) {
                    for name in &added {
                        offer.resource.offer(name.clone());
                    }
                }
            }
        }
    }
}

impl wayland_client::Dispatch<ZwpPrimarySelectionSourceV1, ()> for State {
    fn event(
        state: &mut Self,
        source: &ZwpPrimarySelectionSourceV1,
        event: client_source::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(app) = state.primary.app_source_of(source.id().protocol_id()) else {
            return;
        };
        let Some((source, mimes)) = state
            .primary
            .sources
            .get(&app)
            .map(|held| (held.resource.clone(), held.mimes.clone()))
        else {
            return;
        };
        match event {
            client_source::Event::Send { mime_type, fd } => {
                source.send(served_as(mime_type, &mimes), fd.as_fd());
            }
            client_source::Event::Cancelled => {
                source.cancelled();
                if state.primary.owner == Some(app) {
                    state.primary.owner = None;
                }
            }
            _ => {}
        }
    }
}
