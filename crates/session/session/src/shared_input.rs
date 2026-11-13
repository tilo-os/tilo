//! One keyboard and pointer for the person's devices standing side by side: the pointer pushed
//! past the edge of this device's screen toward the screen of another of their devices, as the
//! shared arrangement places them (Settings → Displays), goes on there, and the keyboard follows
//! it. Pushing back across the edge, or pressing Ctrl+Alt+Escape here, brings both back.
//!
//! The input travels as actions on the person's device fabric: the direct, authenticated session
//! between two devices certified by the same person, which nobody else's device can join. The
//! device being driven decides: it grants a short-lived lease only while the person's Display &
//! input continuity and its automatic grants are on, takes an event only under the lease it
//! issued, and hands what it takes to its compositor the way every remote input reaches it — past
//! the firewall that lets the person's own hand there win and that a lock shuts.
//!
//! Crossing has hysteresis: the pointer must push [`PUSH_THROUGH`] logical pixels past an edge
//! before it leaves a device, so a pointer resting against the edge stays where it is.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use serde_json::{Value as Json, json};
use settings_store::person_displays::{Arrangement, Layout, PersonDisplays, ResolvedDisplay};
use tilo_compositor::{Compositor, InputEvent, ObservedInput};
use tilo_lock::MutexRecover as _;

use crate::platform::fabric::{
    Continuity, DeviceId, FabricInputEvent, InputLease, MAX_INPUT_LEASE_MS, PersonalFabricAction,
    PersonalFabricActionEnvelope, ScrollUnit, SharedLink, nonzero,
};

/// How far, in logical pixels, the pointer pushes past an edge before it crosses to the screen
/// beyond: resting against an edge, or brushing it, keeps the pointer where it is.
pub(crate) const PUSH_THROUGH: f64 = 24.0;

/// How long an ask for a lease waits for its answer before the next push asks again.
const ASK_AGAIN: Duration = Duration::from_secs(2);

/// This device's share of the person's one keyboard and pointer: the device it drives now, if
/// any, and the devices driving it.
pub(crate) struct SharedInput {
    fabric: SharedLink,
    continuity: Continuity,
    arranged: Arranged,
    state: std::sync::Mutex<State>,
}

/// The person's arrangement of their screens, as the session's settings watch last read it, and
/// how many times it changed.
#[derive(Clone, Debug, Default)]
pub(crate) struct Arranged(std::sync::Arc<std::sync::Mutex<(u64, Arrangement)>>);

impl Arranged {
    /// The arrangement the settings say now.
    pub(crate) fn loaded() -> Self {
        Self(std::sync::Arc::new(std::sync::Mutex::new((
            1,
            Arrangement::load(),
        ))))
    }

    /// Take the arrangement from a settings document, as Settings writes it.
    pub(crate) fn read(&self, document: &serde_json::Map<String, Json>) {
        let arrangement = Arrangement::from_document(document);
        let mut kept = self.0.lock_recover();
        if kept.1 != arrangement {
            *kept = (kept.0.wrapping_add(1), arrangement);
        }
    }

    fn generation(&self) -> u64 {
        self.0.lock_recover().0
    }

    fn arrangement(&self) -> Arrangement {
        self.0.lock_recover().1.clone()
    }
}

#[derive(Default)]
struct State {
    local: Option<DeviceId>,
    layout: Layout,
    /// The published generation of the person's displays, and of their arrangement, the layout
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
    /// What the devices driving this one sent since the last pass, and when each said it.
    incoming: Vec<(DeviceId, FabricInputEvent, u64)>,
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
}

impl State {
    /// The lease `device` granted this one, while it holds.
    fn lease(&mut self, device: &DeviceId) -> Option<InputLease> {
        let now = tilo_clock::unix_now_ms();
        let lease = self.leases.get(device)?;
        if lease.validate_at(now).is_ok() {
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
    /// The pointer in the shared arrangement.
    shared: (f64, f64),
    keys: BTreeSet<u32>,
    buttons: BTreeSet<u32>,
    push: f64,
}

impl SharedInput {
    pub(crate) fn new(fabric: SharedLink, continuity: Continuity, arranged: Arranged) -> Self {
        Self {
            fabric,
            continuity,
            arranged,
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
                let event = FabricInputEvent::Scroll {
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
        if state.locked || !state.buttons.is_empty() {
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
            // The screen it was on left the arrangement: control comes back.
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
            display.enabled
                && (display.local
                    || (display.online
                        && (display.device == active.device.as_str()
                            || leased.contains(&display.device))))
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
        if let Some(active) = state.active.as_mut() {
            let ours = if pressed {
                active.buttons.insert(button);
                true
            } else {
                active.buttons.remove(&button)
            };
            if ours {
                return self.forward(state, FabricInputEvent::PointerButton { button, pressed });
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
                    FabricInputEvent::Key {
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
    fn forward(&self, state: &mut State, event: FabricInputEvent) -> bool {
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
                FabricInputEvent::Key {
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
                FabricInputEvent::PointerButton {
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

    fn send(&self, device: &DeviceId, nonce: [u8; 16], event: FabricInputEvent) -> bool {
        self.fabric
            .try_send_personal_fabric_action(
                device,
                PersonalFabricAction::Input {
                    lease_nonce: nonce,
                    event,
                },
            )
            .is_ok()
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
        let _ = self
            .fabric
            .try_send_personal_fabric_action(device, PersonalFabricAction::InputLeaseAsk);
    }

    /// Ask every other device whose screens stand in the arrangement for a lease, so the pointer
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

    /// Take an action of the person's other devices that is this one's: a lease asked of it or
    /// granted to it, or input under a lease it granted. `false` for every other action.
    pub(crate) fn route(&self, envelope: &PersonalFabricActionEnvelope) -> bool {
        let source = &envelope.source;
        let mut state = self.state.lock_recover();
        self.know_local(&mut state);
        match &envelope.action {
            PersonalFabricAction::InputLeaseAsk => {
                self.grant(&mut state, source);
            }
            PersonalFabricAction::InputLeaseGranted { lease } => {
                if state
                    .local
                    .as_ref()
                    .is_some_and(|local| lease.binds(local, source))
                    && lease.validate_at(tilo_clock::unix_now_ms()).is_ok()
                {
                    state.asked.remove(source);
                    state.leases.insert(source.clone(), lease.clone());
                }
            }
            PersonalFabricAction::InputLeaseRevoked { nonce } => {
                state
                    .leases
                    .retain(|device, lease| !(device == source && &lease.nonce == nonce));
                if let Some(active) = state.active.take_if(|active| &active.nonce == nonce) {
                    self.end(&mut state, active);
                }
            }
            PersonalFabricAction::Input { lease_nonce, event } => {
                let admitted = self.continuity.display_input_permitted()
                    && state.issued.get(lease_nonce).is_some_and(|lease| {
                        state
                            .local
                            .as_ref()
                            .is_some_and(|local| lease.binds(source, local))
                            && lease.validate_at(tilo_clock::unix_now_ms()).is_ok()
                            && allowed(lease, event)
                    });
                if admitted {
                    if state.entered.insert(*lease_nonce) {
                        state.arrived = true;
                    }
                    state
                        .incoming
                        .push((source.clone(), *event, envelope.issued_unix_ms));
                }
            }
            _ => return false,
        }
        true
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

    /// Grant `controller` a lease on this device's keyboard and pointer, when the person lets
    /// their other devices drive it: the one lease it holds from here.
    fn grant(&self, state: &mut State, controller: &DeviceId) {
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
        state.issued.retain(|_, issued| {
            &issued.controller != controller && issued.validate_at(now).is_ok()
        });
        let sent = self.fabric.try_send_personal_fabric_action(
            controller,
            PersonalFabricAction::InputLeaseGranted {
                lease: lease.clone(),
            },
        );
        if sent.is_ok() {
            state.issued.insert(lease.nonce, lease);
        }
    }

    /// The session's pass: follow the arrangement and the lock, keep the pointer out of sight while
    /// it drives another device and bring it back where it returned, and hand the compositor what
    /// the devices driving this one sent. Says where the keyboard and pointer moved since the last
    /// pass, for the clipboard to follow them.
    pub(crate) fn follow(&self, compositor: &mut Compositor) -> Handover {
        let mut state = self.state.lock_recover();
        self.know_local(&mut state);
        let generation = (
            PersonDisplays::published_generation(),
            self.arranged.generation(),
        );
        if state.generation != Some(generation) {
            state.generation = Some(generation);
            let arrangement = self.arranged.arrangement();
            state.layout = PersonDisplays::published()
                .map(|view| view.layout(&arrangement))
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
        state
            .leases
            .retain(|_, lease| lease.validate_at(now).is_ok());
        state
            .issued
            .retain(|_, lease| lease.validate_at(now).is_ok());
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
        self.inject(compositor);
        handover
    }

    /// Hand the compositor what the devices driving this one sent since the last look, the way
    /// every remote input reaches it: as soon as it is here, not on the next pass.
    pub(crate) fn inject(&self, compositor: &mut Compositor) {
        let mut state = self.state.lock_recover();
        if state.incoming.is_empty() && state.held.is_empty() {
            return;
        }
        let incoming = std::mem::take(&mut state.incoming);
        let gone = self.departed(&mut state);
        let now_ms = tilo_clock::unix_now_ms();
        let mut events = Vec::with_capacity(incoming.len());
        for (device, event, issued_ms) in incoming {
            let held = state.held.entry(device).or_default();
            match event {
                FabricInputEvent::Key {
                    keycode, pressed, ..
                } => {
                    if pressed {
                        held.0.insert(keycode);
                    } else {
                        held.0.remove(&keycode);
                    }
                }
                FabricInputEvent::PointerButton { button, pressed } => {
                    if pressed {
                        held.1.insert(button);
                    } else {
                        held.1.remove(&button);
                    }
                }
                FabricInputEvent::PointerMotion { .. } | FabricInputEvent::Scroll { .. } => {}
            }
            if state.latency.len() == LATENCY_SAMPLES {
                state.latency.pop_front();
            }
            state.latency.push_back(now_ms.saturating_sub(issued_ms));
            state.injected = state.injected.saturating_add(1);
            events.push(injected(event));
        }
        state
            .held
            .retain(|_, (keys, buttons)| !keys.is_empty() || !buttons.is_empty());
        drop(state);
        for event in gone.into_iter().chain(events) {
            if let Err(error) = compositor.input_remote(event) {
                tracing::warn!(%error, "input from another of the person's devices was not taken");
            }
        }
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
            let _ = self.fabric.try_send_personal_fabric_action(
                &lease.controller,
                PersonalFabricAction::InputLeaseRevoked { nonce },
            );
        }
    }

    /// What a tool reads of it: where this device's pointer is and whether it shows, the device it
    /// drives, the screens of the arrangement, and what the devices driving this one sent and how
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
            "injected": state.injected,
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
    !display.local && display.online && display.enabled
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

/// Whether `lease` lets `event` in.
fn allowed(lease: &InputLease, event: &FabricInputEvent) -> bool {
    match event {
        FabricInputEvent::Key { .. } => lease.allow_keyboard,
        FabricInputEvent::PointerMotion { .. }
        | FabricInputEvent::PointerButton { .. }
        | FabricInputEvent::Scroll { .. } => lease.allow_pointer,
    }
}

/// The pointer at `point` on the driven device's screens, in its own logical pixels.
fn point_event((x, y): (f64, f64)) -> FabricInputEvent {
    FabricInputEvent::PointerMotion {
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
fn injected(event: FabricInputEvent) -> InputEvent {
    match event {
        FabricInputEvent::PointerMotion { x, y } => InputEvent::PointerMotion {
            x: f64::from(x),
            y: f64::from(y),
        },
        FabricInputEvent::PointerButton { button, pressed } => {
            InputEvent::PointerButton { button, pressed }
        }
        FabricInputEvent::Scroll {
            horizontal,
            vertical,
            unit,
        } => {
            // A wheel line is a notch, which the desktop scrolls as far as a portal's notch.
            let per = match unit {
                ScrollUnit::Pixels => 1.0,
                ScrollUnit::Lines => crate::portal_link::NOTCH,
            };
            InputEvent::PointerAxis {
                horizontal: f64::from(horizontal) / ScrollUnit::STEPS * per,
                vertical: f64::from(vertical) / ScrollUnit::STEPS * per,
            }
        }
        FabricInputEvent::Key {
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
            enabled: true,
            order: 0,
            local_rect: Rect { x: 0, ..rect },
            rect,
        }
    }

    #[test]
    fn only_a_push_past_an_edge_counts_toward_crossing() {
        let here = display("laptop", true, 0);
        assert!(outward(&here, (1279.0, 400.0), (6.0, 1.0)) > 0.0);
        assert!(outward(&here, (-0.0, 400.0), (-3.0, 0.0)) > 0.0);
        assert!(
            outward(&here, (600.0, 400.0), (6.0, 1.0)) <= 0.0,
            "motion inside the screen pushes nowhere"
        );
    }

    #[test]
    fn every_event_reaches_the_compositor_as_the_same_input() {
        assert_eq!(
            injected(point_event((12.4, 799.6))),
            InputEvent::PointerMotion { x: 12.0, y: 800.0 }
        );
        assert_eq!(
            injected(FabricInputEvent::Scroll {
                horizontal: steps(0.0),
                vertical: steps(-3.5),
                unit: ScrollUnit::Pixels,
            }),
            InputEvent::PointerAxis {
                horizontal: 0.0,
                vertical: -3.5,
            }
        );
        assert_eq!(
            injected(FabricInputEvent::Key {
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
