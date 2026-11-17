//! Tablets: the desktop's pens, pads and their controls reach the app as they came.
//!
//! The keeper's tablet seat is raw pass-through, like its `wl_seat` (`seat.rs`): a tablet, tool
//! or pad the desktop announced is made for every app tablet seat and every describing and
//! input event is sent on unchanged, values and all. Smithay's own tablet state machine is not
//! used for the app's side: it owns focus and keeps the axes it was handed to emit them with the
//! next motion, which is right for a compositor routing input but would replay events the
//! desktop never sent and delay the ones it did. The serials are the desktop's, so a
//! `set_cursor` the app makes with one is valid on the desktop as it stands.
//!
//! The desktop is asked for a tablet seat only once an app seat exists. Until then the keeper
//! binds the manager but takes no tablets, so a pen stays a pointer to an app that never asked
//! for tablets, exactly as it would without a keeper.
//!
//! When the desktop goes away its devices go with it: tools in proximity get `proximity_out`
//! and `frame`, pads `leave`, and every tablet, tool and pad `removed`. The next desktop's
//! devices are new objects to the app.
//!
//! A cursor shape for the tool (`wp_cursor_shape_manager_v1.get_tablet_tool_v2`) is not offered
//! yet; when the cursor-shape manager lands here, its tool cursors set the same cursor surface
//! [`State::tablet_set_cursor`] mirrors.

use std::collections::HashMap;

use smithay::reexports::wayland_protocols::wp::tablet::zv2::server::{
    zwp_tablet_manager_v2::{self, ZwpTabletManagerV2},
    zwp_tablet_pad_group_v2::{self, ZwpTabletPadGroupV2},
    zwp_tablet_pad_ring_v2::{self, ZwpTabletPadRingV2},
    zwp_tablet_pad_strip_v2::{self, ZwpTabletPadStripV2},
    zwp_tablet_pad_v2::{self, ZwpTabletPadV2},
    zwp_tablet_seat_v2::{self, ZwpTabletSeatV2},
    zwp_tablet_tool_v2::{self, ZwpTabletToolV2},
    zwp_tablet_v2::{self, ZwpTabletV2},
};
use smithay::reexports::wayland_server::backend::{ClientId, ObjectId};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use wayland_client::{Connection, Proxy as _, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::wp::tablet::zv2::client as up;

use crate::state::State;

/// The tablet manager version offered to the app and bound on the desktop. The desktop offers
/// version 1; nothing here speaks the version 2 additions (bustype, dial).
pub(crate) const TABLET_VERSION: u32 = 1;

/// The app's tablet seats and the desktop's devices, by the desktop's protocol id.
#[derive(Default)]
pub(crate) struct TabletMirror {
    /// The app has been offered `zwp_tablet_manager_v2`.
    offered: bool,
    seats: Vec<Seat>,
    /// The desktop's tools and pads, for the requests the app makes with them.
    tools: HashMap<u32, up::zwp_tablet_tool_v2::ZwpTabletToolV2>,
    pads: HashMap<u32, PadUp>,
}

/// One app tablet seat and the app's objects for the desktop's devices.
struct Seat {
    resource: ZwpTabletSeatV2,
    tablets: HashMap<u32, ZwpTabletV2>,
    tools: HashMap<u32, ToolLink>,
    pads: HashMap<u32, PadLink>,
}

struct ToolLink {
    tool: ZwpTabletToolV2,
    /// The time of the last event while the tool is in proximity, for the `frame` that follows
    /// its `proximity_out` when the desktop goes away.
    proximity: Option<u32>,
}

struct PadLink {
    pad: ZwpTabletPadV2,
    /// The serial and app surface of the last `enter`, for the `leave` when the desktop goes.
    focus: Option<(u32, WlSurface)>,
    groups: HashMap<u32, GroupLink>,
}

struct GroupLink {
    group: ZwpTabletPadGroupV2,
    rings: HashMap<u32, ZwpTabletPadRingV2>,
    strips: HashMap<u32, ZwpTabletPadStripV2>,
}

/// The desktop's pad and its controls, by the desktop's protocol id.
struct PadUp {
    pad: up::zwp_tablet_pad_v2::ZwpTabletPadV2,
    rings: HashMap<u32, up::zwp_tablet_pad_ring_v2::ZwpTabletPadRingV2>,
    strips: HashMap<u32, up::zwp_tablet_pad_strip_v2::ZwpTabletPadStripV2>,
}

impl TabletMirror {
    fn seat(&mut self, seat: ZwpTabletSeatV2) {
        self.seats.push(Seat {
            resource: seat,
            tablets: HashMap::new(),
            tools: HashMap::new(),
            pads: HashMap::new(),
        });
    }

    /// The desktop went away: the devices belonged to it, so the app's tools leave proximity,
    /// pads leave their surface and every device is removed.
    pub(crate) fn hold(&mut self) {
        for seat in &mut self.seats {
            for link in seat.tools.values_mut() {
                if let Some(time) = link.proximity.take()
                    && link.tool.is_alive()
                {
                    link.tool.proximity_out();
                    link.tool.frame(time);
                }
            }
            for link in seat.pads.values_mut() {
                if let Some((serial, surface)) = link.focus.take()
                    && surface.is_alive()
                    && link.pad.is_alive()
                {
                    link.pad.leave(serial, &surface);
                }
            }
            for tablet in seat.tablets.values() {
                if tablet.is_alive() {
                    tablet.removed();
                }
            }
            for link in seat.tools.values() {
                if link.tool.is_alive() {
                    link.tool.removed();
                }
            }
            for link in seat.pads.values() {
                if link.pad.is_alive() {
                    link.pad.removed();
                }
            }
            seat.tablets.clear();
            seat.tools.clear();
            seat.pads.clear();
        }
        self.tools.clear();
        self.pads.clear();
    }

    /// For every app tablet of desktop tablet `up`, do `send`.
    fn each_tablet(&self, up: u32, mut send: impl FnMut(&ZwpTabletV2)) {
        for seat in &self.seats {
            if let Some(tablet) = seat.tablets.get(&up) {
                send(tablet);
            }
        }
    }

    /// For every app tool of desktop tool `up`, do `send`.
    fn each_tool(&mut self, up: u32, mut send: impl FnMut(&mut ToolLink)) {
        for seat in &mut self.seats {
            if let Some(link) = seat.tools.get_mut(&up) {
                send(link);
            }
        }
    }

    /// For every app pad of desktop pad `up`, do `send`.
    fn each_pad(&self, up: u32, mut send: impl FnMut(&ZwpTabletPadV2)) {
        for seat in &self.seats {
            if let Some(link) = seat.pads.get(&up) {
                send(&link.pad);
            }
        }
    }

    /// For every app group of desktop group `up`, do `send`.
    fn each_group(&self, up: u32, mut send: impl FnMut(&ZwpTabletPadGroupV2)) {
        for seat in &self.seats {
            for link in seat.pads.values() {
                if let Some(group) = link.groups.get(&up) {
                    send(&group.group);
                }
            }
        }
    }

    /// For every app ring of desktop ring `up`, do `send`.
    fn each_ring(&self, up: u32, mut send: impl FnMut(&ZwpTabletPadRingV2)) {
        for seat in &self.seats {
            for link in seat.pads.values() {
                for group in link.groups.values() {
                    if let Some(ring) = group.rings.get(&up) {
                        send(ring);
                    }
                }
            }
        }
    }

    /// For every app strip of desktop strip `up`, do `send`.
    fn each_strip(&self, up: u32, mut send: impl FnMut(&ZwpTabletPadStripV2)) {
        for seat in &self.seats {
            for link in seat.pads.values() {
                for group in link.groups.values() {
                    if let Some(strip) = group.strips.get(&up) {
                        send(strip);
                    }
                }
            }
        }
    }

    /// The desktop pad an app pad stands for.
    fn pad_up(&self, id: &ObjectId) -> Option<u32> {
        self.seats.iter().find_map(|seat| {
            seat.pads
                .iter()
                .find(|(_, link)| link.pad.id() == *id)
                .map(|(up, _)| *up)
        })
    }

    /// The desktop tool an app tool stands for, for a request addressed to the desktop's copy.
    pub(crate) fn up_tool(&self, id: &ObjectId) -> Option<up::zwp_tablet_tool_v2::ZwpTabletToolV2> {
        let up = self.tool_up(id)?;
        self.tools.get(&up).cloned()
    }

    /// The desktop tool an app tool stands for.
    fn tool_up(&self, id: &ObjectId) -> Option<u32> {
        self.seats.iter().find_map(|seat| {
            seat.tools
                .iter()
                .find(|(_, link)| link.tool.id() == *id)
                .map(|(up, _)| *up)
        })
    }

    /// The desktop ring an app ring stands for: its pad and its own id.
    fn ring_up(&self, id: &ObjectId) -> Option<(u32, u32)> {
        self.seats.iter().find_map(|seat| {
            seat.pads.iter().find_map(|(pad, link)| {
                link.groups.values().find_map(|group| {
                    group
                        .rings
                        .iter()
                        .find(|(_, ring)| ring.id() == *id)
                        .map(|(up, _)| (*pad, *up))
                })
            })
        })
    }

    /// The desktop strip an app strip stands for: its pad and its own id.
    fn strip_up(&self, id: &ObjectId) -> Option<(u32, u32)> {
        self.seats.iter().find_map(|seat| {
            seat.pads.iter().find_map(|(pad, link)| {
                link.groups.values().find_map(|group| {
                    group
                        .strips
                        .iter()
                        .find(|(_, strip)| strip.id() == *id)
                        .map(|(up, _)| (*pad, *up))
                })
            })
        })
    }

    /// The desktop pad a desktop group belongs to.
    fn group_pad(&self, group: u32) -> Option<u32> {
        self.seats.iter().find_map(|seat| {
            seat.pads
                .iter()
                .find_map(|(pad, link)| link.groups.contains_key(&group).then_some(*pad))
        })
    }

    fn tablet_removed(&mut self, id: &ObjectId) {
        for seat in &mut self.seats {
            seat.tablets.retain(|_, tablet| Resource::id(tablet) != *id);
        }
    }

    fn tool_removed(&mut self, id: &ObjectId) {
        for seat in &mut self.seats {
            seat.tools.retain(|_, link| link.tool.id() != *id);
        }
    }

    fn pad_removed(&mut self, id: &ObjectId) {
        for seat in &mut self.seats {
            seat.pads.retain(|_, link| link.pad.id() != *id);
        }
    }

    fn group_removed(&mut self, id: &ObjectId) {
        for seat in &mut self.seats {
            for link in seat.pads.values_mut() {
                link.groups.retain(|_, group| group.group.id() != *id);
            }
        }
    }

    fn ring_removed(&mut self, id: &ObjectId) {
        for seat in &mut self.seats {
            for link in seat.pads.values_mut() {
                for group in link.groups.values_mut() {
                    group.rings.retain(|_, ring| ring.id() != *id);
                }
            }
        }
    }

    fn strip_removed(&mut self, id: &ObjectId) {
        for seat in &mut self.seats {
            for link in seat.pads.values_mut() {
                for group in link.groups.values_mut() {
                    group.strips.retain(|_, strip| strip.id() != *id);
                }
            }
        }
    }

    fn seat_destroyed(&mut self, id: &ObjectId) {
        self.seats.retain(|seat| seat.resource.id() != *id);
    }
}

impl State {
    /// Offer the app the tablet manager the desktop offered.
    pub(crate) fn offer_tablet_manager(&mut self) {
        if self.tablets.offered {
            return;
        }
        self.tablets.offered = true;
        self.dh
            .create_global::<State, ZwpTabletManagerV2, ()>(TABLET_VERSION, ());
    }

    /// An app took a tablet seat: the desktop's devices can follow.
    pub(crate) fn ensure_upstream_tablet_seat(&mut self) {
        let Some(up) = self.up.as_mut() else {
            return;
        };
        if up.tablet_seat.is_some() {
            return;
        }
        let (Some(manager), Some(seat)) = (up.tablet_manager.as_ref(), up.seat.as_ref()) else {
            return;
        };
        up.tablet_seat = Some(manager.get_tablet_seat(seat, &up.qh, ()));
    }

    /// The app asked for a cursor on a tool: mirror the surface and pass the desktop's serial on.
    pub(crate) fn tablet_set_cursor(
        &mut self,
        tool: &ObjectId,
        serial: u32,
        surface: Option<&WlSurface>,
        x: i32,
        y: i32,
    ) {
        let Some(up) = self.tablets.tool_up(tool) else {
            return;
        };
        let up_surface = self.mirror_cursor(surface);
        if surface.is_some() && up_surface.is_none() {
            return;
        }
        if let Some(tool) = self.tablets.tools.get(&up) {
            tool.set_cursor(serial, up_surface.as_ref(), x, y);
        }
    }

    /// A tablet object for every app seat, and the `tablet_added` announcing it.
    fn announce_tablet(&mut self, up: u32) {
        let dh = self.dh.clone();
        for seat in &mut self.tablets.seats {
            let Some(client) = seat.resource.client() else {
                continue;
            };
            let Ok(tablet) =
                client.create_resource::<ZwpTabletV2, _, State>(&dh, seat.resource.version(), ())
            else {
                continue;
            };
            seat.resource.tablet_added(&tablet);
            seat.tablets.insert(up, tablet);
        }
    }

    fn announce_tool(&mut self, up: u32) {
        let dh = self.dh.clone();
        for seat in &mut self.tablets.seats {
            let Some(client) = seat.resource.client() else {
                continue;
            };
            let Ok(tool) = client.create_resource::<ZwpTabletToolV2, _, State>(
                &dh,
                seat.resource.version(),
                (),
            ) else {
                continue;
            };
            seat.resource.tool_added(&tool);
            seat.tools.insert(
                up,
                ToolLink {
                    tool,
                    proximity: None,
                },
            );
        }
    }

    fn announce_pad(&mut self, up: u32) {
        let dh = self.dh.clone();
        for seat in &mut self.tablets.seats {
            let Some(client) = seat.resource.client() else {
                continue;
            };
            let Ok(pad) = client.create_resource::<ZwpTabletPadV2, _, State>(
                &dh,
                seat.resource.version(),
                (),
            ) else {
                continue;
            };
            seat.resource.pad_added(&pad);
            seat.pads.insert(
                up,
                PadLink {
                    pad,
                    focus: None,
                    groups: HashMap::new(),
                },
            );
        }
    }

    /// A group object for every app pad of desktop pad `pad`, told of desktop group `up`.
    fn announce_group(&mut self, pad: u32, up: u32) {
        let dh = self.dh.clone();
        for seat in &mut self.tablets.seats {
            let Some(link) = seat.pads.get_mut(&pad) else {
                continue;
            };
            let Some(client) = seat.resource.client() else {
                continue;
            };
            let Ok(group) = client.create_resource::<ZwpTabletPadGroupV2, _, State>(
                &dh,
                seat.resource.version(),
                (),
            ) else {
                continue;
            };
            link.pad.group(&group);
            link.groups.insert(
                up,
                GroupLink {
                    group,
                    rings: HashMap::new(),
                    strips: HashMap::new(),
                },
            );
        }
    }

    /// A ring object for every app group of desktop group `group`, told of desktop ring `up`.
    fn announce_ring(&mut self, group: u32, up: u32) {
        let dh = self.dh.clone();
        for seat in &mut self.tablets.seats {
            let Some(client) = seat.resource.client() else {
                continue;
            };
            for link in seat.pads.values_mut() {
                let Some(link_group) = link.groups.get_mut(&group) else {
                    continue;
                };
                let Ok(ring) = client.create_resource::<ZwpTabletPadRingV2, _, State>(
                    &dh,
                    seat.resource.version(),
                    (),
                ) else {
                    continue;
                };
                link_group.group.ring(&ring);
                link_group.rings.insert(up, ring);
            }
        }
    }

    /// A strip object for every app group of desktop group `group`, told of desktop strip `up`.
    fn announce_strip(&mut self, group: u32, up: u32) {
        let dh = self.dh.clone();
        for seat in &mut self.tablets.seats {
            let Some(client) = seat.resource.client() else {
                continue;
            };
            for link in seat.pads.values_mut() {
                let Some(link_group) = link.groups.get_mut(&group) else {
                    continue;
                };
                let Ok(strip) = client.create_resource::<ZwpTabletPadStripV2, _, State>(
                    &dh,
                    seat.resource.version(),
                    (),
                ) else {
                    continue;
                };
                link_group.group.strip(&strip);
                link_group.strips.insert(up, strip);
            }
        }
    }
}

impl GlobalDispatch<ZwpTabletManagerV2, ()> for State {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<ZwpTabletManagerV2>,
        (): &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ZwpTabletManagerV2, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &ZwpTabletManagerV2,
        request: zwp_tablet_manager_v2::Request,
        (): &(),
        _: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let zwp_tablet_manager_v2::Request::GetTabletSeat { tablet_seat, .. } = request {
            let seat = data_init.init(tablet_seat, ());
            state.tablets.seat(seat);
            state.ensure_upstream_tablet_seat();
        }
    }
}

impl Dispatch<ZwpTabletSeatV2, ()> for State {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &ZwpTabletSeatV2,
        _: zwp_tablet_seat_v2::Request,
        (): &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }

    fn destroyed(state: &mut Self, _: ClientId, seat: &ZwpTabletSeatV2, (): &()) {
        state.tablets.seat_destroyed(&seat.id());
    }
}

impl Dispatch<ZwpTabletV2, ()> for State {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &ZwpTabletV2,
        _: zwp_tablet_v2::Request,
        (): &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }

    fn destroyed(state: &mut Self, _: ClientId, tablet: &ZwpTabletV2, (): &()) {
        state.tablets.tablet_removed(&Resource::id(tablet));
    }
}

impl Dispatch<ZwpTabletToolV2, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        tool: &ZwpTabletToolV2,
        request: zwp_tablet_tool_v2::Request,
        (): &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        if let zwp_tablet_tool_v2::Request::SetCursor {
            serial,
            surface,
            hotspot_x,
            hotspot_y,
        } = request
        {
            state.tablet_set_cursor(&tool.id(), serial, surface.as_ref(), hotspot_x, hotspot_y);
        }
    }

    fn destroyed(state: &mut Self, _: ClientId, tool: &ZwpTabletToolV2, (): &()) {
        state.tablets.tool_removed(&tool.id());
    }
}

impl Dispatch<ZwpTabletPadV2, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        pad: &ZwpTabletPadV2,
        request: zwp_tablet_pad_v2::Request,
        (): &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        if let zwp_tablet_pad_v2::Request::SetFeedback {
            button,
            description,
            serial,
        } = request
            && let Some(up) = state.tablets.pad_up(&pad.id())
            && let Some(up) = state.tablets.pads.get(&up)
        {
            up.pad.set_feedback(button, description, serial);
        }
    }

    fn destroyed(state: &mut Self, _: ClientId, pad: &ZwpTabletPadV2, (): &()) {
        state.tablets.pad_removed(&pad.id());
    }
}

impl Dispatch<ZwpTabletPadGroupV2, ()> for State {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &ZwpTabletPadGroupV2,
        _: zwp_tablet_pad_group_v2::Request,
        (): &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }

    fn destroyed(state: &mut Self, _: ClientId, group: &ZwpTabletPadGroupV2, (): &()) {
        state.tablets.group_removed(&group.id());
    }
}

impl Dispatch<ZwpTabletPadRingV2, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        ring: &ZwpTabletPadRingV2,
        request: zwp_tablet_pad_ring_v2::Request,
        (): &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        if let zwp_tablet_pad_ring_v2::Request::SetFeedback {
            description,
            serial,
        } = request
            && let Some((pad, up)) = state.tablets.ring_up(&ring.id())
            && let Some(up) = state
                .tablets
                .pads
                .get(&pad)
                .and_then(|pad| pad.rings.get(&up))
        {
            up.set_feedback(description, serial);
        }
    }

    fn destroyed(state: &mut Self, _: ClientId, ring: &ZwpTabletPadRingV2, (): &()) {
        state.tablets.ring_removed(&ring.id());
    }
}

impl Dispatch<ZwpTabletPadStripV2, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        strip: &ZwpTabletPadStripV2,
        request: zwp_tablet_pad_strip_v2::Request,
        (): &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        if let zwp_tablet_pad_strip_v2::Request::SetFeedback {
            description,
            serial,
        } = request
            && let Some((pad, up)) = state.tablets.strip_up(&strip.id())
            && let Some(up) = state
                .tablets
                .pads
                .get(&pad)
                .and_then(|pad| pad.strips.get(&up))
        {
            up.set_feedback(description, serial);
        }
    }

    fn destroyed(state: &mut Self, _: ClientId, strip: &ZwpTabletPadStripV2, (): &()) {
        state.tablets.strip_removed(&strip.id());
    }
}

delegate_noop!(State: ignore up::zwp_tablet_manager_v2::ZwpTabletManagerV2);

impl wayland_client::Dispatch<up::zwp_tablet_seat_v2::ZwpTabletSeatV2, ()> for State {
    fn event(
        state: &mut Self,
        _: &up::zwp_tablet_seat_v2::ZwpTabletSeatV2,
        event: up::zwp_tablet_seat_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            up::zwp_tablet_seat_v2::Event::TabletAdded { id } => {
                state.announce_tablet(id.id().protocol_id());
            }
            up::zwp_tablet_seat_v2::Event::ToolAdded { id, .. } => {
                let up = id.id().protocol_id();
                state.tablets.tools.insert(up, id);
                state.announce_tool(up);
            }
            up::zwp_tablet_seat_v2::Event::PadAdded { id, .. } => {
                let up = id.id().protocol_id();
                state.tablets.pads.insert(
                    up,
                    PadUp {
                        pad: id,
                        rings: HashMap::new(),
                        strips: HashMap::new(),
                    },
                );
                state.announce_pad(up);
            }
            _ => {}
        }
    }

    wayland_client::event_created_child!(State, up::zwp_tablet_seat_v2::ZwpTabletSeatV2, [
        up::zwp_tablet_seat_v2::EVT_TABLET_ADDED_OPCODE => (up::zwp_tablet_v2::ZwpTabletV2, ()),
        up::zwp_tablet_seat_v2::EVT_TOOL_ADDED_OPCODE => (up::zwp_tablet_tool_v2::ZwpTabletToolV2, ()),
        up::zwp_tablet_seat_v2::EVT_PAD_ADDED_OPCODE => (up::zwp_tablet_pad_v2::ZwpTabletPadV2, ()),
    ]);
}

impl wayland_client::Dispatch<up::zwp_tablet_v2::ZwpTabletV2, ()> for State {
    fn event(
        state: &mut Self,
        tablet: &up::zwp_tablet_v2::ZwpTabletV2,
        event: up::zwp_tablet_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let up = tablet.id().protocol_id();
        match event {
            up::zwp_tablet_v2::Event::Name { name } => {
                state
                    .tablets
                    .each_tablet(up, |tablet| tablet.name(name.clone()));
            }
            up::zwp_tablet_v2::Event::Id { vid, pid } => {
                state.tablets.each_tablet(up, |tablet| tablet.id(vid, pid));
            }
            up::zwp_tablet_v2::Event::Path { path } => {
                state
                    .tablets
                    .each_tablet(up, |tablet| tablet.path(path.clone()));
            }
            up::zwp_tablet_v2::Event::Done => {
                state.tablets.each_tablet(up, ZwpTabletV2::done);
            }
            up::zwp_tablet_v2::Event::Removed => {
                state.tablets.each_tablet(up, ZwpTabletV2::removed);
            }
            _ => {}
        }
    }
}

impl State {
    /// One tool event that moves or acts: proximity, the tip, buttons and the axes.
    fn forward_tool_input(&mut self, up: u32, event: up::zwp_tablet_tool_v2::Event) {
        use up::zwp_tablet_tool_v2 as tool_up;
        match event {
            tool_up::Event::ProximityIn {
                serial,
                tablet,
                surface,
            } => {
                let Some(surface) = self.app_surface(&surface) else {
                    return;
                };
                let tablet = tablet.id().protocol_id();
                for seat in &mut self.tablets.seats {
                    let Some(tablet) = seat.tablets.get(&tablet).cloned() else {
                        continue;
                    };
                    let Some(link) = seat.tools.get_mut(&up) else {
                        continue;
                    };
                    link.tool.proximity_in(serial, &tablet, &surface);
                    link.proximity = Some(0);
                }
            }
            tool_up::Event::ProximityOut => self.tablets.each_tool(up, |link| {
                link.proximity = None;
                link.tool.proximity_out();
            }),
            tool_up::Event::Down { serial } => self.tablets.each_tool(up, |link| {
                link.proximity.get_or_insert(0);
                link.tool.down(serial);
            }),
            tool_up::Event::Up => self.tablets.each_tool(up, |link| link.tool.up()),
            tool_up::Event::Motion { x, y } => {
                self.tablets.each_tool(up, |link| link.tool.motion(x, y));
            }
            tool_up::Event::Pressure { pressure } => {
                self.tablets
                    .each_tool(up, |link| link.tool.pressure(pressure));
            }
            tool_up::Event::Distance { distance } => {
                self.tablets
                    .each_tool(up, |link| link.tool.distance(distance));
            }
            tool_up::Event::Tilt { tilt_x, tilt_y } => {
                self.tablets
                    .each_tool(up, |link| link.tool.tilt(tilt_x, tilt_y));
            }
            tool_up::Event::Rotation { degrees } => {
                self.tablets
                    .each_tool(up, |link| link.tool.rotation(degrees));
            }
            tool_up::Event::Slider { position } => {
                self.tablets
                    .each_tool(up, |link| link.tool.slider(position));
            }
            tool_up::Event::Wheel { degrees, clicks } => {
                self.tablets
                    .each_tool(up, |link| link.tool.wheel(degrees, clicks));
            }
            tool_up::Event::Button {
                serial,
                button,
                state: WEnum::Value(button_state),
            } => {
                if let Ok(button_state) =
                    zwp_tablet_tool_v2::ButtonState::try_from(u32::from(button_state))
                {
                    self.tablets.each_tool(up, |link| {
                        link.proximity.get_or_insert(0);
                        link.tool.button(serial, button, button_state);
                    });
                }
            }
            tool_up::Event::Frame { time } => self.tablets.each_tool(up, |link| {
                link.proximity = link.proximity.map(|_| time);
                link.tool.frame(time);
            }),
            _ => {}
        }
    }
}

impl wayland_client::Dispatch<up::zwp_tablet_tool_v2::ZwpTabletToolV2, ()> for State {
    fn event(
        state: &mut Self,
        tool: &up::zwp_tablet_tool_v2::ZwpTabletToolV2,
        event: up::zwp_tablet_tool_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use up::zwp_tablet_tool_v2 as tool_up;
        let up = tool.id().protocol_id();
        match event {
            tool_up::Event::Type {
                tool_type: WEnum::Value(tool_type),
            } => {
                if let Ok(tool_type) = zwp_tablet_tool_v2::Type::try_from(u32::from(tool_type)) {
                    state
                        .tablets
                        .each_tool(up, |link| link.tool._type(tool_type));
                }
            }
            tool_up::Event::HardwareSerial {
                hardware_serial_hi,
                hardware_serial_lo,
            } => state.tablets.each_tool(up, |link| {
                link.tool
                    .hardware_serial(hardware_serial_hi, hardware_serial_lo);
            }),
            tool_up::Event::HardwareIdWacom {
                hardware_id_hi,
                hardware_id_lo,
            } => state.tablets.each_tool(up, |link| {
                link.tool.hardware_id_wacom(hardware_id_hi, hardware_id_lo);
            }),
            tool_up::Event::Capability {
                capability: WEnum::Value(capability),
            } => {
                if let Ok(capability) =
                    zwp_tablet_tool_v2::Capability::try_from(u32::from(capability))
                {
                    state
                        .tablets
                        .each_tool(up, |link| link.tool.capability(capability));
                }
            }
            tool_up::Event::Done => state.tablets.each_tool(up, |link| link.tool.done()),
            tool_up::Event::Removed => {
                state.tablets.each_tool(up, |link| link.tool.removed());
                state.tablets.tools.remove(&up);
            }
            input => state.forward_tool_input(up, input),
        }
    }
}

impl wayland_client::Dispatch<up::zwp_tablet_pad_v2::ZwpTabletPadV2, ()> for State {
    fn event(
        state: &mut Self,
        pad: &up::zwp_tablet_pad_v2::ZwpTabletPadV2,
        event: up::zwp_tablet_pad_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use up::zwp_tablet_pad_v2 as pad_up;
        let up = pad.id().protocol_id();
        match event {
            pad_up::Event::Group { pad_group } => {
                state.announce_group(up, pad_group.id().protocol_id());
            }
            pad_up::Event::Path { path } => {
                state.tablets.each_pad(up, |pad| pad.path(path.clone()));
            }
            pad_up::Event::Buttons { buttons } => {
                state.tablets.each_pad(up, |pad| pad.buttons(buttons));
            }
            pad_up::Event::Done => state.tablets.each_pad(up, ZwpTabletPadV2::done),
            pad_up::Event::Button {
                time,
                button,
                state: WEnum::Value(button_state),
            } => {
                if let Ok(button_state) =
                    zwp_tablet_pad_v2::ButtonState::try_from(u32::from(button_state))
                {
                    state
                        .tablets
                        .each_pad(up, |pad| pad.button(time, button, button_state));
                }
            }
            pad_up::Event::Enter {
                serial,
                tablet,
                surface,
            } => {
                let Some(surface) = state.app_surface(&surface) else {
                    return;
                };
                let tablet = tablet.id().protocol_id();
                for seat in &mut state.tablets.seats {
                    let Some(app_tablet) = seat.tablets.get(&tablet).cloned() else {
                        continue;
                    };
                    let Some(link) = seat.pads.get_mut(&up) else {
                        continue;
                    };
                    link.pad.enter(serial, &app_tablet, &surface);
                    link.focus = Some((serial, surface.clone()));
                }
            }
            pad_up::Event::Leave { serial, surface } => {
                let Some(surface) = state.app_surface(&surface) else {
                    return;
                };
                for seat in &mut state.tablets.seats {
                    if let Some(link) = seat.pads.get_mut(&up) {
                        link.pad.leave(serial, &surface);
                        link.focus = None;
                    }
                }
            }
            pad_up::Event::Removed => {
                state.tablets.each_pad(up, ZwpTabletPadV2::removed);
                state.tablets.pads.remove(&up);
            }
            _ => {}
        }
    }

    wayland_client::event_created_child!(State, up::zwp_tablet_pad_v2::ZwpTabletPadV2, [
        up::zwp_tablet_pad_v2::EVT_GROUP_OPCODE => (up::zwp_tablet_pad_group_v2::ZwpTabletPadGroupV2, ()),
    ]);
}

impl wayland_client::Dispatch<up::zwp_tablet_pad_group_v2::ZwpTabletPadGroupV2, ()> for State {
    fn event(
        state: &mut Self,
        group: &up::zwp_tablet_pad_group_v2::ZwpTabletPadGroupV2,
        event: up::zwp_tablet_pad_group_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use up::zwp_tablet_pad_group_v2 as group_up;
        let up = group.id().protocol_id();
        match event {
            group_up::Event::Ring { ring } => {
                let up_ring = ring.id().protocol_id();
                if let Some(pad) = state.tablets.group_pad(up)
                    && let Some(pad) = state.tablets.pads.get_mut(&pad)
                {
                    pad.rings.insert(up_ring, ring);
                }
                state.announce_ring(up, up_ring);
            }
            group_up::Event::Strip { strip } => {
                let up_strip = strip.id().protocol_id();
                if let Some(pad) = state.tablets.group_pad(up)
                    && let Some(pad) = state.tablets.pads.get_mut(&pad)
                {
                    pad.strips.insert(up_strip, strip);
                }
                state.announce_strip(up, up_strip);
            }
            group_up::Event::Buttons { buttons } => {
                state
                    .tablets
                    .each_group(up, |group| group.buttons(buttons.clone()));
            }
            group_up::Event::Modes { modes } => {
                state.tablets.each_group(up, |group| group.modes(modes));
            }
            group_up::Event::Done => state.tablets.each_group(up, ZwpTabletPadGroupV2::done),
            group_up::Event::ModeSwitch { time, serial, mode } => state
                .tablets
                .each_group(up, |group| group.mode_switch(time, serial, mode)),
            _ => {}
        }
    }

    wayland_client::event_created_child!(State, up::zwp_tablet_pad_group_v2::ZwpTabletPadGroupV2, [
        up::zwp_tablet_pad_group_v2::EVT_RING_OPCODE => (up::zwp_tablet_pad_ring_v2::ZwpTabletPadRingV2, ()),
        up::zwp_tablet_pad_group_v2::EVT_STRIP_OPCODE => (up::zwp_tablet_pad_strip_v2::ZwpTabletPadStripV2, ()),
    ]);
}

impl wayland_client::Dispatch<up::zwp_tablet_pad_ring_v2::ZwpTabletPadRingV2, ()> for State {
    fn event(
        state: &mut Self,
        ring: &up::zwp_tablet_pad_ring_v2::ZwpTabletPadRingV2,
        event: up::zwp_tablet_pad_ring_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use up::zwp_tablet_pad_ring_v2 as ring_up;
        let up = ring.id().protocol_id();
        match event {
            ring_up::Event::Source {
                source: WEnum::Value(source),
            } => {
                if let Ok(source) = zwp_tablet_pad_ring_v2::Source::try_from(u32::from(source)) {
                    state.tablets.each_ring(up, |ring| ring.source(source));
                }
            }
            ring_up::Event::Angle { degrees } => {
                state.tablets.each_ring(up, |ring| ring.angle(degrees));
            }
            ring_up::Event::Stop => state.tablets.each_ring(up, ZwpTabletPadRingV2::stop),
            ring_up::Event::Frame { time } => {
                state.tablets.each_ring(up, |ring| ring.frame(time));
            }
            _ => {}
        }
    }
}

impl wayland_client::Dispatch<up::zwp_tablet_pad_strip_v2::ZwpTabletPadStripV2, ()> for State {
    fn event(
        state: &mut Self,
        strip: &up::zwp_tablet_pad_strip_v2::ZwpTabletPadStripV2,
        event: up::zwp_tablet_pad_strip_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use up::zwp_tablet_pad_strip_v2 as strip_up;
        let up = strip.id().protocol_id();
        match event {
            strip_up::Event::Source {
                source: WEnum::Value(source),
            } => {
                if let Ok(source) = zwp_tablet_pad_strip_v2::Source::try_from(u32::from(source)) {
                    state.tablets.each_strip(up, |strip| strip.source(source));
                }
            }
            strip_up::Event::Position { position } => {
                state
                    .tablets
                    .each_strip(up, |strip| strip.position(position));
            }
            strip_up::Event::Stop => state.tablets.each_strip(up, ZwpTabletPadStripV2::stop),
            strip_up::Event::Frame { time } => {
                state.tablets.each_strip(up, |strip| strip.frame(time));
            }
            _ => {}
        }
    }
}
