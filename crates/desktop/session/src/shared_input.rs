//! One keyboard and pointer for the person's devices side by side: the pointer pushed
//! past the edge of this device's screen toward the screen of another of their devices, as the
//! shared placement places them (Settings → Displays), goes on there, and the keyboard follows
//! it. Pushing back across the edge, or pressing Ctrl+Alt+Escape here, brings both back.
//!
//! Which device drives which is the record of the person's devices ([`InputHand`]): the device
//! whose pointer pushes toward another's screen asks there for it, and the device being driven
//! decides, letting it in for a short while only while the person's Display & input continuity and
//! its automatic grants are on, and ending it when they turn off. Every device of theirs reads who
//! drives whom there. The input itself travels on the live link, from this device's daemon straight
//! to the driven device's ([`crate::live::OwnDevices`]): its relay admits only the person's own
//! devices, as their record keeps them, lets only the driven device read what is sent to it, and
//! what crosses is sealed under the keys of the person's own workspace, which nobody else's device
//! holds. The driven device takes an event only under the hand-over it let in, and hands
//! what it takes to its compositor the way every remote input reaches it — past the firewall that
//! lets the person's own hand there win and that a lock shuts.
//!
//! Crossing has hysteresis: the pointer must push [`PUSH_THROUGH`] logical pixels past an edge
//! before it leaves a device, so a pointer resting against the edge stays where it is.
//!
//! A held button keeps the pointer on its device, unless it carries something: a window lifted by
//! its bar or an item the shell drags, or a drag of words, a picture or a file out of an app. Then
//! the drag crosses with the pointer, ends here dropped nowhere, and goes on there until the button
//! comes up. The shell's drags name a resource of the workspace, which is on both devices already,
//! so its address is what crosses, and what it is let go on there takes it as a drop from here
//! would. An app's drag is content: each format's bytes are kept in the workspace's content,
//! sealed under its keys, and the other device reads them from there by their references. Nothing
//! a path names on this device crosses.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use serde_json::{Value as Json, json};
use settings_sections::desktop::placements_in;
use settings_store::person_displays::{Layout, PersonDisplays, PlacedDisplay, ResolvedDisplay};
use tilo_compositor::clipboard_history::{Pull, Pulled};
use tilo_compositor::{Compositor, InputEvent, ObservedInput};
use tilo_lock::MutexRecover as _;
use tilo_session_core::desktop::state::DesktopHandle;
use tilo_session_core::surface::Scroll;
use tilo_workspace_service::workspaces::person_workspace::devices::{Devices, InputHand};

use crate::followed_resource::SharedFollow;

use crate::platform::fabric::{Continuity, DeviceId, SharedLink, nonzero};

mod wire;

use wire::{
    CarriedDrag, CarriedOffer, Drive, Driven, InputLease, MAX_CARRIED_OFFERS, MAX_INPUT_LEASE_MS,
    RemoteInput, ScrollUnit,
};

/// What carries what this device sends the devices it drives, and what the devices driving it
/// sent: the live link to each of the person's devices ([`crate::live::OwnDevices`]).
pub(crate) trait Carrier: Send + Sync {
    /// Send `body` to `to`, after what was sent to it before: `false` when it cannot go.
    fn send(&self, to: &DeviceId, body: Vec<u8>) -> bool;
    /// What the devices `from` sent this one since the last ask, in order, each with its sender.
    fn heard(&self, from: &BTreeSet<DeviceId>) -> Vec<(DeviceId, Vec<u8>)>;
}

impl Carrier for crate::live::OwnDevices {
    fn send(&self, to: &DeviceId, body: Vec<u8>) -> bool {
        key_of(to).is_some_and(|to| Self::send(self, to, body))
    }

    fn heard(&self, from: &BTreeSet<DeviceId>) -> Vec<(DeviceId, Vec<u8>)> {
        let from: BTreeSet<[u8; 32]> = from.iter().filter_map(key_of).collect();
        Self::heard(self, &from)
            .into_iter()
            .filter_map(|(sender, body)| Some((DeviceId::new(hex::encode(sender)).ok()?, body)))
            .collect()
    }
}

/// How far, in logical pixels, the pointer pushes past an edge before it crosses to the screen
/// beyond: resting against an edge, or brushing it, keeps the pointer where it is.
pub(crate) const PUSH_THROUGH: f64 = 24.0;

/// How long an ask for a lease waits for its answer before the next push asks again.
const ASK_AGAIN: Duration = Duration::from_secs(2);

/// How long a drag that crossed here waits for its content before the input behind it goes on
/// without it.
const CONTENT_WAIT: Duration = Duration::from_secs(10);

/// What a pass hands the keyboard and pointer of the shell: the workspace in front, and the content
/// of a workspace, sealed under its keys, by workspace.
pub(crate) struct Shell<'a> {
    pub(crate) handle: &'a DesktopHandle,
    pub(crate) content: &'a dyn Fn(&str) -> Option<tilo_content::ContentStore>,
}

/// What this desktop's pointer carries in a drag, as the last pass saw it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Carrying {
    /// A resource the shell carries, as its drag names it: a window's record lifted by its bar, an
    /// item of a list.
    Resource {
        kind: String,
        address: String,
        label: String,
    },
    /// A drag of content offering these formats.
    Content(Vec<String>),
}

/// A drag that crossed to the device this one drives, until it is handed over there.
struct Carried {
    device: DeviceId,
    nonce: [u8; 16],
    what: Carrying,
    /// The buttons held as it crossed.
    buttons: BTreeSet<u32>,
    /// Whether the drag here was put back or ended; a release before then waits for it.
    ended_here: bool,
    released_here: Vec<u32>,
    /// The reads of its formats, while they run.
    reads: Vec<Pull>,
    read: Vec<(String, Vec<u8>)>,
    /// Whether the other device was told what it carries; a release before then waits for it.
    told: bool,
    released_there: Vec<u32>,
}

/// What the devices driving this one sent, in the order they sent it.
enum Arrived {
    Input(RemoteInput),
    /// A drag carried here.
    Carried(CarriedDrag),
}

/// What the shell's drag bus names a drag carried here by.
const CARRIED_HERE: &str = "carried-here";

/// What a drag carried here does as the input behind it reaches the shell.
enum Step {
    Input(InputEvent),
    /// The button came up: the resource lands where the pointer is.
    Land(tilo_ui_base::dnd::DragPayload),
}

/// A drag that crossed here, waiting for its content.
struct Landing {
    since: Instant,
}

/// This device's share of the person's one keyboard and pointer: the device it drives now, if
/// any, and the devices driving it.
pub(crate) struct SharedInput {
    fabric: SharedLink,
    /// What carries the input itself, once this session has a live link.
    carrier: Option<std::sync::Arc<dyn Carrier>>,
    /// The record of the person's devices, as the session follows it: it says who drives whom.
    hands: Option<SharedFollow<Devices>>,
    continuity: Continuity,
    placed: PlacementWatch,
    state: std::sync::Mutex<State>,
}

/// The person's placement of their screens, as the session's settings watch last read it, and
/// how many times it changed.
#[derive(Clone, Debug, Default)]
pub(crate) struct PlacementWatch(
    std::sync::Arc<std::sync::Mutex<(u64, tilo_resource::Keyed<PlacedDisplay>)>>,
);

impl PlacementWatch {
    /// The placement the settings say now.
    pub(crate) fn loaded() -> Self {
        Self(std::sync::Arc::new(std::sync::Mutex::new((
            1,
            placements_in(&settings_store::load()),
        ))))
    }

    /// Take the placement from a settings document, as Settings writes it.
    pub(crate) fn read(&self, document: &serde_json::Map<String, Json>) {
        let placement = placements_in(document);
        let mut kept = self.0.lock_recover();
        if kept.1 != placement {
            *kept = (kept.0.wrapping_add(1), placement);
        }
    }

    fn generation(&self) -> u64 {
        self.0.lock_recover().0
    }

    fn placement(&self) -> tilo_resource::Keyed<PlacedDisplay> {
        self.0.lock_recover().1.clone()
    }
}

#[derive(Default)]
struct State {
    local: Option<DeviceId>,
    layout: Layout,
    /// The published generation of the person's displays, and of their placement, the layout
    /// was made from.
    generation: Option<(u64, u64)>,
    locked: bool,
    /// What the devices this one drives granted it, by device.
    leases: BTreeMap<DeviceId, InputLease>,
    asked: BTreeMap<DeviceId, Instant>,
    /// The buttons this machine's pointer holds here: no crossing in the middle of a press.
    buttons: BTreeSet<u32>,
    /// How far the pointer pushed past this device's edge so far.
    push: f64,
    active: Option<Active>,
    /// Keys and buttons let go on the device this one drove when control came back while they
    /// were still held: their releases here are theirs, not this desktop's.
    swallow_keys: BTreeSet<u32>,
    swallow_buttons: BTreeSet<u32>,
    /// Where the pointer comes back to on this device's screen.
    warp: Option<(f64, f64)>,
    /// The leases this device granted, by nonce.
    issued: BTreeMap<[u8; 16], InputLease>,
    /// How far the record of the person's devices moved as this device last read who drives whom.
    hands_read: Option<u64>,
    /// What the devices driving this one sent since the last pass, and when each said it.
    incoming: std::collections::VecDeque<(DeviceId, Arrived, u64)>,
    /// What this desktop's pointer carries in a drag, as the last pass saw it.
    carrying: Option<Carrying>,
    /// A drag that crossed to the device this one drives, until it is handed over.
    carried: Option<Carried>,
    /// A drag that crossed here, waiting for its content.
    landing: Option<Landing>,
    /// A resource carried here, in flight on the shell's drag bus until the button comes up.
    on_bus: Option<tilo_ui_base::dnd::DragPayload>,
    /// The keys and buttons each device driving this one holds down here.
    held: BTreeMap<DeviceId, (BTreeSet<u32>, BTreeSet<u32>)>,
    injected: u64,
    /// The leases under which input reached this device: the first event under a new one is
    /// control arriving here.
    entered: BTreeSet<[u8; 16]>,
    /// The device this one drove on the last pass.
    driving: Option<DeviceId>,
    /// Whether control arrived here since the last pass.
    arrived: bool,
    /// How long each of the latest events took from being sent there to being handed to the
    /// compositor here, in milliseconds.
    latency: std::collections::VecDeque<u64>,
    /// How many drags and windows carried here from the devices driving this one landed.
    landed: u64,
}

impl State {
    /// The lease `device` granted this one, while it holds.
    fn lease(&mut self, device: &DeviceId) -> Option<InputLease> {
        let now = tilo_clock::unix_now_ms();
        let lease = self.leases.get(device)?;
        if lease.holds_at(now) {
            return Some(lease.clone());
        }
        self.leases.remove(device);
        None
    }
}

/// Where the person's keyboard and pointer moved, as the session's pass hands it on.
pub(crate) struct Handover {
    /// Control arrived here since the last pass: from the device it drove, or from a device that
    /// starts driving this one.
    pub(crate) arrived: bool,
    /// The devices this one drives or is driven by under a lease that holds, by id.
    pub(crate) partners: BTreeSet<String>,
}

/// The device this one drives now.
struct Active {
    device: DeviceId,
    output: String,
    nonce: [u8; 16],
    /// The pointer in the shared placement.
    shared: (f64, f64),
    keys: BTreeSet<u32>,
    buttons: BTreeSet<u32>,
    push: f64,
}

/// Make `invoke` on the record of the person's devices at the device's daemon, without waiting.
fn change(record: &SharedFollow<Devices>, invoke: &tilo_resource::Invoke<Devices>) {
    let action = invoke.id;
    record.invoke_unwaited(invoke, move |made| {
        if let Err(error) = made {
            tracing::warn!(%error, action, "who drives whose keyboard and pointer was not kept");
        }
    });
}

impl SharedInput {
    /// The person's keyboard and pointer here: `fabric` says which device this is and which of
    /// theirs are online, `carrier` carries the input, and the record of their devices `hands`
    /// follows says who drives whom. The follow wakes the session's pass each time the record
    /// moves, so an ask of this device is answered and a hand-over it got is taken up though
    /// nothing happens on its own screen.
    pub(crate) fn new(
        fabric: SharedLink,
        carrier: Option<std::sync::Arc<dyn Carrier>>,
        hands: Option<SharedFollow<Devices>>,
        continuity: Continuity,
        placed: PlacementWatch,
    ) -> Self {
        Self {
            fabric,
            carrier,
            hands,
            continuity,
            placed,
            state: std::sync::Mutex::new(State::default()),
        }
    }

    /// See one of this machine's inputs before the desktop does: `true` when it went to the device
    /// this one drives, or ended that, and the desktop must not see it.
    pub(crate) fn observe(&self, input: ObservedInput) -> bool {
        let mut state = self.state.lock_recover();
        let taken = match input {
            ObservedInput::PointerMotion { x, y, dx, dy } => {
                self.motion(&mut state, (x - dx, y - dy), (dx, dy))
            }
            ObservedInput::PointerButton { button, pressed } => {
                self.button(&mut state, button, pressed)
            }
            ObservedInput::Scroll { dx, dy } => {
                let event = RemoteInput::Scroll {
                    horizontal: steps(dx),
                    vertical: steps(dy),
                    unit: ScrollUnit::Pixels,
                };
                state.active.is_some() && self.forward(&mut state, event)
            }
            ObservedInput::Key { keycode, pressed } => self.key(&mut state, keycode, pressed),
            ObservedInput::GiveBack => {
                let active = state.active.take();
                active.is_some_and(|active| {
                    self.end(&mut state, active);
                    true
                })
            }
        };
        drop(state);
        taken
    }

    fn motion(&self, state: &mut State, pointer: (f64, f64), delta: (f64, f64)) -> bool {
        if state.active.is_some() {
            return self.remote_motion(state, delta);
        }
        // A held button keeps the pointer here, unless it carries something across.
        let carrying = if state.buttons.is_empty() {
            None
        } else {
            state.carrying.clone()
        };
        if state.locked || (!state.buttons.is_empty() && carrying.is_none()) {
            state.push = 0.0;
            return false;
        }
        let Some(local) = state.local.clone() else {
            return false;
        };
        let Some(current) = state.layout.locate(local.as_str(), pointer) else {
            return false;
        };
        let Some(here) = state.layout.displays.get(current).cloned() else {
            return false;
        };
        let from = here.to_shared(pointer);
        let Some(step) = state
            .layout
            .step_pointer(current, from, delta, other_device)
        else {
            return false;
        };
        let Some(there) = state.layout.displays.get(step.display).cloned() else {
            return false;
        };
        if step.display == current || there.local {
            state.push = 0.0;
            return false;
        }
        let Ok(device) = DeviceId::new(there.device.clone()) else {
            return false;
        };
        let Some(lease) = state.lease(&device) else {
            self.ask(state, &device);
            return false;
        };
        state.push += outward(&here, from, delta);
        if state.push < PUSH_THROUGH {
            // The desktop keeps the pointer at its edge while the push builds.
            return false;
        }
        state.push = 0.0;
        if !self.send(&device, lease.nonce, point_event(step.device_point)) {
            return false;
        }
        tracing::info!(
            device = device.as_str(),
            "the pointer crossed to another of the person's devices"
        );
        if let Some(what) = carrying {
            self.carry(state, &device, lease.nonce, what);
        }
        state.active = Some(Active {
            device,
            output: there.output,
            nonce: lease.nonce,
            shared: step.shared,
            keys: BTreeSet::new(),
            buttons: BTreeSet::new(),
            push: 0.0,
        });
        self.ask_neighbours(state);
        true
    }

    /// The pointer crossed to `device` carrying `what` : a resource goes on there at once, a drag's
    /// content once the pass read it.
    fn carry(&self, state: &mut State, device: &DeviceId, nonce: [u8; 16], what: Carrying) {
        let told = match &what {
            Carrying::Resource {
                kind,
                address,
                label,
            } => self.drive(
                device,
                nonce,
                Drive::Carried(CarriedDrag::Resource {
                    kind: kind.clone(),
                    address: address.clone(),
                    label: label.clone(),
                }),
            ),
            Carrying::Content(_) => false,
        };
        tracing::info!(
            device = device.as_str(),
            "a drag crossed to another of the person's devices"
        );
        state.carried = Some(Carried {
            device: device.clone(),
            nonce,
            what,
            buttons: state.buttons.clone(),
            ended_here: false,
            released_here: Vec::new(),
            reads: Vec::new(),
            read: Vec::new(),
            told,
            released_there: Vec::new(),
        });
    }

    /// A button held as a drag crossed came up: here it ends the gesture the drag was put back
    /// from, there it drops what the drag carried. `None` for every other button.
    fn carried_release(&self, state: &mut State, button: u32) -> Option<bool> {
        let carried = state.carried.as_mut()?;
        if !carried.buttons.remove(&button) {
            return None;
        }
        state.buttons.remove(&button);
        if carried.told {
            let _ = self.send(&carried.device, carried.nonce, release(button));
        } else {
            carried.released_there.push(button);
        }
        // Until the pass put the drag back here, the release waits for it: it would drop it.
        let taken = !carried.ended_here;
        if taken {
            carried.released_here.push(button);
        }
        Some(taken)
    }

    /// Move the pointer on the device this one drives by `delta`: along its screens, across to
    /// another device that granted a lease, or back to this one once it pushed through.
    fn remote_motion(&self, state: &mut State, delta: (f64, f64)) -> bool {
        let Some(mut active) = state.active.take() else {
            return false;
        };
        let Some(index) = state
            .layout
            .displays
            .iter()
            .position(|display| display.is(active.device.as_str(), &active.output))
        else {
            // The screen it was on left the placement: control comes back.
            self.end(state, active);
            return true;
        };
        let Some(here) = state.layout.displays.get(index).cloned() else {
            self.end(state, active);
            return true;
        };
        let leased: BTreeSet<String> = state
            .leases
            .keys()
            .map(|device| device.as_str().to_owned())
            .collect();
        let reachable = |display: &ResolvedDisplay| {
            display.local
                || (display.online
                    && (display.device == active.device.as_str()
                        || leased.contains(&display.device)))
        };
        let Some(step) = state
            .layout
            .step_pointer(index, active.shared, delta, reachable)
        else {
            self.end(state, active);
            return true;
        };
        let Some(there) = state.layout.displays.get(step.display).cloned() else {
            self.end(state, active);
            return true;
        };
        if there.device == active.device.as_str() {
            active.push = 0.0;
            active.shared = step.shared;
            active.output = there.output;
            let sent = self.send(&active.device, active.nonce, point_event(step.device_point));
            if sent {
                state.active = Some(active);
            } else {
                self.end(state, active);
            }
            return true;
        }
        active.push += outward(&here, active.shared, delta);
        if active.push < PUSH_THROUGH {
            // Held at the edge of the screen it is on while the push builds.
            if let Some(held) = state
                .layout
                .step_pointer(index, active.shared, delta, |_| false)
            {
                active.shared = held.shared;
                let _ = self.send(&active.device, active.nonce, point_event(held.device_point));
            }
            state.active = Some(active);
            return true;
        }
        self.end(state, active);
        if there.local {
            state.warp = Some(step.device_point);
            return true;
        }
        let Ok(device) = DeviceId::new(there.device.clone()) else {
            return true;
        };
        if let Some(lease) = state.lease(&device)
            && self.send(&device, lease.nonce, point_event(step.device_point))
        {
            state.active = Some(Active {
                device,
                output: there.output,
                nonce: lease.nonce,
                shared: step.shared,
                keys: BTreeSet::new(),
                buttons: BTreeSet::new(),
                push: 0.0,
            });
        }
        true
    }

    fn button(&self, state: &mut State, button: u32, pressed: bool) -> bool {
        if !pressed && let Some(taken) = self.carried_release(state, button) {
            return taken;
        }
        if let Some(active) = state.active.as_mut() {
            let ours = if pressed {
                active.buttons.insert(button);
                true
            } else {
                active.buttons.remove(&button)
            };
            if ours {
                return self.forward(state, RemoteInput::PointerButton { button, pressed });
            }
        }
        if pressed {
            state.buttons.insert(button);
            return false;
        }
        state.buttons.remove(&button);
        state.swallow_buttons.remove(&button)
    }

    fn key(&self, state: &mut State, keycode: u32, pressed: bool) -> bool {
        if let Some(active) = state.active.as_mut() {
            let ours = if pressed {
                active.keys.insert(keycode);
                true
            } else {
                active.keys.remove(&keycode)
            };
            if ours {
                return self.forward(
                    state,
                    RemoteInput::Key {
                        keycode,
                        pressed,
                        modifiers: 0,
                    },
                );
            }
            return false;
        }
        !pressed && state.swallow_keys.remove(&keycode)
    }

    /// Send `event` to the device this one drives; control comes back when it cannot go.
    fn forward(&self, state: &mut State, event: RemoteInput) -> bool {
        let Some(active) = state.active.take() else {
            return false;
        };
        if self.send(&active.device, active.nonce, event) {
            state.active = Some(active);
        } else {
            self.end(state, active);
        }
        true
    }

    /// Control comes back here: what the other device still holds is let go there, and the
    /// releases of those keys and buttons here are theirs.
    fn end(&self, state: &mut State, active: Active) {
        for &keycode in &active.keys {
            let _ = self.send(
                &active.device,
                active.nonce,
                RemoteInput::Key {
                    keycode,
                    pressed: false,
                    modifiers: 0,
                },
            );
        }
        for &button in &active.buttons {
            let _ = self.send(
                &active.device,
                active.nonce,
                RemoteInput::PointerButton {
                    button,
                    pressed: false,
                },
            );
        }
        state.swallow_keys.extend(active.keys);
        state.swallow_buttons.extend(active.buttons);
        state.push = 0.0;
        tracing::info!(
            device = active.device.as_str(),
            "the keyboard and pointer came back to this device"
        );
    }

    fn send(&self, device: &DeviceId, nonce: [u8; 16], event: RemoteInput) -> bool {
        self.drive(device, nonce, Drive::Input(event))
    }

    /// Send `what` to `device`, under the hand-over `nonce` it let in.
    fn drive(&self, device: &DeviceId, nonce: [u8; 16], what: Drive) -> bool {
        let driven = Driven {
            lease_nonce: nonce,
            issued_ms: tilo_clock::unix_now_ms(),
            what,
        };
        self.carrier.as_ref().is_some_and(|carrier| {
            driven
                .encode()
                .is_some_and(|body| carrier.send(device, body))
        })
    }

    /// Ask `device` for a lease, unless an ask is on its way.
    fn ask(&self, state: &mut State, device: &DeviceId) {
        let now = Instant::now();
        if state
            .asked
            .get(device)
            .is_some_and(|asked| now.duration_since(*asked) < ASK_AGAIN)
        {
            return;
        }
        state.asked.insert(device.clone(), now);
        let (Some(resources), Some(target), Some(controller)) = (
            &self.hands,
            key_of(device),
            state.local.as_ref().and_then(key_of),
        ) else {
            return;
        };
        let asked_ms = unix_ms(tilo_clock::unix_now_ms());
        change(
            resources,
            &Devices::asking_input(&target, &controller, asked_ms),
        );
    }

    /// Ask every other device whose screens stand in the placement for a lease, so the pointer
    /// can go on from the device it entered to the next one.
    fn ask_neighbours(&self, state: &mut State) {
        let devices: BTreeSet<String> = state
            .layout
            .displays
            .iter()
            .filter(|display| other_device(display))
            .map(|display| display.device.clone())
            .collect();
        for device in devices {
            if let Ok(device) = DeviceId::new(device)
                && state.lease(&device).is_none()
            {
                self.ask(state, &device);
            }
        }
    }

    /// Take what the devices driving this one sent since the last look: input under a hand-over
    /// this device let in, and only while it holds. Only a device this one let in is heard at all.
    pub(crate) fn hear(&self) {
        let Some(carrier) = &self.carrier else {
            return;
        };
        let mut state = self.state.lock_recover();
        self.know_local(&mut state);
        let drivers: BTreeSet<DeviceId> = state
            .issued
            .values()
            .map(|lease| lease.controller.clone())
            .collect();
        for (source, body) in carrier.heard(&drivers) {
            let Some(driven) = Driven::decode(&body) else {
                tracing::debug!(
                    source = source.as_str(),
                    "a device driving this one sent what reads as nothing"
                );
                continue;
            };
            let nonce = driven.lease_nonce;
            match driven.what {
                Drive::Input(event) => {
                    if self.admits(&state, &source, &nonce, |lease| allowed(lease, &event)) {
                        if state.entered.insert(nonce) {
                            state.arrived = true;
                        }
                        state
                            .incoming
                            .push_back((source, Arrived::Input(event), driven.issued_ms));
                    }
                }
                // A drag carried here by the pointer of a device driving this one.
                Drive::Carried(drag) => {
                    if self.admits(&state, &source, &nonce, |lease| lease.allow_pointer) {
                        state.incoming.push_back((
                            source,
                            Arrived::Carried(drag),
                            driven.issued_ms,
                        ));
                    }
                }
            }
        }
    }

    /// Whether `source` drives this device under the lease `nonce` this one issued it, and the
    /// lease lets in what `lets` asks of it.
    fn admits(
        &self,
        state: &State,
        source: &DeviceId,
        nonce: &[u8; 16],
        lets: impl FnOnce(&InputLease) -> bool,
    ) -> bool {
        self.continuity.display_input_permitted()
            && state.issued.get(nonce).is_some_and(|lease| {
                state
                    .local
                    .as_ref()
                    .is_some_and(|local| lease.binds(source, local))
                    && lease.holds_at(tilo_clock::unix_now_ms())
                    && lets(lease)
            })
    }

    /// Learn which device this one is, once its fabric says.
    fn know_local(&self, state: &mut State) {
        if state.local.is_none() {
            state.local = self
                .fabric
                .local_device_fabric_identity()
                .map(|(device, _)| device);
        }
    }

    /// Read who drives whom in the record of the person's devices, when it moved: let in the
    /// device that asked to drive this one, take up the hand-over a device let this one have, and
    /// let go of what was ended.
    fn read_hands(&self, state: &mut State) {
        let Some(resources) = &self.hands else {
            return;
        };
        let moved = resources.revision();
        if state.hands_read == Some(moved) {
            return;
        }
        state.hands_read = Some(moved);
        let Some(local) = state.local.clone() else {
            state.hands_read = None;
            return;
        };
        let devices = resources.get();
        let mut granted = BTreeMap::new();
        let mut asked_here = Vec::new();
        let mut kept_here = BTreeSet::new();
        for (target, hand) in devices.inputs.iter() {
            let Ok(target) = DeviceId::new(target.as_str()) else {
                continue;
            };
            let Ok(controller) = DeviceId::new(hand.driver.as_str()) else {
                continue;
            };
            if target == local {
                match nonce_of(hand) {
                    Some(nonce) => {
                        kept_here.insert(nonce);
                    }
                    None => asked_here.push((controller, hand.asked_ms)),
                }
            } else if controller == local
                && let Some(lease) = lease_of(hand, &local, &target)
            {
                granted.insert(target, lease);
            }
        }
        for (device, lease) in &granted {
            state.asked.remove(device);
            state.leases.insert(device.clone(), lease.clone());
        }
        // A hand-over ended, or one the driven device let in afresh, is no longer this one's.
        let ended: Vec<[u8; 16]> = state
            .leases
            .iter()
            .filter(|(device, lease)| granted.get(*device).is_none_or(|kept| kept != *lease))
            .map(|(_, lease)| lease.nonce)
            .collect();
        state
            .leases
            .retain(|_, lease| !ended.contains(&lease.nonce));
        if let Some(active) = state.active.take_if(|active| ended.contains(&active.nonce)) {
            self.end(state, active);
        }
        // What this device let in is ended once the record no longer holds it; one it let in a
        // moment ago may not have reached the record yet.
        let now = tilo_clock::unix_now_ms();
        let landing = u64::try_from(ASK_AGAIN.as_millis()).unwrap_or(u64::MAX);
        state.issued.retain(|nonce, lease| {
            kept_here.contains(nonce) || now.saturating_sub(lease.issued_unix_ms) < landing
        });
        for (driver, asked_ms) in asked_here {
            self.grant(state, &driver, asked_ms);
        }
    }

    /// Let `controller` drive this device's keyboard and pointer for the ask it made at
    /// `asked_ms`, when the person lets their other devices drive it: the one hand-over it holds
    /// from here.
    fn grant(&self, state: &mut State, controller: &DeviceId, asked_ms: i64) {
        if !self.continuity.display_input_permitted() {
            tracing::debug!(
                controller = controller.as_str(),
                "Display & input is off here: no lease for it"
            );
            return;
        }
        let Some(local) = state.local.clone() else {
            return;
        };
        let now = tilo_clock::unix_now_ms();
        let lease = InputLease {
            controller: controller.clone(),
            target: local,
            issued_unix_ms: now,
            expires_unix_ms: now.saturating_add(MAX_INPUT_LEASE_MS),
            nonce: nonzero::<16>(),
            allow_keyboard: true,
            allow_pointer: true,
        };
        state
            .issued
            .retain(|_, issued| &issued.controller != controller && issued.holds_at(now));
        let (Some(resources), Some(driven)) = (&self.hands, key_of(&lease.target)) else {
            return;
        };
        let hand = InputHand {
            driver: controller.as_str().to_owned(),
            asked_ms,
            nonce: hex::encode(lease.nonce),
            keyboard: lease.allow_keyboard,
            pointer: lease.allow_pointer,
            granted_ms: unix_ms(lease.issued_unix_ms),
            until_ms: unix_ms(lease.expires_unix_ms),
        };
        change(resources, &Devices::granting_input(&driven, &hand));
        state.issued.insert(lease.nonce, lease);
    }

    /// The session's pass: follow the placement and the lock, keep the pointer out of sight while
    /// it drives another device and bring it back where it returned, and hand the compositor what
    /// the devices driving this one sent. Says where the keyboard and pointer moved since the last
    /// pass, for the clipboard to follow them.
    pub(crate) fn follow(&self, compositor: &mut Compositor, shell: &Shell<'_>) -> Handover {
        let mut state = self.state.lock_recover();
        self.know_local(&mut state);
        state.carrying = compositor
            .dragged_formats()
            .map(|formats| carried_formats(&formats))
            .filter(|formats| !formats.is_empty())
            .map(Carrying::Content)
            .or_else(on_the_bus);
        self.read_hands(&mut state);
        let released = self.hand_over(&mut state, compositor, shell);
        let generation = (
            PersonDisplays::published_generation(),
            self.placed.generation(),
        );
        if state.generation != Some(generation) {
            state.generation = Some(generation);
            let placement = self.placed.placement();
            state.layout = PersonDisplays::published()
                .map(|view| view.layout(&placement))
                .unwrap_or_default();
        }
        state.locked = compositor.locked();
        if state.locked
            && let Some(active) = state.active.take()
        {
            self.end(&mut state, active);
        }
        if !self.continuity.display_input_permitted() && !state.issued.is_empty() {
            self.revoke(&mut state);
        }
        let driving = state.active.as_ref().map(|active| active.device.clone());
        // Control coming back from the device this one drove arrives here as it arrives on a
        // device this one starts to drive.
        if state.driving.is_some() && driving.is_none() {
            state.arrived = true;
        }
        state.driving = driving;
        let now = tilo_clock::unix_now_ms();
        state.leases.retain(|_, lease| lease.holds_at(now));
        state.issued.retain(|_, lease| lease.holds_at(now));
        let State {
            issued, entered, ..
        } = &mut *state;
        entered.retain(|nonce| issued.contains_key(nonce));
        let handover = Handover {
            arrived: std::mem::take(&mut state.arrived),
            partners: state
                .leases
                .keys()
                .chain(state.issued.values().map(|lease| &lease.controller))
                .map(|device| device.as_str().to_owned())
                .collect(),
        };
        let parked = state.active.is_some();
        let warp = state.warp.take();
        drop(state);
        compositor.park_pointer(parked);
        if let Some((x, y)) = warp
            && let Err(error) = compositor.input(InputEvent::PointerMotion { x, y })
        {
            tracing::warn!(%error, "the pointer did not come back to this screen");
        }
        // The releases that waited for the drag they ended to be put back.
        for button in released {
            if let Err(error) = compositor.input(InputEvent::PointerButton {
                button,
                pressed: false,
            }) {
                tracing::warn!(%error, "a button that carried a drag away did not come up here");
            }
        }
        self.inject(compositor, Some(shell));
        handover
    }

    /// Hand over the drag that crossed to the device this one drives: put it back here, read
    /// what it carries and keep it in the workspace's content, and tell the other device its
    /// references. The buttons whose release here waited for the drag to be put back.
    fn hand_over(
        &self,
        state: &mut State,
        compositor: &mut Compositor,
        shell: &Shell<'_>,
    ) -> Vec<u32> {
        let Some(carried) = state.carried.as_mut() else {
            return Vec::new();
        };
        let mut released = Vec::new();
        if !carried.ended_here {
            match &carried.what {
                Carrying::Resource { .. } => ui::widget::drag_bus::carry_away(),
                Carrying::Content(formats) => {
                    // Read before the drag ends: a source lets its data go once it is cancelled.
                    for format in formats {
                        match compositor.pull_drag(format) {
                            Ok(pull) => carried.reads.push(pull),
                            Err(error) => {
                                tracing::warn!(%error, format, "a format of the drag is not read");
                            }
                        }
                    }
                    if let Err(error) = compositor.cancel_drag() {
                        tracing::warn!(%error, "the drag that crossed did not end here");
                    }
                }
            }
            carried.ended_here = true;
            released = std::mem::take(&mut carried.released_here);
        }
        if !carried.told {
            carried.reads.retain_mut(|pull| match pull.drain() {
                Pulled::Pending => true,
                Pulled::Ready(bytes) => {
                    carried.read.push((pull.mime().to_owned(), bytes));
                    false
                }
                Pulled::Lost => false,
            });
            if carried.reads.is_empty() {
                carried.told = true;
                let read = std::mem::take(&mut carried.read);
                let told = keep_carried(shell, read).is_some_and(|(workspace, offers)| {
                    self.drive(
                        &carried.device,
                        carried.nonce,
                        Drive::Carried(CarriedDrag::Content { workspace, offers }),
                    )
                });
                // What came up before it was told drops it now; nothing carried, nothing drops.
                for button in std::mem::take(&mut carried.released_there) {
                    if told {
                        let _ = self.send(&carried.device, carried.nonce, release(button));
                    }
                }
            }
        }
        if carried.told && carried.buttons.is_empty() {
            state.carried = None;
        }
        released
    }

    /// Hand the compositor what the devices driving this one sent since the last look, the way
    /// every remote input reaches it: as soon as it is here, not on the next pass. A drag carried
    /// here goes on from where the pointer is with `shell` ; without it, or while its content is
    /// on its way, what was sent after it waits. The drag moves with the compositor's pointer, in
    /// the desktop's pixels, as the drag bus keeps it: each display's tree sees it in its own.
    pub(crate) fn inject(&self, compositor: &mut Compositor, shell: Option<&Shell<'_>>) {
        loop {
            for step in self.take_input() {
                let event = match step {
                    Step::Input(event) => event,
                    Step::Land(payload) => {
                        let (x, y) = compositor.pointer_location();
                        ui::widget::drag_bus::end(Some(ui::widget::Landed {
                            source: CARRIED_HERE.to_owned(),
                            payload,
                            pointer: (x, y).into(),
                        }));
                        continue;
                    }
                };
                if let Err(error) = compositor.input_remote(event) {
                    tracing::warn!(%error, "input from another of the person's devices was not taken");
                }
            }
            // A resource carried here follows the pointer over the shell.
            if ui::widget::drag_bus::current().is_some_and(|flight| flight.source == CARRIED_HERE) {
                let (x, y) = compositor.pointer_location();
                ui::widget::drag_bus::moved(
                    (x, y).into(),
                    (CARRIED_HERE.to_owned(), ui::kurbo::Rect::default()),
                );
            }
            let Some(shell) = shell else {
                return;
            };
            if !self.land(compositor, shell) {
                return;
            }
        }
    }

    /// The input the devices driving this one sent, up to the first drag they carried here, and the
    /// releases of what a device that left held.
    fn take_input(&self) -> Vec<Step> {
        let mut state = self.state.lock_recover();
        if state.incoming.is_empty() && state.held.is_empty() {
            return Vec::new();
        }
        let mut steps: Vec<Step> = self
            .departed(&mut state)
            .into_iter()
            .map(Step::Input)
            .collect();
        let now_ms = tilo_clock::unix_now_ms();
        while let Some((_, Arrived::Input(_), _)) = state.incoming.front() {
            let Some((device, Arrived::Input(event), issued_ms)) = state.incoming.pop_front()
            else {
                break;
            };
            let held = state.held.entry(device).or_default();
            match event {
                RemoteInput::Key {
                    keycode, pressed, ..
                } => {
                    if pressed {
                        held.0.insert(keycode);
                    } else {
                        held.0.remove(&keycode);
                    }
                }
                RemoteInput::PointerButton { button, pressed } => {
                    if pressed {
                        held.1.insert(button);
                    } else {
                        held.1.remove(&button);
                    }
                }
                RemoteInput::PointerMotion { .. } | RemoteInput::Scroll { .. } => {}
            }
            if state.latency.len() == LATENCY_SAMPLES {
                state.latency.pop_front();
            }
            state.latency.push_back(now_ms.saturating_sub(issued_ms));
            state.injected = state.injected.saturating_add(1);
            if let RemoteInput::PointerButton { pressed: false, .. } = event
                && let Some(payload) = state.on_bus.take()
            {
                steps.push(Step::Land(payload));
            }
            steps.push(Step::Input(injected(event)));
        }
        state
            .held
            .retain(|_, (keys, buttons)| !keys.is_empty() || !buttons.is_empty());
        steps
    }

    /// Land the drag carried here that is next in what was sent: a resource goes on over the shell
    /// from where the pointer is, content once it is here. Whether one landed, or was given up on.
    fn land(&self, compositor: &mut Compositor, shell: &Shell<'_>) -> bool {
        let mut state = self.state.lock_recover();
        let content = match state.incoming.front() {
            Some((
                _,
                Arrived::Carried(CarriedDrag::Resource {
                    kind,
                    address,
                    label,
                }),
                _,
            )) => {
                let payload = tilo_ui_base::dnd::DragPayload::Custom {
                    kind: kind.clone(),
                    blob: address.clone().into_bytes(),
                };
                let (x, y) = compositor.pointer_location();
                ui::widget::drag_bus::publish(ui::widget::InFlight {
                    source: CARRIED_HERE.to_owned(),
                    payload: payload.clone(),
                    label: label.clone(),
                    pointer: (x, y).into(),
                    runs: Vec::new(),
                });
                state.incoming.pop_front();
                state.on_bus = Some(payload);
                state.landed = state.landed.saturating_add(1);
                return true;
            }
            Some((_, Arrived::Carried(CarriedDrag::Content { workspace, offers }), _)) => {
                (shell.content)(workspace).and_then(|store| {
                    offers
                        .iter()
                        .map(|offer| Some((offer.mime.clone(), store.read(&offer.reference)?)))
                        .collect::<Option<Vec<_>>>()
                })
            }
            Some((_, Arrived::Input(_), _)) | None => return false,
        };
        let since = state
            .landing
            .get_or_insert_with(|| Landing {
                since: Instant::now(),
            })
            .since;
        if content.is_none() && since.elapsed() < CONTENT_WAIT {
            return false;
        }
        state.incoming.pop_front();
        state.landing = None;
        drop(state);
        let Some(offer) = content else {
            tracing::warn!(
                "the content of a drag carried here did not arrive: it goes on without it"
            );
            return true;
        };
        match compositor.carry_drag_here(offer) {
            Ok(true) => {
                let mut state = self.state.lock_recover();
                state.landed = state.landed.saturating_add(1);
            }
            Ok(false) => tracing::debug!("the drag carried here could not go on"),
            Err(error) => tracing::warn!(%error, "the drag carried here did not go on"),
        }
        true
    }

    /// Let go of what a device that left still holds down here.
    fn departed(&self, state: &mut State) -> Vec<InputEvent> {
        if state.held.is_empty() {
            return Vec::new();
        }
        let online = self.fabric.online_device_ids();
        let mut releases = Vec::new();
        state.held.retain(|device, (keys, buttons)| {
            if online.contains(device) {
                return true;
            }
            releases.extend(keys.iter().map(|&keycode| InputEvent::Key {
                keycode,
                pressed: false,
            }));
            releases.extend(buttons.iter().map(|&button| InputEvent::PointerButton {
                button,
                pressed: false,
            }));
            false
        });
        releases
    }

    /// The person turned Display & input off here: every lease this device granted ends.
    fn revoke(&self, state: &mut State) {
        for (nonce, lease) in std::mem::take(&mut state.issued) {
            if let (Some(resources), Some(target), Some(controller)) = (
                &self.hands,
                key_of(&lease.target),
                key_of(&lease.controller),
            ) {
                let nonce = hex::encode(nonce);
                change(
                    resources,
                    &Devices::ending_input(&target, &controller, &nonce),
                );
            }
        }
    }

    /// What a tool reads of it: where this device's pointer is and whether it shows, the device it
    /// drives, the screens of the placement, and what the devices driving this one sent and how
    /// long it took.
    pub(crate) fn inspection(&self, pointer: (f64, f64), pointer_shown: bool) -> Json {
        let state = self.state.lock_recover();
        let displays: Vec<Json> = state
            .layout
            .displays
            .iter()
            .map(|display| {
                json!({
                    "device": display.device,
                    "output": display.output,
                    "local": display.local,
                    "online": display.online,
                    "rect": [display.rect.x, display.rect.y, display.rect.width, display.rect.height],
                    "local_rect": [
                        display.local_rect.x,
                        display.local_rect.y,
                        display.local_rect.width,
                        display.local_rect.height,
                    ],
                })
            })
            .collect();
        json!({
            "local": state.local.as_ref().map(DeviceId::as_str),
            "pointer": [pointer.0, pointer.1],
            "pointer_shown": pointer_shown,
            "driving": state.active.as_ref().map(|active| active.device.as_str()),
            "displays": displays,
            "leases": state.leases.keys().map(DeviceId::as_str).collect::<Vec<_>>(),
            "let_in": state
                .issued
                .values()
                .map(|lease| lease.controller.as_str())
                .collect::<Vec<_>>(),
            "injected": state.injected,
            "carrying": state.carrying.as_ref().map(|carrying| match carrying {
                Carrying::Resource { address, .. } => json!({"resource": address}),
                Carrying::Content(formats) => json!({"content": formats}),
            }),
            "landed": state.landed,
            "latency_ms": latency(&state.latency),
        })
    }
}

/// How many of the latest events' latencies an inspection reads.
const LATENCY_SAMPLES: usize = 64;

/// The median and the worst of `samples` .
fn latency(samples: &std::collections::VecDeque<u64>) -> Json {
    let mut sorted: Vec<u64> = samples.iter().copied().collect();
    sorted.sort_unstable();
    match (sorted.get(sorted.len() / 2), sorted.last()) {
        (Some(median), Some(max)) => {
            json!({"median": median, "max": max, "samples": sorted.len()})
        }
        _ => Json::Null,
    }
}

/// A screen of another of the person's devices the pointer may go to.
fn other_device(display: &ResolvedDisplay) -> bool {
    !display.local && display.online
}

/// How far `delta` from `from` pushes past the edges of `display` .
fn outward(display: &ResolvedDisplay, from: (f64, f64), delta: (f64, f64)) -> f64 {
    let rect = display.rect;
    let (x, y) = (from.0 + delta.0, from.1 + delta.1);
    let inside = x >= f64::from(rect.x)
        && x < f64::from(rect.right())
        && y >= f64::from(rect.y)
        && y < f64::from(rect.bottom());
    if inside {
        return 0.0;
    }
    delta.0.abs().max(delta.1.abs())
}

/// The resource the shell's drag bus carries now, when the shell itself carries one: a drag carried
/// here from another device crosses on only once it landed.
fn on_the_bus() -> Option<Carrying> {
    let flight = ui::widget::drag_bus::current()?;
    if flight.source == CARRIED_HERE {
        return None;
    }
    let tilo_ui_base::dnd::DragPayload::Custom { kind, blob } = flight.payload else {
        return None;
    };
    // A drag that names nothing (a foreign window's bar) lands nowhere here either.
    let address = match String::from_utf8(blob) {
        Ok(address) if !address.is_empty() => address,
        // A resource is named in text; anything else is no resource of the workspace.
        Ok(_) | Err(_) => return None,
    };
    Some(Carrying::Resource {
        kind,
        address,
        label: flight.label,
    })
}

/// The formats of a drag that cross with it: what names a path on this device stays here.
fn carried_formats(formats: &[String]) -> Vec<String> {
    formats
        .iter()
        .filter(|format| {
            !tilo_ui_base::dnd::is_file_mime(format)
                && format.as_str() != ui::clipboard::SECRET_MIME
        })
        .take(MAX_CARRIED_OFFERS)
        .cloned()
        .collect()
}

/// Keep what a drag carries in the content of the workspace in front, sealed under its keys:
/// the workspace and each format's reference. `None` when nothing could be kept.
fn keep_carried(
    shell: &Shell<'_>,
    read: Vec<(String, Vec<u8>)>,
) -> Option<(String, Vec<CarriedOffer>)> {
    let workspace = shell
        .handle
        .read(|tree| tree.active_workspace().map(|space| space.id().to_string()))?;
    let store = (shell.content)(&workspace)?;
    let offers: Vec<CarriedOffer> = read
        .into_iter()
        .filter_map(|(mime, bytes)| match store.put(&bytes) {
            Ok(reference) => Some(CarriedOffer { mime, reference }),
            Err(error) => {
                tracing::warn!(%error, mime, "a format of the drag could not be kept");
                None
            }
        })
        .collect();
    (!offers.is_empty()).then_some((workspace, offers))
}

/// The key of the device `device` names: its id is the key in hex.
fn key_of(device: &DeviceId) -> Option<[u8; 32]> {
    let mut key = [0; 32];
    hex::decode_to_slice(device.as_str(), &mut key).ok()?;
    Some(key)
}

/// A time in ms since the epoch, as the record keeps it.
fn unix_ms(ms: u64) -> i64 {
    i64::try_from(ms).unwrap_or(i64::MAX)
}

/// The nonce a hand-over was let in under; `None` while it is only asked.
fn nonce_of(hand: &InputHand) -> Option<[u8; 16]> {
    if hand.granted_ms <= 0 {
        return None;
    }
    let mut nonce = [0; 16];
    hex::decode_to_slice(&hand.nonce, &mut nonce).ok()?;
    Some(nonce)
}

/// The lease `hand` gives `local` on `driven`, while it holds.
fn lease_of(hand: &InputHand, local: &DeviceId, driven: &DeviceId) -> Option<InputLease> {
    let lease = InputLease {
        controller: local.clone(),
        target: driven.clone(),
        issued_unix_ms: u64::try_from(hand.granted_ms).ok()?,
        expires_unix_ms: u64::try_from(hand.until_ms).ok()?,
        nonce: nonce_of(hand)?,
        allow_keyboard: hand.keyboard,
        allow_pointer: hand.pointer,
    };
    lease.holds_at(tilo_clock::unix_now_ms()).then_some(lease)
}

/// The release of `button` .
const fn release(button: u32) -> RemoteInput {
    RemoteInput::PointerButton {
        button,
        pressed: false,
    }
}

/// Whether `lease` lets `event` in.
fn allowed(lease: &InputLease, event: &RemoteInput) -> bool {
    match event {
        RemoteInput::Key { .. } => lease.allow_keyboard,
        RemoteInput::PointerMotion { .. }
        | RemoteInput::PointerButton { .. }
        | RemoteInput::Scroll { .. } => lease.allow_pointer,
    }
}

/// The pointer at `point` on the driven device's screens, in its own logical pixels.
fn point_event((x, y): (f64, f64)) -> RemoteInput {
    RemoteInput::PointerMotion {
        x: whole(x),
        y: whole(y),
    }
}

/// `value` in the wire's whole units, inside its bounds.
#[allow(
    clippy::cast_possible_truncation,
    reason = "clamped to the wire's bounds before it is narrowed"
)]
fn whole(value: f64) -> i32 {
    value.clamp(-1_000_000.0, 1_000_000.0).round() as i32
}

/// A scroll of `pixels` in the wire's steps.
fn steps(pixels: f64) -> i32 {
    whole(pixels * ScrollUnit::STEPS)
}

/// What the compositor takes for `event` .
fn injected(event: RemoteInput) -> InputEvent {
    match event {
        RemoteInput::PointerMotion { x, y } => InputEvent::PointerMotion {
            x: f64::from(x),
            y: f64::from(y),
        },
        RemoteInput::PointerButton { button, pressed } => {
            InputEvent::PointerButton { button, pressed }
        }
        RemoteInput::Scroll {
            horizontal,
            vertical,
            unit,
        } => {
            // A wheel line is a notch.
            let x = f64::from(horizontal) / ScrollUnit::STEPS;
            let y = f64::from(vertical) / ScrollUnit::STEPS;
            InputEvent::PointerAxis(match unit {
                ScrollUnit::Pixels => Scroll::Pixels(x, y),
                ScrollUnit::Lines => Scroll::Notches(x, y),
            })
        }
        RemoteInput::Key {
            keycode, pressed, ..
        } => InputEvent::Key { keycode, pressed },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use settings_store::person_displays::Rect;

    fn display(device: &str, local: bool, x: i32) -> ResolvedDisplay {
        let rect = Rect {
            x,
            y: 0,
            width: 1280,
            height: 800,
        };
        ResolvedDisplay {
            device: device.into(),
            output: "out".into(),
            name: "Screen".into(),
            device_label: device.into(),
            builtin: true,
            local,
            online: true,
            order: 0,
            local_rect: Rect { x: 0, ..rect },
            rect,
        }
    }

    /// The person's fabric as the laptop sees it: which device it is.
    struct Recorded {
        local: DeviceId,
    }

    /// What the laptop sent the devices it drives, in order.
    #[derive(Default)]
    struct Sent {
        actions: std::sync::Mutex<Vec<(DeviceId, Drive)>>,
    }

    impl Carrier for Sent {
        fn send(&self, to: &DeviceId, body: Vec<u8>) -> bool {
            let driven = Driven::decode(&body).expect("what the laptop sends reads back");
            self.actions.lock_recover().push((to.clone(), driven.what));
            true
        }

        fn heard(&self, _: &BTreeSet<DeviceId>) -> Vec<(DeviceId, Vec<u8>)> {
            Vec::new()
        }
    }

    impl tilo_speaker_service::devices::Record for Recorded {
        fn record(
            &self,
        ) -> Option<tilo_workspace_service::workspaces::person_workspace::devices::Devices>
        {
            None
        }

        fn apply(
            &self,
            _: tilo_resource::Invoke<
                tilo_workspace_service::workspaces::person_workspace::devices::Devices,
            >,
        ) -> Result<(), String> {
            Err("the laptop has read no record of the person's devices".to_owned())
        }

        fn person(&self) -> [u8; 32] {
            [0; 32]
        }
    }

    impl tilo_speaker_service::devices::Speakers for Recorded {
        fn online_device_ids(&self) -> BTreeSet<DeviceId> {
            BTreeSet::new()
        }

        fn local_device_fabric_identity(&self) -> Option<(DeviceId, String)> {
            Some((self.local.clone(), "Laptop".to_owned()))
        }

        fn try_send_personal_fabric_action(
            &self,
            _: &DeviceId,
            _: crate::platform::fabric::PersonalFabricAction,
        ) -> Result<[u8; 16], crate::platform::fabric::PersonalFabricSendError> {
            Err(crate::platform::fabric::PersonalFabricSendError::Rejected(
                "the laptop's input crosses on the live link".to_owned(),
            ))
        }

        fn require_media(&self) -> Result<(), String> {
            Ok(())
        }

        fn known(&self) -> tilo_speaker_service::devices::Listing {
            tilo_speaker_service::devices::Listing::default()
        }
    }

    /// The laptop's keyboard and pointer with the phone's screen to the right of its own, the
    /// phone having granted it its input, and what the laptop sent the phone.
    fn laptop_beside_phone() -> (SharedInput, std::sync::Arc<Sent>, DeviceId) {
        let laptop = DeviceId::new("laptop").expect("a device id");
        let phone = DeviceId::new("phone").expect("a device id");
        let fabric = std::sync::Arc::new(Recorded {
            local: laptop.clone(),
        });
        let recorded = std::sync::Arc::new(Sent::default());
        let shared = SharedInput::new(
            fabric as SharedLink,
            Some(std::sync::Arc::clone(&recorded) as std::sync::Arc<dyn Carrier>),
            None,
            Continuity::default(),
            PlacementWatch::default(),
        );
        {
            let mut state = shared.state.lock_recover();
            state.local = Some(laptop.clone());
            state.layout = Layout {
                displays: vec![display("laptop", true, 0), display("phone", false, 1280)],
            };
            let now = tilo_clock::unix_now_ms();
            state.leases.insert(
                phone.clone(),
                InputLease {
                    controller: laptop,
                    target: phone.clone(),
                    issued_unix_ms: now,
                    expires_unix_ms: now.saturating_add(MAX_INPUT_LEASE_MS),
                    nonce: [9; 16],
                    allow_keyboard: true,
                    allow_pointer: true,
                },
            );
        }
        (shared, recorded, phone)
    }

    /// Push the laptop's pointer past its right edge, a step at a time: whether it crossed.
    fn push_right(shared: &SharedInput) -> bool {
        (0..6).any(|_| {
            shared.observe(ObservedInput::PointerMotion {
                x: 1285.0,
                y: 400.0,
                dx: 6.0,
                dy: 0.0,
            })
        })
    }

    fn press(shared: &SharedInput, pressed: bool) -> bool {
        shared.observe(ObservedInput::PointerButton {
            button: 0x110,
            pressed,
        })
    }

    /// The releases the laptop sent the phone.
    fn releases(recorded: &Sent) -> usize {
        recorded
            .actions
            .lock_recover()
            .iter()
            .filter(|(_, action)| {
                matches!(
                    action,
                    Drive::Input(RemoteInput::PointerButton { pressed: false, .. })
                )
            })
            .count()
    }

    tilo_test_env::scenario! {
        fn shared_input_crosses_an_edge_only_with_what_may_cross;

        phase a_held_button_keeps_the_pointer_here_unless_it_carries_a_window;
        {
            let (shared, recorded, phone) = laptop_beside_phone();
            assert!(!press(&shared, true), "the press is this desktop's");
            assert!(!push_right(&shared), "a press that carries nothing stays");
            assert!(shared.state.lock_recover().active.is_none());

            // The shell lifts a window by its bar: its record crosses with the pointer.
            shared.state.lock_recover().carrying = Some(Carrying::Resource {
                kind: "resource".to_owned(),
                address: "notes.page:one".to_owned(),
                label: "Notes".to_owned(),
            });
            assert!(push_right(&shared), "the window carried the pointer across");
            let sent = recorded.actions.lock_recover().clone();
            let told = sent
                .iter()
                .position(|(to, action)| {
                    to == &phone
                        && matches!(
                            action,
                            Drive::Carried(CarriedDrag::Resource { address, .. })
                                if address == "notes.page:one"
                        )
                })
                .expect("the phone is told what the drag carries");
            assert!(
                sent[..told].iter().any(|(_, action)| matches!(
                    action,
                    Drive::Input(RemoteInput::PointerMotion { .. })
                )),
                "the pointer arrives there before the window: {sent:?}"
            );

            // Let go before the shell here let go of it: the release waits here, and drops it there.
            assert!(
                press(&shared, false),
                "the release here waits for the drag to end here"
            );
            assert_eq!(
                shared
                    .state
                    .lock_recover()
                    .carried
                    .as_ref()
                    .map(|carried| carried.released_here.clone()),
                Some(vec![0x110])
            );
            assert_eq!(releases(&recorded), 1, "the release drops it on the phone");
        }

        phase a_drag_of_content_drops_there_only_once_the_phone_was_told_what_it_carries;
        {
            let (shared, recorded, _) = laptop_beside_phone();
            assert!(!press(&shared, true));
            shared.state.lock_recover().carrying =
                Some(Carrying::Content(vec!["text/plain".to_owned()]));
            assert!(push_right(&shared));
            // The pass has not read the drag yet: its release waits there as here.
            assert!(press(&shared, false));
            assert_eq!(releases(&recorded), 0);
            let (released_there, told) = shared
                .state
                .lock_recover()
                .carried
                .as_ref()
                .map(|carried| (carried.released_there.clone(), carried.told))
                .expect("the drag is being handed over");
            assert_eq!(released_there, vec![0x110]);
            assert!(!told);
        }

        phase what_names_a_path_here_stays_here;
        {
            let formats = [
                "text/uri-list",
                "x-special/gnome-copied-files",
                "text/plain;charset=utf-8",
                "image/png",
            ]
            .map(str::to_owned);
            assert_eq!(
                carried_formats(&formats),
                vec![
                    "text/plain;charset=utf-8".to_owned(),
                    "image/png".to_owned()
                ]
            );
        }

        phase only_a_push_past_an_edge_counts_toward_crossing;
        {
            let here = display("laptop", true, 0);
            assert!(outward(&here, (1279.0, 400.0), (6.0, 1.0)) > 0.0);
            assert!(outward(&here, (-0.0, 400.0), (-3.0, 0.0)) > 0.0);
            assert!(
                outward(&here, (600.0, 400.0), (6.0, 1.0)) <= 0.0,
                "motion inside the screen pushes nowhere"
            );
        }

        phase every_event_reaches_the_compositor_as_the_same_input;
        {
            assert_eq!(
                injected(point_event((12.4, 799.6))),
                InputEvent::PointerMotion { x: 12.0, y: 800.0 }
            );
            assert_eq!(
                injected(RemoteInput::Scroll {
                    horizontal: steps(0.0),
                    vertical: steps(-3.5),
                    unit: ScrollUnit::Pixels,
                }),
                InputEvent::PointerAxis(Scroll::Pixels(0.0, -3.5))
            );
            assert_eq!(
                injected(RemoteInput::Key {
                    keycode: 125,
                    pressed: true,
                    modifiers: 0,
                }),
                InputEvent::Key {
                    keycode: 125,
                    pressed: true,
                }
            );
        }
    }
}
