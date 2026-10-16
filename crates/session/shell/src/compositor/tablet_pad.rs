//! Tablet pads for clients.
//!
//! A pad's buttons, rings and strips follow keyboard focus. When the focused
//! app took a tablet seat, it gets them over `zwp_tablet_pad_v2`, exactly as
//! the hardware reports them, and switches its own tools per mode. Anywhere
//! else the pad's saved settings decide: a mapped button runs its shortcut,
//! and a ring or strip scrolls, zooms, resizes the brush or steps the volume
//! or brightness ([`dial_events`]).
//!
//! A focused tablet-aware app may say what each control does in its current
//! mode (`set_feedback`); the pad map ([`State::tablet_pad_map`]) shows
//! those labels, and Tilo's own mapping where there are none.
//!
//! Smithay's tablet manager knows pens only, so the pad side of the protocol
//! lives here. Each pad is announced to a client with a `zwp_tablet_v2` of
//! its own (named after the pad device, as libinput reports it separately
//! from the pen), which is the tablet its `enter` events name.

use std::collections::HashMap;

use settings_store::hardware::{KeyChord, PadDial, PadDialAction, TabletConfig};
use smithay::reexports::wayland_protocols::wp::tablet::zv2::server::{
    zwp_tablet_pad_group_v2::{self, ZwpTabletPadGroupV2},
    zwp_tablet_pad_ring_v2::{self, ZwpTabletPadRingV2},
    zwp_tablet_pad_strip_v2::{self, ZwpTabletPadStripV2},
    zwp_tablet_pad_v2::{self, ZwpTabletPadV2},
    zwp_tablet_seat_v2::ZwpTabletSeatV2,
    zwp_tablet_v2::{self, ZwpTabletV2},
};
use smithay::reexports::wayland_server::backend::{ClientId, ObjectId};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, DataInit, Dispatch, DisplayHandle, Resource};

use crate::compositor::backend::{BackendEvent, PadAction, PadDescription, PadEvent, PadGroup};
use crate::compositor::state::State;
use crate::shell::pad_map::{PadControl, PadMap, PadMapAction, PadMapEntry};

/// Steps of a mapped action in one full turn of a ring: one every 15°.
pub const RING_STEPS_PER_TURN: u32 = 24;
/// How far along a strip (of `0.0..=1.0`) a finger slides for one step.
pub const STRIP_STEP: f64 = 0.1;

/// The longest control label an app may give, in characters.
const FEEDBACK_MAX_CHARS: usize = 64;

/// evdev codes a volume or brightness step presses; the shell's media-key
/// handling steps the level and shows it.
const KEY_VOLUMEDOWN: u32 = 114;
const KEY_VOLUMEUP: u32 = 115;
const KEY_BRIGHTNESSDOWN: u32 = 224;
const KEY_BRIGHTNESSUP: u32 = 225;

/// User data of every pad-side protocol object the compositor creates.
#[derive(Debug, Default)]
pub struct PadObject;

/// The pads currently attached, by device identity.
#[derive(Default)]
pub struct TabletPads {
    pads: HashMap<String, Pad>,
}

struct Pad {
    description: PadDescription,
    /// The current mode of each group.
    modes: Vec<u32>,
    /// One announcement per tablet seat that has seen this pad.
    instances: Vec<PadInstance>,
    /// The pad object that holds focus, and the surface it entered.
    focus: Option<(ObjectId, WlSurface)>,
}

struct PadInstance {
    seat: ObjectId,
    tablet: ZwpTabletV2,
    pad: ZwpTabletPadV2,
    groups: Vec<ZwpTabletPadGroupV2>,
    rings: HashMap<u32, ZwpTabletPadRingV2>,
    strips: HashMap<u32, ZwpTabletPadStripV2>,
    /// The serial of the last `enter`, which feedback for a control in no
    /// group names.
    enter_serial: u32,
    /// The serial of each group's last `mode_switch`, which feedback for
    /// its controls must name.
    group_serials: Vec<u32>,
    /// What the app says each control does in its group's current mode.
    feedback: HashMap<PadControl, String>,
}

impl PadInstance {
    /// Forget the app's labels for the controls of `group`, or all of them.
    fn forget_feedback(&mut self, groups: &[PadGroup], group: Option<usize>) {
        match group.and_then(|index| groups.get(index)) {
            Some(group) => self
                .feedback
                .retain(|control, _| !group_has(group, *control)),
            None => self.feedback.clear(),
        }
    }
}

fn group_has(group: &PadGroup, control: PadControl) -> bool {
    match control {
        PadControl::Button(button) => group.buttons.contains(&button),
        PadControl::Ring(ring) => group.rings.contains(&ring),
        PadControl::Strip(strip) => group.strips.contains(&strip),
    }
}

impl TabletPads {
    /// Keep an app's label for the control `find` picks out of one of its
    /// pad objects, if `serial` is that control group's latest.
    fn record_feedback(
        &mut self,
        serial: u32,
        description: &str,
        find: impl Fn(&PadInstance) -> Option<PadControl>,
    ) {
        for pad in self.pads.values_mut() {
            for instance in &mut pad.instances {
                let Some(control) = find(instance) else {
                    continue;
                };
                let expected = pad
                    .description
                    .groups
                    .iter()
                    .position(|group| group_has(group, control))
                    .map_or(Some(instance.enter_serial), |group| {
                        instance.group_serials.get(group).copied()
                    });
                let in_range = match control {
                    PadControl::Button(button) => button < pad.description.buttons,
                    PadControl::Ring(_) | PadControl::Strip(_) => true,
                };
                if expected != Some(serial) || !in_range {
                    return;
                }
                let label: String = description
                    .chars()
                    .filter(|ch| !ch.is_control())
                    .take(FEEDBACK_MAX_CHARS)
                    .collect();
                let label = label.trim();
                if label.is_empty() {
                    instance.feedback.remove(&control);
                } else {
                    instance.feedback.insert(control, label.to_string());
                }
                return;
            }
        }
    }
}

impl State {
    /// Announce a pad that appeared to every client with a tablet seat.
    pub fn add_tablet_pad(&mut self, description: PadDescription) {
        self.remove_tablet_pad(&description.key);
        let modes = vec![0; description.groups.len()];
        self.tablet_clients.pads.pads.insert(
            description.key.clone(),
            Pad {
                description,
                modes,
                instances: Vec::new(),
                focus: None,
            },
        );
        self.announce_tablet_pads();
    }

    /// Tell every client that saw the pad `key` that it went away.
    pub fn remove_tablet_pad(&mut self, key: &str) {
        let Some(pad) = self.tablet_clients.pads.pads.remove(key) else {
            return;
        };
        for instance in pad.instances {
            if instance.pad.is_alive() {
                instance.pad.removed();
            }
            if instance.tablet.is_alive() {
                instance.tablet.removed();
            }
        }
    }

    /// Announce every attached pad to each tablet seat that has not seen it
    /// yet: seats created before the pad appeared, and after. A seat sees a
    /// pad once: a client that destroys it declines the pad, and where that
    /// client has keyboard focus the pad's own mapping applies.
    pub(crate) fn announce_tablet_pads(&mut self) {
        if self.tablet_clients.pads.pads.is_empty() {
            return;
        }
        let dh = self.display_handle.clone();
        let seats = tablet_seats(&dh, self.tablet_clients.client_ids());
        for pad in self.tablet_clients.pads.pads.values_mut() {
            pad.instances
                .retain(|instance| seats.iter().any(|seat| seat.id() == instance.seat));
            for seat in &seats {
                if pad
                    .instances
                    .iter()
                    .any(|instance| instance.seat == seat.id())
                {
                    continue;
                }
                if let Some(instance) = announce(&dh, seat, &pad.description) {
                    pad.instances.push(instance);
                }
            }
        }
    }

    /// Deliver one pad event over `zwp_tablet_pad_v2` to `target`, the
    /// keyboard-focused surface when it takes tablet input, moving the
    /// pad's focus there first. Returns whether a client took the event;
    /// when not, the caller applies [`PadEvent::fallback`].
    pub fn forward_tablet_pad(&mut self, event: &PadEvent, target: Option<u64>) -> bool {
        if let PadAction::ModeSwitch { group, mode, .. } = event.action
            && let Some(pad) = self.tablet_clients.pads.pads.get_mut(&event.pad)
            && let Some(slot) = usize::try_from(group)
                .ok()
                .and_then(|group| pad.modes.get_mut(group))
        {
            *slot = mode;
        }
        let serial = u32::from(self.next_serial());
        let Some((focused, entered)) =
            self.move_tablet_pad_focus(&event.pad, target, event.time_msec, serial)
        else {
            return false;
        };
        let Some(pad) = self.tablet_clients.pads.pads.get_mut(&event.pad) else {
            return false;
        };
        let Some(instance) = focused.and_then(|index| pad.instances.get_mut(index)) else {
            return false;
        };
        send_pad_action(
            instance,
            &pad.description.groups,
            event.action,
            event.time_msec,
            serial,
            entered,
        );
        true
    }

    /// Point pad `key`'s focus at `target` (a surface id), the surface
    /// keyboard focus is on when it takes tablet input, before a map of the
    /// pad is shown: an app learns the pad is there, and can label its
    /// controls, before any of them is touched. `time` is the input
    /// device's clock.
    pub fn focus_tablet_pad(&mut self, key: &str, target: Option<u64>, time: u32) {
        let serial = u32::from(self.next_serial());
        let _ = self.move_tablet_pad_focus(key, target, time, serial);
    }

    /// Point pad `key` at `target` (a surface id), leaving the surface it
    /// was on. Returns the focused instance, if any, and whether it just
    /// entered; `None` when no such pad is attached.
    fn move_tablet_pad_focus(
        &mut self,
        key: &str,
        target: Option<u64>,
        time: u32,
        serial: u32,
    ) -> Option<(Option<usize>, bool)> {
        let surface = target.and_then(|id| self.client_input.surface(id).cloned());
        if surface.is_some() {
            self.announce_tablet_pads();
        }
        let pad = self.tablet_clients.pads.pads.get_mut(key)?;
        let focused = surface.as_ref().and_then(|surface| {
            pad.instances.iter().position(|instance| {
                instance.pad.is_alive() && instance.pad.id().same_client_as(&surface.id())
            })
        });
        let wanted = focused.zip(surface);
        let unchanged = match (&pad.focus, &wanted) {
            (Some((pad_id, old)), Some((index, new))) => pad
                .instances
                .get(*index)
                .is_some_and(|instance| instance.pad.id() == *pad_id && old.id() == new.id()),
            (None, None) => true,
            _ => false,
        };
        let mut entered = false;
        if !unchanged {
            leave_pad(pad, serial);
            if let Some((index, new)) = wanted
                && let Some(instance) = pad.instances.get_mut(index)
            {
                instance.pad.enter(serial, &instance.tablet, &new);
                instance.enter_serial = serial;
                instance.feedback.clear();
                // A client learns each group's mode on entry.
                for ((group, mode), group_serial) in instance
                    .groups
                    .iter()
                    .zip(&pad.modes)
                    .zip(instance.group_serials.iter_mut())
                {
                    group.mode_switch(time, serial, *mode);
                    *group_serial = serial;
                }
                pad.focus = Some((instance.pad.id(), new));
                entered = true;
            }
        }
        Some((focused, entered))
    }

    /// Pads follow the keyboard: when keyboard focus leaves the surface a
    /// pad entered, that app hears `leave` now rather than at the pad's
    /// next touch, so it stops showing pad state it no longer gets.
    pub(crate) fn tablet_pads_follow_keyboard(&mut self, focus: Option<&WlSurface>) {
        let stale = |pad: &Pad| {
            pad.focus
                .as_ref()
                .is_some_and(|(_, entered)| focus.is_none_or(|focus| focus.id() != entered.id()))
        };
        if !self.tablet_clients.pads.pads.values().any(stale) {
            return;
        }
        let serial = u32::from(self.next_serial());
        for pad in self.tablet_clients.pads.pads.values_mut() {
            if stale(pad) {
                leave_pad(pad, serial);
            }
        }
    }

    /// What every control of pad `key` does right now, with `config` its
    /// saved settings: the label the focused tablet-aware app gave it, or
    /// else Tilo's mapping. `None` when no such pad is attached.
    pub fn tablet_pad_map(&self, key: &str, config: &TabletConfig) -> Option<PadMap> {
        let pad = self.tablet_clients.pads.pads.get(key)?;
        let description = &pad.description;
        let feedback = pad.focus.as_ref().and_then(|(pad_id, _)| {
            pad.instances
                .iter()
                .find(|instance| instance.pad.id() == *pad_id)
                .map(|instance| &instance.feedback)
        });
        let app_label = |control| feedback.and_then(|labels| labels.get(&control)).cloned();
        let app_focused = feedback.is_some();
        let mode_of = |control| {
            description
                .groups
                .iter()
                .position(|group| group_has(group, control))
                .and_then(|group| pad.modes.get(group))
                .copied()
                .unwrap_or(0)
        };
        let mut entries = Vec::new();
        for button in 0..description.buttons {
            let control = PadControl::Button(button);
            let mode_group = description
                .groups
                .iter()
                .enumerate()
                .find(|(_, group)| group.mode_buttons.contains(&button));
            // A mapped shortcut and the map button never reach the app, so
            // they come first; then the app's label; then what the button
            // does on its own.
            let action = if config.pad_map_button == Some(button) {
                PadMapAction::ShowsMap
            } else if let Some(shortcut) = config.pad_buttons.get(&button) {
                PadMapAction::Shortcut(*shortcut)
            } else if let Some(label) = app_label(control) {
                PadMapAction::App(label)
            } else if let Some((index, group)) = mode_group {
                PadMapAction::ModeSwitch {
                    mode: pad.modes.get(index).copied().unwrap_or(0),
                    modes: group.modes.max(1),
                }
            } else if app_focused {
                PadMapAction::AppDecides
            } else {
                PadMapAction::Nothing
            };
            entries.push(PadMapEntry { control, action });
        }
        let dials = |pick: fn(&PadGroup) -> &[u32]| {
            let mut all: Vec<u32> = description
                .groups
                .iter()
                .flat_map(|group| pick(group).iter().copied())
                .collect();
            all.sort_unstable();
            all.dedup();
            all
        };
        let rings = dials(|group| &group.rings)
            .into_iter()
            .map(|ring| (PadControl::Ring(ring), PadDial::Ring(ring)));
        let strips = dials(|group| &group.strips)
            .into_iter()
            .map(|strip| (PadControl::Strip(strip), PadDial::Strip(strip)));
        for (control, dial) in rings.chain(strips) {
            let action = if let Some(label) = app_label(control) {
                PadMapAction::App(label)
            } else if app_focused {
                PadMapAction::AppDecides
            } else {
                PadMapAction::Dial(config.dial_action(dial, mode_of(control)))
            };
            entries.push(PadMapEntry { control, action });
        }
        Some(PadMap {
            name: description.device.name.clone(),
            layout: description.layout.clone(),
            entries,
        })
    }
}

/// Send `leave` to the surface pad focus is on, if any, and forget what that
/// app said about the controls.
fn leave_pad(pad: &mut Pad, serial: u32) {
    if let Some((pad_id, old)) = pad.focus.take()
        && let Some(instance) = pad.instances.iter_mut().find(|i| i.pad.id() == pad_id)
    {
        if old.is_alive() && instance.pad.is_alive() {
            instance.pad.leave(serial, &old);
        }
        instance.feedback.clear();
    }
}

/// Send `action` to the pad instance holding focus. `entered` means the pad
/// just entered, which already told the client every group's mode.
fn send_pad_action(
    instance: &mut PadInstance,
    groups: &[PadGroup],
    action: PadAction,
    time: u32,
    serial: u32,
    entered: bool,
) {
    match action {
        PadAction::Button { button, pressed } => instance.pad.button(
            time,
            button,
            if pressed {
                zwp_tablet_pad_v2::ButtonState::Pressed
            } else {
                zwp_tablet_pad_v2::ButtonState::Released
            },
        ),
        PadAction::Ring {
            ring,
            angle,
            finger,
        } => {
            if let Some(object) = instance.rings.get(&ring) {
                if finger {
                    object.source(zwp_tablet_pad_ring_v2::Source::Finger);
                }
                match angle {
                    Some(degrees) => object.angle(degrees),
                    None => object.stop(),
                }
                object.frame(time);
            }
        }
        PadAction::Strip {
            strip,
            position,
            finger,
        } => {
            if let Some(object) = instance.strips.get(&strip) {
                if finger {
                    object.source(zwp_tablet_pad_strip_v2::Source::Finger);
                }
                match position {
                    Some(position) => object.position(strip_units(position)),
                    None => object.stop(),
                }
                object.frame(time);
            }
        }
        PadAction::ModeSwitch { group, mode, .. } => {
            let index = usize::try_from(group).ok();
            if !entered && let Some(object) = index.and_then(|group| instance.groups.get(group)) {
                object.mode_switch(time, serial, mode);
                if let Some(slot) = index.and_then(|group| instance.group_serials.get_mut(group)) {
                    *slot = serial;
                }
                // The app labels the controls afresh for the new mode.
                instance.forget_feedback(groups, index);
            }
        }
    }
}

/// Every live `zwp_tablet_seat_v2` of `clients`.
fn tablet_seats<'a>(
    dh: &DisplayHandle,
    clients: impl Iterator<Item = &'a ClientId>,
) -> Vec<ZwpTabletSeatV2> {
    let handle = dh.backend_handle();
    let mut ids = Vec::new();
    for client in clients {
        let _ = handle.with_all_objects_for(client.clone(), |id| {
            if id.interface().name == ZwpTabletSeatV2::interface().name {
                ids.push(id);
            }
        });
    }
    ids.into_iter()
        .filter_map(|id| ZwpTabletSeatV2::from_id(dh, id).ok())
        .collect()
}

/// Announce `description` on `seat`: its tablet, the pad, and each group
/// with its rings and strips.
fn announce(
    dh: &DisplayHandle,
    seat: &ZwpTabletSeatV2,
    description: &PadDescription,
) -> Option<PadInstance> {
    let client = seat.client()?;
    let version = seat.version();
    let tablet = create::<ZwpTabletV2>(&client, dh, version)?;
    seat.tablet_added(&tablet);
    tablet.name(description.device.name.clone());
    if let Some((vendor, product)) = description.device.usb_id {
        tablet.id(vendor, product);
    }
    let path = description
        .device
        .syspath
        .as_ref()
        .and_then(|path| path.to_str())
        .map(str::to_owned);
    if let Some(path) = &path {
        tablet.path(path.clone());
    }
    tablet.done();

    let pad = create::<ZwpTabletPadV2>(&client, dh, version)?;
    seat.pad_added(&pad);
    if let Some(path) = path {
        pad.path(path);
    }
    pad.buttons(description.buttons);
    let mut groups = Vec::with_capacity(description.groups.len());
    let mut rings = HashMap::new();
    let mut strips = HashMap::new();
    for layout in &description.groups {
        let group = create::<ZwpTabletPadGroupV2>(&client, dh, version)?;
        pad.group(&group);
        group.buttons(
            layout
                .buttons
                .iter()
                .flat_map(|button| button.to_ne_bytes())
                .collect(),
        );
        group.modes(layout.modes.max(1));
        for index in &layout.rings {
            let ring = create::<ZwpTabletPadRingV2>(&client, dh, version)?;
            group.ring(&ring);
            rings.insert(*index, ring);
        }
        for index in &layout.strips {
            let strip = create::<ZwpTabletPadStripV2>(&client, dh, version)?;
            group.strip(&strip);
            strips.insert(*index, strip);
        }
        group.done();
        groups.push(group);
    }
    pad.done();
    let group_serials = vec![0; groups.len()];
    Some(PadInstance {
        seat: seat.id(),
        tablet,
        pad,
        groups,
        rings,
        strips,
        enter_serial: 0,
        group_serials,
        feedback: HashMap::new(),
    })
}

fn create<I>(client: &Client, dh: &DisplayHandle, version: u32) -> Option<I>
where
    I: Resource + 'static,
    State: Dispatch<I, PadObject>,
{
    client
        .create_resource::<I, PadObject, State>(dh, version, PadObject)
        .ok()
}

/// A strip position (`0.0..=1.0`) in the protocol's `0..=65535`.
fn strip_units(position: f64) -> u32 {
    let position = if position.is_finite() {
        position.clamp(0.0, 1.0)
    } else {
        0.0
    };
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "position is clamped to [0, 1], so the product is within 0..=65535"
    )]
    let units = (position * 65535.0).round() as u32;
    units
}

/// The tablet and group objects take no requests the compositor acts on;
/// `destroy` is handled by the protocol layer.
macro_rules! inert_pad_object {
    ($interface:ty, $request:ty) => {
        impl Dispatch<$interface, PadObject> for State {
            fn request(
                _state: &mut Self,
                _client: &Client,
                _resource: &$interface,
                _request: $request,
                _data: &PadObject,
                _dh: &DisplayHandle,
                _data_init: &mut DataInit<'_, Self>,
            ) {
            }
        }
    };
}

inert_pad_object!(ZwpTabletV2, zwp_tablet_v2::Request);
inert_pad_object!(ZwpTabletPadGroupV2, zwp_tablet_pad_group_v2::Request);

/// A button's `set_feedback`: what the app does with it in the current
/// mode, for the pad map.
impl Dispatch<ZwpTabletPadV2, PadObject> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ZwpTabletPadV2,
        request: zwp_tablet_pad_v2::Request,
        _data: &PadObject,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        if let zwp_tablet_pad_v2::Request::SetFeedback {
            button,
            description,
            serial,
        } = request
        {
            let id = resource.id();
            state
                .tablet_clients
                .pads
                .record_feedback(serial, &description, |instance| {
                    (instance.pad.id() == id).then_some(PadControl::Button(button))
                });
        }
    }
}

/// A ring's `set_feedback`, like a button's.
impl Dispatch<ZwpTabletPadRingV2, PadObject> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ZwpTabletPadRingV2,
        request: zwp_tablet_pad_ring_v2::Request,
        _data: &PadObject,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        if let zwp_tablet_pad_ring_v2::Request::SetFeedback {
            description,
            serial,
        } = request
        {
            let id = resource.id();
            state
                .tablet_clients
                .pads
                .record_feedback(serial, &description, |instance| {
                    instance.rings.iter().find_map(|(index, ring)| {
                        (ring.id() == id).then_some(PadControl::Ring(*index))
                    })
                });
        }
    }
}

/// A strip's `set_feedback`, like a button's.
impl Dispatch<ZwpTabletPadStripV2, PadObject> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ZwpTabletPadStripV2,
        request: zwp_tablet_pad_strip_v2::Request,
        _data: &PadObject,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        if let zwp_tablet_pad_strip_v2::Request::SetFeedback {
            description,
            serial,
        } = request
        {
            let id = resource.id();
            state
                .tablet_clients
                .pads
                .record_feedback(serial, &description, |instance| {
                    instance.strips.iter().find_map(|(index, strip)| {
                        (strip.id() == id).then_some(PadControl::Strip(*index))
                    })
                });
        }
    }
}

/// Turns a ring's angle or a strip's position into whole steps, per pad and
/// control, carrying the remainder so slow turns still add up.
#[derive(Debug, Default)]
pub struct DialSteps {
    dials: HashMap<(String, PadDial), (f64, f64)>,
}

impl DialSteps {
    /// Steps `dial` of pad `pad` moved to reach `position` (ring degrees
    /// clockwise from the top, or strip `0.0..=1.0` from the top); positive
    /// means up: turned clockwise or slid upward. `None`, the finger
    /// lifting, starts the next touch afresh.
    pub fn advance(&mut self, pad: &str, dial: PadDial, position: Option<f64>) -> i32 {
        let key = (pad.to_string(), dial);
        let Some(position) = position.filter(|p| p.is_finite()) else {
            self.dials.remove(&key);
            return 0;
        };
        let Some((last, carry)) = self.dials.get(&key).copied() else {
            self.dials.insert(key, (position, 0.0));
            return 0;
        };
        let (moved, step) = match dial {
            // The shorter way round, so crossing the top is a small turn.
            PadDial::Ring(_) => (
                (position - last + 540.0).rem_euclid(360.0) - 180.0,
                360.0 / f64::from(RING_STEPS_PER_TURN),
            ),
            PadDial::Strip(_) => (last - position, STRIP_STEP),
        };
        let total = carry + moved;
        let steps = (total / step).trunc();
        self.dials.insert(key, (position, total - steps * step));
        #[allow(
            clippy::cast_possible_truncation,
            reason = "one event moves a ring at most half a turn or a strip its length: a few steps"
        )]
        let steps = steps as i32;
        steps
    }
}

/// A key a mapped ring or strip presses for one step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DialStep {
    /// A chord on the active layout, as a shortcut would press it.
    Chord(KeyChord),
    /// A machine key by evdev code.
    Key(u32),
}

/// The key one step of `action` presses, up or down. Scrolling and doing
/// nothing press no key.
pub fn dial_step(action: PadDialAction, up: bool) -> Option<DialStep> {
    let chord = |modifiers: &[&str], key: &str| {
        DialStep::Chord(KeyChord {
            modifiers: modifiers.iter().map(|m| (*m).to_string()).collect(),
            key: key.to_string(),
        })
    };
    Some(match (action, up) {
        (PadDialAction::Nothing | PadDialAction::Scroll, _) => return None,
        (PadDialAction::Zoom, true) => chord(&["Ctrl"], "="),
        (PadDialAction::Zoom, false) => chord(&["Ctrl"], "-"),
        (PadDialAction::BrushSize, true) => chord(&[], "]"),
        (PadDialAction::BrushSize, false) => chord(&[], "["),
        (PadDialAction::Volume, true) => DialStep::Key(KEY_VOLUMEUP),
        (PadDialAction::Volume, false) => DialStep::Key(KEY_VOLUMEDOWN),
        (PadDialAction::Brightness, true) => DialStep::Key(KEY_BRIGHTNESSUP),
        (PadDialAction::Brightness, false) => DialStep::Key(KEY_BRIGHTNESSDOWN),
    })
}

/// The input `steps` of a ring or strip doing `action` stand for in an app
/// without tablet support: a wheel scroll under the pointer, or one key
/// press per step, which `keys` turns into the backend's keyboard events.
pub fn dial_events(
    action: PadDialAction,
    steps: i32,
    mut keys: impl FnMut(&DialStep) -> Vec<BackendEvent>,
) -> Vec<BackendEvent> {
    if steps == 0 {
        return Vec::new();
    }
    match action {
        PadDialAction::Nothing => Vec::new(),
        #[allow(
            clippy::cast_precision_loss,
            reason = "a handful of steps per event is exact in f32"
        )]
        PadDialAction::Scroll => vec![BackendEvent::Input(ui::synthetic_input::mouse_wheel(
            0.0,
            steps as f32,
        ))],
        other => (0..steps.unsigned_abs())
            .filter_map(|_| dial_step(other, steps > 0))
            .flat_map(|step| keys(&step))
            .collect(),
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "whole wheel steps are exact values under test"
)]
mod tests {
    use super::*;
    use winit::event::{MouseScrollDelta, WindowEvent};

    #[test]
    fn a_ring_steps_every_fifteen_degrees_across_the_top() {
        let mut steps = DialSteps::default();
        let ring = PadDial::Ring(0);
        assert_eq!(steps.advance("pad", ring, Some(350.0)), 0, "a touch lands");
        assert_eq!(
            steps.advance("pad", ring, Some(5.0)),
            1,
            "clockwise over the top is one step up, not a whole turn back"
        );
        assert_eq!(steps.advance("pad", ring, Some(10.0)), 0);
        assert_eq!(
            steps.advance("pad", ring, Some(20.0)),
            1,
            "the remainder carries between events"
        );
        assert_eq!(steps.advance("pad", ring, Some(345.0)), -2);
        assert_eq!(steps.advance("pad", ring, None), 0, "the finger lifts");
        assert_eq!(
            steps.advance("pad", ring, Some(90.0)),
            0,
            "the next touch starts where it lands"
        );
        assert_eq!(
            steps.advance("other pad", ring, Some(180.0)),
            0,
            "each pad keeps its own rings"
        );
    }

    #[test]
    fn a_strip_steps_up_as_the_finger_slides_up() {
        let mut steps = DialSteps::default();
        let strip = PadDial::Strip(1);
        assert_eq!(steps.advance("pad", strip, Some(0.8)), 0);
        assert_eq!(steps.advance("pad", strip, Some(0.55)), 2);
        assert_eq!(steps.advance("pad", strip, Some(0.95)), -3);
        assert_eq!(
            steps.advance("pad", PadDial::Ring(1), Some(0.2)),
            0,
            "ring 1 and strip 1 are different controls"
        );
    }

    #[test]
    fn each_dial_action_becomes_scrolling_or_keys() {
        let keys = |step: &DialStep| vec![BackendEvent::Input(key_marker(step))];
        let scroll = dial_events(PadDialAction::Scroll, -2, keys);
        assert!(
            matches!(
                scroll.as_slice(),
                [BackendEvent::Input(WindowEvent::MouseWheel {
                    delta: MouseScrollDelta::LineDelta(x, y),
                    ..
                })] if *x == 0.0 && *y == -2.0
            ),
            "{scroll:?}"
        );
        assert!(dial_events(PadDialAction::Nothing, 3, keys).is_empty());
        assert!(dial_events(PadDialAction::Volume, 0, keys).is_empty());
        assert_eq!(dial_events(PadDialAction::Volume, 3, keys).len(), 3);

        assert_eq!(
            dial_step(PadDialAction::Volume, true),
            Some(DialStep::Key(KEY_VOLUMEUP))
        );
        assert_eq!(
            dial_step(PadDialAction::Brightness, false),
            Some(DialStep::Key(KEY_BRIGHTNESSDOWN))
        );
        let Some(DialStep::Chord(zoom_in)) = dial_step(PadDialAction::Zoom, true) else {
            panic!("zoom presses a chord");
        };
        assert_eq!(zoom_in.key, "=");
        assert!(zoom_in.modifiers.contains("Ctrl"));
        let Some(DialStep::Chord(smaller)) = dial_step(PadDialAction::BrushSize, false) else {
            panic!("brush size presses a key");
        };
        assert_eq!(smaller.key, "[");
        assert!(smaller.modifiers.is_empty());
        assert_eq!(dial_step(PadDialAction::Scroll, true), None);
    }

    fn key_marker(_: &DialStep) -> WindowEvent {
        WindowEvent::Focused(true)
    }

    #[test]
    fn strip_positions_span_the_protocol_range() {
        assert_eq!(strip_units(0.0), 0);
        assert_eq!(strip_units(1.0), 65535);
        assert_eq!(strip_units(0.5), 32768);
        assert_eq!(strip_units(f64::NAN), 0);
        assert_eq!(strip_units(3.0), 65535);
    }
}
