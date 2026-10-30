use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use wayland_protocols::wp::tablet::zv2::server::{
    zwp_tablet_pad_group_v2::{self, ZwpTabletPadGroupV2},
    zwp_tablet_pad_ring_v2::{self, ZwpTabletPadRingV2},
    zwp_tablet_pad_strip_v2::{self, ZwpTabletPadStripV2},
    zwp_tablet_pad_v2::{self, ZwpTabletPadV2},
    zwp_tablet_seat_v2::ZwpTabletSeatV2,
};
use wayland_server::{
    backend::ClientId, protocol::wl_surface::WlSurface, Client, DataInit, Dispatch, DisplayHandle, Resource,
    Weak,
};

use crate::backend::input::ButtonState;
use crate::utils::{Serial, SERIAL_COUNTER};

use super::tablet::TabletHandle;
use super::TabletManagerState;

/// Description of one group of a tablet pad
///
/// A group is a set of buttons, rings and strips that share one mode,
/// usually the controls on one side of the tablet.
#[derive(Debug, Clone, Default, Hash, Eq, PartialEq)]
pub struct TabletPadGroupDescriptor {
    /// Buttons of the group, by pad button index (from zero)
    pub buttons: Vec<u32>,
    /// Rings of the group, by pad ring index (from zero)
    pub rings: Vec<u32>,
    /// Strips of the group, by pad strip index (from zero)
    pub strips: Vec<u32>,
    /// Number of modes the group switches between (0 or 1 for none)
    pub modes: u32,
}

/// Description of a tablet pad device
#[derive(Debug, Clone, Default, Hash, Eq, PartialEq)]
pub struct TabletPadDescriptor {
    /// Pad device name
    pub name: String,
    /// Path to the device
    pub syspath: Option<PathBuf>,
    /// Number of buttons on the pad
    pub buttons: u32,
    /// Groups of the pad; a pad without groups is announced with one group holding everything
    pub groups: Vec<TabletPadGroupDescriptor>,
}

impl TabletPadDescriptor {
    fn groups(&self) -> Vec<TabletPadGroupDescriptor> {
        if self.groups.is_empty() {
            vec![TabletPadGroupDescriptor {
                buttons: (0..self.buttons).collect(),
                ..Default::default()
            }]
        } else {
            self.groups.clone()
        }
    }
}

#[derive(Debug)]
struct PadInstance {
    pad: Weak<ZwpTabletPadV2>,
    groups: Vec<Weak<ZwpTabletPadGroupV2>>,
    rings: Vec<(u32, Weak<ZwpTabletPadRingV2>)>,
    strips: Vec<(u32, Weak<ZwpTabletPadStripV2>)>,
}

#[derive(Debug)]
struct TabletPad {
    desc: TabletPadDescriptor,
    tablet: TabletHandle,
    instances: Vec<PadInstance>,
    focus: Option<WlSurface>,
    modes: Vec<u32>,
}

impl TabletPad {
    fn focused(&self) -> Option<&PadInstance> {
        let focus = self.focus.as_ref()?;
        self.instances
            .iter()
            .find(|i| i.pad.id().same_client_as(&focus.id()))
    }

    fn enter(&self, serial: Serial, time: u32) {
        let (Some(focus), Some(instance)) = (self.focus.as_ref(), self.focused()) else {
            return;
        };
        let Ok(pad) = instance.pad.upgrade() else {
            return;
        };
        self.tablet.with_focused_tablet(focus, |wl_tablet| {
            pad.enter(serial.into(), wl_tablet, focus);
        });
        // The current mode of every group follows enter (required by protocol)
        for (group, mode) in instance.groups.iter().zip(&self.modes) {
            if let Ok(group) = group.upgrade() {
                group.mode_switch(time, serial.into(), *mode);
            }
        }
    }

    fn leave(&self, serial: Serial) {
        let (Some(focus), Some(instance)) = (self.focus.as_ref(), self.focused()) else {
            return;
        };
        // A destroyed surface takes the focus with it, without a leave
        if let (Ok(pad), true) = (instance.pad.upgrade(), focus.is_alive()) {
            pad.leave(serial.into(), focus);
        }
    }
}

/// Handle to a tablet pad device
///
/// A pad is the set of buttons, rings and strips on a tablet (or a remote attached to one).
/// Its events go to the client of the surface it is focused on, see [`TabletPadHandle::set_focus`].
#[derive(Debug, Clone)]
pub struct TabletPadHandle {
    inner: Arc<Mutex<TabletPad>>,
}

impl TabletPadHandle {
    pub(super) fn new(desc: &TabletPadDescriptor, tablet: &TabletHandle) -> Self {
        let modes = vec![0; desc.groups().len()];
        Self {
            inner: Arc::new(Mutex::new(TabletPad {
                desc: desc.clone(),
                tablet: tablet.clone(),
                instances: Vec::new(),
                focus: None,
                modes,
            })),
        }
    }

    pub(super) fn new_instance<D>(&self, client: &Client, dh: &DisplayHandle, seat: &ZwpTabletSeatV2)
    where
        D: Dispatch<ZwpTabletPadV2, TabletPadUserData>,
        D: Dispatch<ZwpTabletPadGroupV2, TabletPadUserData>,
        D: Dispatch<ZwpTabletPadRingV2, TabletPadUserData>,
        D: Dispatch<ZwpTabletPadStripV2, TabletPadUserData>,
        D: 'static,
    {
        let version = seat.version();
        let data = || TabletPadUserData { handle: self.clone() };
        let mut inner = self.inner.lock().unwrap();

        let Ok(pad) = client.create_resource::<ZwpTabletPadV2, _, D>(dh, version, data()) else {
            return;
        };
        seat.pad_added(&pad);

        let mut instance = PadInstance {
            pad: pad.downgrade(),
            groups: Vec::new(),
            rings: Vec::new(),
            strips: Vec::new(),
        };
        for desc in inner.desc.groups() {
            let Ok(group) = client.create_resource::<ZwpTabletPadGroupV2, _, D>(dh, version, data()) else {
                continue;
            };
            pad.group(&group);
            group.buttons(desc.buttons.iter().flat_map(|b| b.to_ne_bytes()).collect());
            for index in desc.rings {
                if let Ok(ring) = client.create_resource::<ZwpTabletPadRingV2, _, D>(dh, version, data()) {
                    group.ring(&ring);
                    instance.rings.push((index, ring.downgrade()));
                }
            }
            for index in desc.strips {
                if let Ok(strip) = client.create_resource::<ZwpTabletPadStripV2, _, D>(dh, version, data())
                {
                    group.strip(&strip);
                    instance.strips.push((index, strip.downgrade()));
                }
            }
            if desc.modes > 1 {
                group.modes(desc.modes);
            }
            group.done();
            instance.groups.push(group.downgrade());
        }
        if let Some(syspath) = inner.desc.syspath.as_ref().and_then(|p| p.to_str()) {
            pad.path(syspath.to_owned());
        }
        if inner.desc.buttons > 0 {
            pad.buttons(inner.desc.buttons);
        }
        pad.done();

        inner.instances.push(instance);

        // A client that binds while one of its surfaces has the focus enters it at once
        if inner
            .focus
            .as_ref()
            .is_some_and(|focus| focus.id().same_client_as(&pad.id()))
        {
            inner.enter(SERIAL_COUNTER.next_serial(), 0);
        }
    }

    /// Focus the pad on a surface, or on none
    ///
    /// The pad leaves the surface it was focused on and enters the new one; the client also gets the current
    /// mode of every group. Compositors usually have the pad follow the keyboard focus.
    pub fn set_focus(&self, focus: Option<&WlSurface>, serial: Serial, time: u32) {
        let mut inner = self.inner.lock().unwrap();
        if inner.focus.as_ref() == focus {
            return;
        }
        inner.leave(serial);
        inner.focus = focus.cloned();
        inner.enter(serial, time);
    }

    /// Whether the pad's events reach a client: it is focused on a surface whose client bound it
    pub fn has_focused_client(&self) -> bool {
        self.inner.lock().unwrap().focused().is_some()
    }

    /// A button (by index, from zero) was pressed or released
    pub fn button(&self, button: u32, state: ButtonState, time: u32) {
        let inner = self.inner.lock().unwrap();
        if let Some(pad) = inner.focused().and_then(|i| i.pad.upgrade().ok()) {
            let state = match state {
                ButtonState::Pressed => zwp_tablet_pad_v2::ButtonState::Pressed,
                ButtonState::Released => zwp_tablet_pad_v2::ButtonState::Released,
            };
            pad.button(time, button, state);
        }
    }

    /// A finger is on a ring (by index) at `degrees` clockwise from the top, or lifted (`None`)
    pub fn ring(&self, ring: u32, degrees: Option<f64>, time: u32) {
        let inner = self.inner.lock().unwrap();
        let Some(ring) = inner
            .focused()
            .and_then(|i| i.rings.iter().find(|(index, _)| *index == ring))
            .and_then(|(_, ring)| ring.upgrade().ok())
        else {
            return;
        };
        match degrees {
            Some(degrees) => {
                ring.source(zwp_tablet_pad_ring_v2::Source::Finger);
                ring.angle(degrees);
            }
            None => ring.stop(),
        }
        ring.frame(time);
    }

    /// A finger is on a strip (by index) at `position` (`0.0` top or left to `1.0`), or lifted (`None`)
    pub fn strip(&self, strip: u32, position: Option<f64>, time: u32) {
        let inner = self.inner.lock().unwrap();
        let Some(strip) = inner
            .focused()
            .and_then(|i| i.strips.iter().find(|(index, _)| *index == strip))
            .and_then(|(_, strip)| strip.upgrade().ok())
        else {
            return;
        };
        match position {
            Some(position) => {
                strip.source(zwp_tablet_pad_strip_v2::Source::Finger);
                strip.position((position.clamp(0.0, 1.0) * 65535.0).round() as u32);
            }
            None => strip.stop(),
        }
        strip.frame(time);
    }

    /// A group (by index) switched to another mode
    pub fn mode_switch(&self, group: usize, mode: u32, serial: Serial, time: u32) {
        let mut inner = self.inner.lock().unwrap();
        let Some(current) = inner.modes.get_mut(group) else {
            return;
        };
        *current = mode;
        if let Some(group) = inner
            .focused()
            .and_then(|i| i.groups.get(group))
            .and_then(|g| g.upgrade().ok())
        {
            group.mode_switch(time, serial.into(), mode);
        }
    }

    pub(super) fn removed(&self) {
        let mut inner = self.inner.lock().unwrap();
        for instance in inner.instances.drain(..) {
            if let Ok(pad) = instance.pad.upgrade() {
                pad.removed();
            }
        }
        inner.focus = None;
    }
}

/// User data of the ZwpTabletPadV2 object and its groups, rings and strips
#[derive(Debug)]
pub struct TabletPadUserData {
    handle: TabletPadHandle,
}

impl<D> Dispatch<ZwpTabletPadV2, TabletPadUserData, D> for TabletManagerState
where
    D: Dispatch<ZwpTabletPadV2, TabletPadUserData>,
    D: 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _pad: &ZwpTabletPadV2,
        _request: zwp_tablet_pad_v2::Request,
        _data: &TabletPadUserData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        // Button feedback (set_feedback) is a hint for on-screen displays; nothing to do
    }

    fn destroyed(_state: &mut D, _client: ClientId, pad: &ZwpTabletPadV2, data: &TabletPadUserData) {
        data.handle
            .inner
            .lock()
            .unwrap()
            .instances
            .retain(|i| i.pad.id() != pad.id());
    }
}

impl<D> Dispatch<ZwpTabletPadGroupV2, TabletPadUserData, D> for TabletManagerState
where
    D: Dispatch<ZwpTabletPadGroupV2, TabletPadUserData>,
    D: 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _group: &ZwpTabletPadGroupV2,
        _request: zwp_tablet_pad_group_v2::Request,
        _data: &TabletPadUserData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
    }
}

impl<D> Dispatch<ZwpTabletPadRingV2, TabletPadUserData, D> for TabletManagerState
where
    D: Dispatch<ZwpTabletPadRingV2, TabletPadUserData>,
    D: 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _ring: &ZwpTabletPadRingV2,
        _request: zwp_tablet_pad_ring_v2::Request,
        _data: &TabletPadUserData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
    }
}

impl<D> Dispatch<ZwpTabletPadStripV2, TabletPadUserData, D> for TabletManagerState
where
    D: Dispatch<ZwpTabletPadStripV2, TabletPadUserData>,
    D: 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _strip: &ZwpTabletPadStripV2,
        _request: zwp_tablet_pad_strip_v2::Request,
        _data: &TabletPadUserData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
    }
}
