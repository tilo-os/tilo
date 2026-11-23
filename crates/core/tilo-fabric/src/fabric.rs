//! The person's devices as this one takes part among them, published as the provider
//! [`PROVIDER`]: who is here and on this network, the lock that follows the person, waking a
//! sleeping device, and unlocking another device's disk at boot.
//!
//! Every exchange with another of the person's devices is an action call on the one device link
//! (`devices/<id>/…`, through the [`Invoker`] the host hands in), and every answer comes back as
//! that call's output. What the desktop must do (follow a lock elsewhere) is a call to the session
//! provider on this device ([`SeatCalls`]), which a device without a session refuses. What lasts
//! is a fact in the person's store ([`Facts`]).
//!
//! The host tells the fabric which devices its link reaches ([`Fabric::arrived`],
//! [`Fabric::left`]) and which it heard on the local network ([`Fabric::heard_nearby`]), and calls
//! [`Fabric::tick`] about once a second, which re-reads this machine's interfaces and moves the
//! wakes it watches on.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tilo_actions::remote::{ActionCallError, ActionProvider, Pending};
use tilo_lock::MutexRecover as _;
use tilo_unlock_protocol::{
    EnrolReply, EnrolRequest, RefusalReason, SiblingFrame, UnlockHello, UnlockRequest,
};
use tilo_user_identity::DeviceCertification;

use crate::actions::{Asker, Provider, Who, asking_device};
use crate::calls::{
    DeviceCall, Facts, Invoker, SeatCalls, keep_fact, pack, read_facts, spend, unpack,
};
use crate::interfaces::WakeInterfaces;
use crate::owner::{Follow, SessionFollow, latest_use_nearby, shares_network};
use crate::unlock::{
    DecisionOutcome, MergedUnlock, UnlockDecision, UnlockFabricState, UnlockSibling, decision_for,
    merged_unlock_state,
};
use crate::unlock_keys::{
    Ownership, PersonKeys, boot_cert_digest, enrol, enrol_reply, slot_name, view_of, with_decision,
    with_enrolment,
};
use crate::wake::{
    MagicPackets, ROUTE_SCAN_INTERVAL, WakeEvent, WakeOutcome, Waker, WatchStep, magic_packets_for,
    plan_wake, remember_subnets, resolve_device,
};
use crate::{Device, DeviceAbout, DeviceDisplay, DeviceId, DevicePresence, Devices, OwnerSession};

/// The provider the fabric's resources and actions are published under.
pub const PROVIDER: &str = "so.tilo.fabric";
/// The scope a booting device with no certificate yet calls unlock and enrolment under
/// (`ActionService::admit_uncertified`).
pub const UNLOCK_SCOPE: &str = "so.tilo.unlock";
/// The fact a device keeps of what it is ([`DeviceAbout`]).
pub const ABOUT: &str = "about";
/// The fact a device keeps of its disk unlock ([`UnlockFabricState`]).
pub const UNLOCK: &str = "unlock";
/// Wake outcomes kept for the person to read.
const MAX_WAKE_EVENTS: usize = 16;

/// What a fabric is made of, as the host that mounts it hands it in.
pub struct Parts {
    /// This device's key: what the person's other devices know it by.
    pub key: [u8; 32],
    /// The person's root key, whose certificates name their devices.
    pub person: [u8; 32],
    /// What this device is.
    pub about: DeviceAbout,
    pub invoker: Arc<dyn Invoker>,
    pub seat: Arc<dyn SeatCalls>,
    pub facts: Arc<dyn Facts>,
    pub keys: Arc<dyn PersonKeys>,
    pub interfaces: Arc<dyn WakeInterfaces>,
    pub packets: Arc<dyn MagicPackets>,
}

/// The person's devices, as this one takes part among them. Cloning shares it.
#[derive(Clone)]
pub struct Fabric(Arc<Inner>);

type Arrival = Arc<dyn Fn(&DeviceId) + Send + Sync>;

struct Inner {
    local: DeviceId,
    key: [u8; 32],
    person: [u8; 32],
    about: DeviceAbout,
    invoker: Arc<dyn Invoker>,
    seat: Arc<dyn SeatCalls>,
    facts: Arc<dyn Facts>,
    keys: Arc<dyn PersonKeys>,
    interfaces: Arc<dyn WakeInterfaces>,
    packets: Arc<dyn MagicPackets>,
    state: Mutex<State>,
    /// Serializes what this device keeps of its disk unlock: two answers at once must not
    /// overwrite each other's.
    unlock_kept: Mutex<()>,
    arrivals: Mutex<Vec<Arrival>>,
}

/// Another of the person's devices, as its link to this one says it.
#[derive(Clone, Debug, Default)]
struct Live {
    /// What its certificate calls it, until it writes what it is.
    label: String,
    /// Whether the link reaches it now.
    reachable: bool,
    /// What it last said of itself.
    presence: Option<DevicePresence>,
    last_seen_unix_ms: u64,
}

/// What the session here last said of the person's session on this device.
#[derive(Clone, Copy, Debug)]
struct Said {
    locked: bool,
    input_unix_ms: u64,
    in_use: bool,
}

/// A boot waiting for the person's answer to its unlock request.
#[derive(Clone, Debug)]
struct Boot {
    code: String,
    device: [u8; 32],
    slot: String,
    /// The answer, once the person gave it, until the boot reads it.
    decided: Option<SiblingFrame>,
}

/// Another device's enrolment waiting for the person's answer.
#[derive(Clone, Debug)]
struct Enrol {
    request: EnrolRequest,
    answer: Option<EnrolReply>,
}

#[derive(Default)]
struct State {
    live: BTreeMap<DeviceId, Live>,
    nearby: BTreeSet<DeviceId>,
    own: DevicePresence,
    follow: SessionFollow,
    said: Option<Said>,
    waker: Waker,
    routes_read: Option<Instant>,
    wakes: VecDeque<WakeEvent>,
    unlock: Option<UnlockSibling>,
    boots: BTreeMap<[u8; 32], Boot>,
    enrols: BTreeMap<[u8; 32], Enrol>,
}

impl State {
    fn sibling(&mut self, key: [u8; 32]) -> &mut UnlockSibling {
        self.unlock.get_or_insert_with(|| UnlockSibling::new(key))
    }
}

impl Fabric {
    /// The fabric of the device `parts` describes: it keeps what this device is among the person's
    /// facts at once.
    #[must_use]
    pub fn new(parts: Parts) -> Self {
        let local = DeviceId::of_key(&parts.key);
        if let Err(error) = keep_fact(parts.facts.as_ref(), ABOUT, &parts.about) {
            tracing::warn!(%error, "what this device is was not kept for the person's devices");
        }
        Self(Arc::new(Inner {
            local,
            key: parts.key,
            person: parts.person,
            about: parts.about,
            invoker: parts.invoker,
            seat: parts.seat,
            facts: parts.facts,
            keys: parts.keys,
            interfaces: parts.interfaces,
            packets: parts.packets,
            state: Mutex::default(),
            unlock_kept: Mutex::new(()),
            arrivals: Mutex::default(),
        }))
    }

    /// This device.
    #[must_use]
    pub fn local(&self) -> &DeviceId {
        &self.0.local
    }

    /// The person whose devices these are.
    #[must_use]
    pub fn person(&self) -> &[u8; 32] {
        &self.0.person
    }

    /// The session on this device, as the services reach it.
    #[must_use]
    pub fn seat(&self) -> &Arc<dyn SeatCalls> {
        &self.0.seat
    }

    /// Call `provider`'s `resource#action` on `device` with `input`.
    pub fn call(
        &self,
        device: &DeviceId,
        provider: &str,
        resource: &str,
        action: &str,
        input: impl Serialize,
    ) -> Pending<Result<Value, ActionCallError>> {
        self.0
            .invoker
            .call(DeviceCall::new(device, provider, resource, action, input))
    }

    /// Run `then` each time a device of the person's arrives on the link, after it was told what
    /// this device says of itself: what a service that offers each device its own state does.
    pub fn on_arrival(&self, then: impl Fn(&DeviceId) + Send + Sync + 'static) {
        self.0.arrivals.lock_recover().push(Arc::new(then));
    }

    /// The link reaches `device`, which its certificate calls `label`: it is told at once what
    /// this device says of itself, as each side does on every link, so neither has to ask.
    pub fn arrived(&self, device: &DeviceId, label: &str) {
        if device == self.local() {
            return;
        }
        {
            let mut state = self.0.state.lock_recover();
            let live = state.live.entry(device.clone()).or_default();
            live.reachable = true;
            if live.label.is_empty() {
                label.clone_into(&mut live.label);
            }
            drop(state);
        }
        self.tell_presence(Some(device));
        let arrivals = self.0.arrivals.lock_recover().clone();
        for arrival in arrivals {
            arrival(device);
        }
    }

    /// The link no longer reaches `device`: it is no longer here.
    pub fn left(&self, device: &DeviceId) {
        if let Some(live) = self.0.state.lock_recover().live.get_mut(device) {
            live.reachable = false;
        }
    }

    /// The person's devices heard on this device's local network now.
    pub fn heard_nearby(&self, devices: BTreeSet<DeviceId>) {
        let local = self.local().clone();
        self.0.state.lock_recover().nearby = devices
            .into_iter()
            .filter(|device| device != &local)
            .collect();
    }

    /// The person's devices that are here: reached by the link and said themselves on it.
    #[must_use]
    pub fn online(&self) -> BTreeSet<DeviceId> {
        self.0
            .state
            .lock_recover()
            .live
            .iter()
            .filter(|(_, live)| live.reachable && live.presence.is_some())
            .map(|(device, _)| device.clone())
            .collect()
    }

    /// The person's devices as this one knows them: what each keeps of itself in the person's store,
    /// with what each said live put in. A device the link reached that the store does not list yet
    /// is the person's all the same: it proved a certificate of theirs.
    #[must_use]
    pub fn devices(&self) -> Devices {
        let abouts: Vec<(DeviceId, DeviceAbout)> = read_facts(self.0.facts.as_ref(), ABOUT);
        let mut devices = Devices::new(
            abouts
                .into_iter()
                .filter(|(_, about)| about.validate().is_ok())
                .map(|(device, about)| Device::new(device, about)),
        );
        devices.insert(Device::new(self.local().clone(), self.0.about.clone()));
        let state = self.0.state.lock_recover();
        for (device, live) in &state.live {
            if !devices.contains(device) {
                devices.insert(Device::new(
                    device.clone(),
                    DeviceAbout::named(live.label.clone()),
                ));
            }
        }
        let now = tilo_clock::unix_now_ms();
        let ids: Vec<DeviceId> = devices.devices().keys().cloned().collect();
        for id in ids {
            let Some(device) = devices.get_mut(&id) else {
                continue;
            };
            device.nearby = state.nearby.contains(&id);
            if id == self.0.local {
                device.online = true;
                device.last_seen_unix_ms = now;
                device.presence = state.own.clone();
            } else if let Some(live) = state.live.get(&id) {
                device.online = live.reachable && live.presence.is_some();
                device.last_seen_unix_ms = live.last_seen_unix_ms;
                device.presence = live.presence.clone().unwrap_or_default();
            }
        }
        drop(state);
        devices
    }

    /// The name the person knows `device` by.
    #[must_use]
    pub fn label_of(&self, device: &DeviceId) -> String {
        self.devices().label_of(device)
    }

    /// Tell `to`, or every device here, what this device says of itself now.
    fn tell_presence(&self, to: Option<&DeviceId>) {
        let presence = self.0.state.lock_recover().own.clone();
        let targets: Vec<DeviceId> = to.map_or_else(
            || {
                self.0
                    .state
                    .lock_recover()
                    .live
                    .iter()
                    .filter(|(_, live)| live.reachable)
                    .map(|(device, _)| device.clone())
                    .collect()
            },
            |device| vec![device.clone()],
        );
        for device in targets {
            spend(
                self.call(
                    &device,
                    PROVIDER,
                    "presence",
                    "tell",
                    Told {
                        presence: presence.clone(),
                    },
                ),
                "this device's presence",
            );
        }
    }

    /// Change what this device says of itself, telling the others when it changed.
    fn say(&self, change: impl FnOnce(&mut DevicePresence)) {
        let changed = {
            let mut state = self.0.state.lock_recover();
            let before = state.own.clone();
            change(&mut state.own);
            state.own != before
        };
        if changed {
            self.tell_presence(None);
        }
    }

    /// Re-read this machine's interfaces when they are due, move the wakes this device watches on,
    /// and tell the person how the ones that ended went. The host calls this about once a second.
    pub fn tick(&self, now: Instant) {
        self.read_routes(now);
        let online = self.online();
        let devices = self.devices();
        let steps = {
            let mut state = self.0.state.lock_recover();
            let mut steps = state.waker.rejoins(&online);
            steps.extend(state.waker.poll(now));
            state
                .waker
                .abandoned(now, |device| devices.label_of(device));
            steps
        };
        for step in steps {
            self.carry_out(step, &devices);
        }
        self.keep_wake_outcomes();
    }

    /// This machine's wake routes, read again when [`ROUTE_SCAN_INTERVAL`] passed; an interface
    /// that was last seen on a network keeps that subnet while it is off it.
    fn read_routes(&self, now: Instant) -> Vec<crate::WakeRoute> {
        let due = self
            .0
            .state
            .lock_recover()
            .routes_read
            .is_none_or(|at| now.saturating_duration_since(at) >= ROUTE_SCAN_INTERVAL);
        if due {
            let read = self.0.interfaces.routes();
            let previous = {
                let mut state = self.0.state.lock_recover();
                state.routes_read = Some(now);
                state.own.wake_routes.clone()
            };
            let routes = remember_subnets(read, &previous);
            self.say(|presence| presence.wake_routes = routes);
        }
        self.0.state.lock_recover().own.wake_routes.clone()
    }

    fn keep_wake_outcomes(&self) {
        let mut state = self.0.state.lock_recover();
        let outcomes = state.waker.outcomes();
        for outcome in outcomes {
            if state.wakes.len() >= MAX_WAKE_EVENTS {
                state.wakes.pop_front();
            }
            state.wakes.push_back(outcome);
        }
    }

    /// Do the next thing a watch asks for: ask the woken device whether it is back, or tell whoever
    /// asked how it ended.
    fn carry_out(&self, step: WatchStep, devices: &Devices) {
        match step {
            WatchStep::Probe { target, nonce } => {
                // Unanswered until the target's link is up again.
                let fabric = self.clone();
                let asked = target.clone();
                self.call(&target, PROVIDER, "wake", "probe", Probe { nonce })
                    .then(move |answer| {
                        let Some(Ok(value)) = answer else {
                            return;
                        };
                        let Ok(reply) = serde_json::from_value::<Probe>(value) else {
                            return;
                        };
                        let step = fabric
                            .0
                            .state
                            .lock_recover()
                            .waker
                            .answered(&asked, reply.nonce);
                        if let Some(step) = step {
                            let devices = fabric.devices();
                            fabric.carry_out(step, &devices);
                            fabric.keep_wake_outcomes();
                        }
                    });
            }
            WatchStep::Finished {
                target,
                requester: Some(requester),
                outcome,
            } => spend(
                self.call(
                    &requester,
                    PROVIDER,
                    "wake",
                    "report",
                    Report { target, outcome },
                ),
                "a wake's outcome",
            ),
            finished @ WatchStep::Finished { .. } => {
                let WatchStep::Finished { target, .. } = &finished else {
                    return;
                };
                let label = devices.label_of(target);
                self.0.state.lock_recover().waker.tell_step(finished, label);
            }
        }
    }

    /// Send this machine's magic packets for `target` and start watching for it on behalf of
    /// `requester`.
    fn wake_from_here(&self, target: &DeviceId, requester: Option<DeviceId>) -> WakeOutcome {
        let routes = self.read_routes(Instant::now());
        let packets = magic_packets_for(&self.devices(), &routes, target);
        let Some((_, subnet)) = packets.first().copied() else {
            return WakeOutcome::NotOnTargetLan;
        };
        let mut sent = false;
        for (mac, subnet) in &packets {
            match self.0.packets.send(*mac, *subnet) {
                Ok(()) => sent = true,
                Err(error) => {
                    tracing::warn!(%error, %subnet, "a wake-up signal could not be sent");
                }
            }
        }
        if !sent {
            return WakeOutcome::SendFailed;
        }
        self.0
            .state
            .lock_recover()
            .waker
            .started(target.clone(), requester, Instant::now());
        WakeOutcome::Sent { subnet }
    }

    /// Wake the device the person named, by its id or its label: this device sends the packets when
    /// it shares the sleeping device's network, else asks the device that does.
    fn wake(&self, name: &str) -> Result<Woken, ActionCallError> {
        let refused = |reason: String| ActionCallError::Refused { reason };
        let devices = self.devices();
        let target =
            resolve_device(&devices, name).map_err(|refusal| refused(refusal.to_string()))?;
        let routes = self.read_routes(Instant::now());
        let online = self.online();
        let plan = plan_wake(&devices, self.local(), &routes, &target, |device| {
            online.contains(device)
        })
        .map_err(|refusal| refused(refusal.to_string()))?;
        let waker = devices.label_of(&plan.waker);
        if &plan.waker != self.local() {
            self.0.state.lock_recover().waker.asked(
                target.clone(),
                plan.waker.clone(),
                Instant::now(),
            );
            let fabric = self.clone();
            let asked = plan.waker.clone();
            let woken = target.clone();
            self.call(&plan.waker, PROVIDER, "wake", "ask", Ask { target })
                .then(move |answer| {
                    let outcome = match answer {
                        Some(Ok(value)) => serde_json::from_value::<Asked>(value)
                            .map_or(WakeOutcome::SendFailed, |asked| asked.outcome),
                        _ => WakeOutcome::SendFailed,
                    };
                    fabric.reported(&asked, &woken, outcome);
                });
            return Ok(Woken {
                waker,
                asked: true,
                subnet: plan.subnet.to_string(),
            });
        }
        match self.wake_from_here(&target, None) {
            WakeOutcome::Sent { subnet } => Ok(Woken {
                waker,
                asked: false,
                subnet: subnet.to_string(),
            }),
            _ => Err(refused(format!(
                "could not send a wake-up signal to {} on {}",
                devices.label_of(&target),
                plan.subnet
            ))),
        }
    }

    /// The waker `from` says how waking `target` went.
    fn reported(&self, from: &DeviceId, target: &DeviceId, outcome: WakeOutcome) {
        let label = self.label_of(target);
        self.0
            .state
            .lock_recover()
            .waker
            .reported(from, target, outcome, label);
        self.keep_wake_outcomes();
    }

    /// The session here said where the person's session on this device stands.
    fn say_lock(&self, said: SayLock) -> Followed {
        let changed = {
            let mut state = self.0.state.lock_recover();
            match said.change {
                Some(LockChange::Person) => state
                    .follow
                    .changed_by_person(tilo_clock::unix_now_ms().max(said.input_unix_ms)),
                Some(LockChange::Idle) => state.follow.locked_by_idle(),
                None => {}
            }
            state.said = Some(Said {
                locked: said.locked,
                input_unix_ms: said.input_unix_ms,
                in_use: said.in_use,
            });
            let session = state.follow.presence(said.locked, said.input_unix_ms);
            let changed = state.own.session != session;
            state.own.session = session;
            changed
        };
        if changed {
            self.tell_presence(None);
        }
        Followed {
            followed: self.follow_lock(),
        }
    }

    /// Follow a lock or unlock on another of the person's devices, when the rules say this device
    /// does: the session here is asked to lock or unlock.
    fn follow_lock(&self) -> Option<Follow> {
        let follow = {
            let mut state = self.0.state.lock_recover();
            let said = state.said?;
            let own = state.own.clone();
            let others: Vec<(OwnerSession, bool)> = state
                .live
                .values()
                .filter_map(|live| live.presence.as_ref())
                .map(|other| (other.session, shares_network(&own, other)))
                .collect();
            state
                .follow
                .decide(said.locked, said.input_unix_ms, said.in_use, others)?
        };
        spend(
            self.0.seat.call(
                "lock",
                "follow",
                serde_json::json!({ "locked": follow == Follow::Lock }),
            ),
            "the lock following the person",
        );
        Some(follow)
    }

    fn owner(&self) -> OwnerReading {
        let devices = self.devices();
        let state = self.0.state.lock_recover();
        let others: Vec<(&DevicePresence, bool)> = devices
            .devices()
            .values()
            .filter(|device| device.device != self.0.local)
            .map(|device| (&device.presence, device.online))
            .collect();
        OwnerReading {
            locked: state.own.session.locked,
            followed_lock: state.follow.followed_lock(),
            used_nearby_unix_ms: latest_use_nearby(&state.own, others.iter().copied()),
            devices: devices
                .devices()
                .values()
                .filter(|device| device.device != self.0.local && device.online)
                .map(|device| OwnerRow {
                    id: device.device.as_str().to_owned(),
                    label: device.about.label.clone(),
                    locked: device.presence.session.locked,
                    nearby: shares_network(&state.own, &device.presence),
                    active_unix_ms: device.presence.session.active_unix_ms,
                })
                .collect(),
        }
    }

    /// Another device said what it is now.
    fn told(&self, from: &DeviceId, presence: DevicePresence) -> Result<(), ActionCallError> {
        presence
            .validate()
            .map_err(|error| ActionCallError::InvalidRequest {
                reason: error.to_string(),
            })?;
        {
            let mut state = self.0.state.lock_recover();
            let live = state.live.entry(from.clone()).or_default();
            live.reachable = true;
            live.presence = Some(presence);
            live.last_seen_unix_ms = tilo_clock::unix_now_ms();
            drop(state);
        }
        self.follow_lock();
        Ok(())
    }

    // ---- disk unlock

    /// The enrolments, auto-approvals and answers the person's store holds now, under the fleet
    /// this device trusts.
    fn published(&self, ownership: &Ownership) -> MergedUnlock {
        let states: Vec<(DeviceId, UnlockFabricState)> = read_facts(self.0.facts.as_ref(), UNLOCK);
        merged_unlock_state(
            states
                .iter()
                .filter(|(owner, state)| state.validate(owner).is_ok())
                .map(|(_, state)| state),
            &ownership.fleet,
            tilo_clock::unix_now_ns(),
        )
    }

    /// Change what this device keeps of its disk unlock: its own part of the person's store is the
    /// only one it writes.
    fn keep_unlock(&self, change: impl FnOnce(&mut UnlockFabricState)) {
        let _serialized = self.0.unlock_kept.lock_recover();
        let mut state = read_facts::<UnlockFabricState>(self.0.facts.as_ref(), UNLOCK)
            .into_iter()
            .find(|(owner, _)| owner == &self.0.local)
            .map(|(_, state)| state)
            .unwrap_or_default();
        change(&mut state);
        if let Err(error) = keep_fact(self.0.facts.as_ref(), UNLOCK, &state) {
            tracing::warn!(%error, "this device's disk unlock is not kept");
        }
    }

    fn label_of_key(&self, key: &[u8; 32]) -> String {
        self.label_of(&DeviceId::of_key(key))
    }

    /// Answer a boot's hello with a challenge, or refuse it.
    fn unlock_hello(&self, hello: &UnlockHello) -> SiblingFrame {
        let Some(ownership) = self.0.keys.ownership() else {
            return SiblingFrame::Refused(RefusalReason::NotAKeyHolder);
        };
        let published = self.published(&ownership);
        let view = view_of(&ownership, &published, None);
        let key = self.0.key;
        self.0.state.lock_recover().sibling(key).hello(
            hello,
            &view,
            crate::nonzero::<32>(),
            tilo_clock::unix_now_ns(),
        )
    }

    /// Take a boot's answer to its challenge: released or denied at once where the person already
    /// answered this boot or allowed it unattended, else put to the person.
    fn unlock_request(&self, hello: &UnlockHello, request: &UnlockRequest) -> SiblingFrame {
        let Some(ownership) = self.0.keys.ownership() else {
            return SiblingFrame::Refused(RefusalReason::NotAKeyHolder);
        };
        let now = tilo_clock::unix_now_ns();
        let published = self.published(&ownership);
        // The owner's answer to this boot, when their devices kept one, is the whole exchange: an
        // approval needs the half, a denial needs nothing loaded.
        let boot_digest = boot_cert_digest(&hello.boot_cert);
        let allowed_before = boot_digest
            .and_then(|digest| {
                decision_for(
                    &published.decisions,
                    &digest,
                    request.boot_id,
                    request.slot,
                    now,
                )
            })
            .is_some_and(|record| record.outcome == DecisionOutcome::Approve);
        let secret_sync = if allowed_before && ownership.key_holder {
            self.0.keys.secret_sync()
        } else {
            None
        };
        let key = self.0.key;
        let frame = {
            let view = view_of(&ownership, &published, secret_sync.as_ref());
            self.0.state.lock_recover().sibling(key).request(
                request,
                hello,
                &request.nonce,
                &view,
                now,
            )
        };
        if !matches!(frame, SiblingFrame::Pending { .. }) {
            return frame;
        }
        let Ok(digest) = request.digest(&hello.boot_cert, &key) else {
            return SiblingFrame::Refused(RefusalReason::Malformed);
        };
        let mut state = self.0.state.lock_recover();
        let Some(boot) = state
            .sibling(key)
            .pending()
            .find(|pending| pending.request_digest == digest)
            .map(|pending| Boot {
                code: pending.approval_code.clone(),
                device: pending.device_public_key,
                slot: slot_name(&pending.slot),
                decided: None,
            })
        else {
            return SiblingFrame::Refused(RefusalReason::Malformed);
        };
        state.boots.insert(digest, boot);
        frame
    }

    /// Where a boot's request stands: the person's answer once they gave it, else still waiting.
    fn unlock_poll(&self, hello: &UnlockHello, request: &UnlockRequest) -> SiblingFrame {
        let Ok(digest) = request.digest(&hello.boot_cert, &self.0.key) else {
            return SiblingFrame::Refused(RefusalReason::Malformed);
        };
        let mut state = self.0.state.lock_recover();
        match state.boots.get(&digest).map(|boot| boot.decided.clone()) {
            Some(Some(frame)) => {
                state.boots.remove(&digest);
                frame
            }
            Some(None) => {
                let waiting = state.unlock.as_ref().is_some_and(|sibling| {
                    sibling
                        .pending()
                        .any(|pending| pending.request_digest == digest)
                });
                if waiting {
                    let code = state
                        .boots
                        .get(&digest)
                        .map(|boot| boot.code.clone())
                        .unwrap_or_default();
                    SiblingFrame::Pending {
                        approval_code: code,
                    }
                } else {
                    // Nobody answered within the window, which refuses this boot.
                    state.boots.remove(&digest);
                    SiblingFrame::Denied
                }
            }
            None => SiblingFrame::Refused(RefusalReason::UnknownChallenge),
        }
    }

    /// The boot went away: its question goes with it.
    fn unlock_withdraw(&self, hello: &UnlockHello, request: &UnlockRequest) {
        let Ok(digest) = request.digest(&hello.boot_cert, &self.0.key) else {
            return;
        };
        let key = self.0.key;
        let mut state = self.0.state.lock_recover();
        state.sibling(key).withdraw(&digest);
        state.boots.remove(&digest);
    }

    /// Another device without the person's root asks this one to enrol its boot key: put to the
    /// person, unless this device could not enrol it anyway.
    fn enrol_asked(&self, device: [u8; 32], request: EnrolRequest) -> Option<EnrolReply> {
        let Some(ownership) = self.0.keys.ownership() else {
            return Some(EnrolReply::Refused(
                "this device holds no identity for the person's devices".into(),
            ));
        };
        if !ownership.key_holder {
            return Some(EnrolReply::NotKeyHolder);
        }
        if ownership
            .fleet
            .certification(&device, tilo_clock::unix_now_ns())
            != DeviceCertification::Certified
        {
            // Nobody is asked for a device that is not the person's.
            return Some(EnrolReply::Refused(
                "this device is not one of the person's".into(),
            ));
        }
        if request.verify().is_err() {
            return Some(EnrolReply::Refused(
                "the enrolment request does not prove its boot key".into(),
            ));
        }
        self.0.state.lock_recover().enrols.insert(
            device,
            Enrol {
                request,
                answer: None,
            },
        );
        None
    }

    /// The answer to `device`'s enrolment, once the person gave it.
    fn enrol_poll(&self, device: [u8; 32]) -> Option<EnrolReply> {
        let mut state = self.0.state.lock_recover();
        let waiting = state.enrols.get(&device).map(|enrol| enrol.answer.clone());
        if matches!(waiting, Some(Some(_))) {
            state.enrols.remove(&device);
        }
        drop(state);
        match waiting {
            Some(answer) => answer,
            None => Some(EnrolReply::Refused(
                "no enrolment of this device waits".into(),
            )),
        }
    }

    /// Enrol `enrollee`'s boot key here and keep the enrolment for the person's other devices.
    fn enrol_here(&self, request: &EnrolRequest, enrollee: [u8; 32]) -> EnrolReply {
        let outcome = enrol(
            self.0.keys.as_ref(),
            request,
            enrollee,
            self.0.key,
            |key| self.label_of_key(key),
            tilo_clock::unix_now_ns(),
        );
        enrol_reply(outcome.map(|enrolled| {
            self.keep_unlock(|state| with_enrolment(state, enrolled.enrolment, enrolled.approval));
            enrolled.response
        }))
    }

    /// The questions waiting for the person: boots asking to unlock, devices asking to enrol.
    #[expect(
        clippy::needless_collect,
        reason = "the asks are read under the state lock, which naming their devices takes again"
    )]
    fn unlock_asks(&self) -> Vec<UnlockAsk> {
        let state = self.0.state.lock_recover();
        let boots: Vec<(String, Boot)> = state
            .boots
            .iter()
            .filter(|(_, boot)| boot.decided.is_none())
            .map(|(digest, boot)| (hex::encode(digest), boot.clone()))
            .collect();
        let enrols: Vec<(String, bool, [u8; 32])> = state
            .enrols
            .iter()
            .filter(|(_, enrol)| enrol.answer.is_none())
            .map(|(device, enrol)| {
                (
                    hex::encode(device),
                    enrol.request.unattended_chain.is_some(),
                    *device,
                )
            })
            .collect();
        drop(state);
        boots
            .into_iter()
            .map(|(digest, boot)| UnlockAsk {
                ask: format!("unlock:{digest}"),
                device: self.label_of_key(&boot.device),
                approval_code: Some(boot.code),
                slot: Some(boot.slot),
                unattended: false,
            })
            .chain(
                enrols
                    .into_iter()
                    .map(|(id, unattended, device)| UnlockAsk {
                        ask: format!("enrol:{id}"),
                        device: self.label_of_key(&device),
                        approval_code: None,
                        slot: None,
                        unattended,
                    }),
            )
            .collect()
    }

    /// The person answered one of the questions [`Self::unlock_asks`] lists.
    fn answer_unlock(&self, answer: &Answer) -> Result<(), ActionCallError> {
        let gone = || ActionCallError::Refused {
            reason: "that device no longer waits for an answer".to_owned(),
        };
        let key = |hex_key: &str| -> Result<[u8; 32], ActionCallError> {
            let mut key = [0; 32];
            hex::decode_to_slice(hex_key, &mut key).map_err(|_| gone())?;
            Ok(key)
        };
        if let Some(digest) = answer.ask.strip_prefix("unlock:") {
            let digest = key(digest)?;
            let ownership = self.0.keys.ownership().ok_or_else(gone)?;
            let published = self.published(&ownership);
            let secret_sync = if answer.allow && ownership.key_holder {
                self.0.keys.secret_sync()
            } else {
                None
            };
            let decision = if answer.allow {
                UnlockDecision::Approve {
                    approver: self.0.key,
                }
            } else {
                UnlockDecision::Deny {
                    approver: self.0.key,
                }
            };
            let now = tilo_clock::unix_now_ns();
            let local = self.0.key;
            let decided = {
                let view = view_of(&ownership, &published, secret_sync.as_ref());
                self.0
                    .state
                    .lock_recover()
                    .sibling(local)
                    .decide(&digest, decision, &view, now)
                    .ok_or_else(gone)?
            };
            if let Some(record) = decided.record {
                self.keep_unlock(|state| with_decision(state, record, now));
            }
            if let Some(boot) = self.0.state.lock_recover().boots.get_mut(&digest) {
                boot.decided = Some(decided.frame);
            }
            return Ok(());
        }
        if let Some(device) = answer.ask.strip_prefix("enrol:") {
            let device = key(device)?;
            let request = self
                .0
                .state
                .lock_recover()
                .enrols
                .get(&device)
                .filter(|enrol| enrol.answer.is_none())
                .map(|enrol| enrol.request.clone())
                .ok_or_else(gone)?;
            let reply = if answer.allow {
                self.enrol_here(&request, device)
            } else {
                EnrolReply::Denied
            };
            if let Some(enrol) = self.0.state.lock_recover().enrols.get_mut(&device) {
                enrol.answer = Some(reply);
            }
            return Ok(());
        }
        Err(gone())
    }

    fn unlock_reading(&self) -> UnlockReading {
        let Some(ownership) = self.0.keys.ownership() else {
            return UnlockReading::default();
        };
        let published = self.published(&ownership);
        UnlockReading {
            key_holder: ownership.key_holder,
            enrolled: published
                .enrolments
                .values()
                .map(|enrolment| EnrolledRow {
                    device: self.label_of_key(&enrolment.device()),
                    slot: slot_name(&enrolment.slot),
                    unattended: published.auto_approvals.contains_key(&enrolment.device()),
                })
                .collect(),
            answers: published.decisions.len(),
        }
    }

    // ---- providers

    /// The fabric's resources and actions, as the device's action host mounts them under
    /// [`PROVIDER`].
    #[must_use]
    #[expect(
        clippy::too_many_lines,
        reason = "one table of the fabric's actions, each beside the closure that answers it"
    )]
    pub fn provider(&self) -> Arc<dyn ActionProvider + Send + Sync> {
        let at = || self.clone();
        Provider::new(PROVIDER, self.local().clone(), *self.person())
            .read("devices", "The person's devices", {
                let fabric = at();
                move || fabric.devices().listed(fabric.local())
            })
            .act(
                "presence",
                "tell",
                "What a device says of itself",
                Asker::Device,
                {
                    let fabric = at();
                    move |who, told: Told| fabric.told(asking_device(who)?, told.presence)
                },
            )
            .act(
                "presence",
                "say-display",
                "Say where this device's screens stand",
                Asker::Here,
                {
                    let fabric = at();
                    move |_, said: SayDisplay| {
                        if let Some(display) = &said.display {
                            display.validate().map_err(|error| invalid(&error))?;
                        }
                        fabric.say(|presence| presence.display = said.display);
                        Ok(())
                    }
                },
            )
            .read("owner", "The lock across the person's devices", {
                let fabric = at();
                move || fabric.owner()
            })
            .act(
                "owner",
                "say-lock",
                "Say where the person's session here stands",
                Asker::Here,
                {
                    let fabric = at();
                    move |_, said: SayLock| Ok(fabric.say_lock(said))
                },
            )
            .read("wake", "How the person's wakes went", {
                let fabric = at();
                move || -> Vec<WakeEvent> {
                    fabric
                        .0
                        .state
                        .lock_recover()
                        .wakes
                        .iter()
                        .cloned()
                        .collect()
                }
            })
            .act(
                "wake",
                "wake",
                "Wake one of the person's devices",
                Asker::Here,
                {
                    let fabric = at();
                    move |_, wake: WakeDevice| fabric.wake(&wake.device)
                },
            )
            .act(
                "wake",
                "ask",
                "Wake a device on this device's network",
                Asker::Device,
                {
                    let fabric = at();
                    move |who, ask: Ask| {
                        let from = asking_device(who)?.clone();
                        Ok(Asked {
                            outcome: fabric.wake_from_here(&ask.target, Some(from)),
                        })
                    }
                },
            )
            .act(
                "wake",
                "report",
                "How a wake asked of another device went",
                Asker::Device,
                {
                    let fabric = at();
                    move |who, report: Report| {
                        fabric.reported(asking_device(who)?, &report.target, report.outcome);
                        Ok(())
                    }
                },
            )
            .act(
                "wake",
                "probe",
                "Whether this device is awake",
                Asker::Device,
                |_, probe: Probe| Ok(probe),
            )
            .read("unlock", "Disk unlock across the person's devices", {
                let fabric = at();
                move || fabric.unlock_reading()
            })
            .read(
                "unlock-asks",
                "Devices waiting for the person to unlock or enrol them",
                {
                    let fabric = at();
                    move || fabric.unlock_asks()
                },
            )
            .act(
                "unlock",
                "answer",
                "Answer a device waiting to unlock or enrol",
                Asker::Here,
                {
                    let fabric = at();
                    move |_, answer: Answer| fabric.answer_unlock(&answer)
                },
            )
            .act(
                "unlock",
                "enrol-boot-key",
                "Enrol this device's boot key",
                Asker::Here,
                {
                    let fabric = at();
                    move |_, enrol: Packed| -> Result<Packed, ActionCallError> {
                        let request: EnrolRequest = unpack(&enrol.frame)?;
                        let reply = fabric.enrol_here(&request, fabric.0.key);
                        Ok(Packed {
                            frame: pack(&reply)?,
                        })
                    }
                },
            )
            .act(
                "unlock",
                "forget-device",
                "Forget a device that left the person's devices",
                Asker::Here,
                {
                    let fabric = at();
                    move |_, forget: Forget| {
                        let key = DeviceId::new(forget.device)
                            .ok()
                            .and_then(|device| device.key())
                            .ok_or_else(|| ActionCallError::InvalidRequest {
                                reason: "a device is named by its key".to_owned(),
                            })?;
                        let local = fabric.0.key;
                        fabric
                            .0
                            .state
                            .lock_recover()
                            .sibling(local)
                            .remove_device(key);
                        Ok(())
                    }
                },
            )
            .shared()
    }

    /// What a booting device with no certificate yet may call, under [`UNLOCK_SCOPE`]: unlocking
    /// its disk and enrolling its boot key. The daemon admits such a device to this scope alone.
    #[must_use]
    pub fn unlock_scope(&self) -> Arc<dyn ActionProvider + Send + Sync> {
        let at = || self.clone();
        Provider::uncertified(UNLOCK_SCOPE, self.local().clone(), *self.person())
            .act("unlock", "hello", "A boot says who it is", Asker::Device, {
                let fabric = at();
                move |_, boot: Hello| -> Result<Packed, ActionCallError> {
                    let hello: UnlockHello = unpack(&boot.hello)?;
                    Ok(Packed {
                        frame: pack(&fabric.unlock_hello(&hello))?,
                    })
                }
            })
            .act(
                "unlock",
                "request",
                "A boot answers its challenge",
                Asker::Device,
                {
                    let fabric = at();
                    move |_, boot: BootRequest| -> Result<Packed, ActionCallError> {
                        let (hello, request) = boot.unpack()?;
                        Ok(Packed {
                            frame: pack(&fabric.unlock_request(&hello, &request))?,
                        })
                    }
                },
            )
            .act(
                "unlock",
                "poll",
                "Where a boot's request stands",
                Asker::Device,
                {
                    let fabric = at();
                    move |_, boot: BootRequest| -> Result<Packed, ActionCallError> {
                        let (hello, request) = boot.unpack()?;
                        Ok(Packed {
                            frame: pack(&fabric.unlock_poll(&hello, &request))?,
                        })
                    }
                },
            )
            .act(
                "unlock",
                "withdraw",
                "A boot stops waiting",
                Asker::Device,
                {
                    let fabric = at();
                    move |_, boot: BootRequest| {
                        let (hello, request) = boot.unpack()?;
                        fabric.unlock_withdraw(&hello, &request);
                        Ok(())
                    }
                },
            )
            .act(
                "enrol",
                "ask",
                "A device without the person's root asks to enrol its boot key",
                Asker::Device,
                {
                    let fabric = at();
                    move |who, enrol: Packed| -> Result<Polled, ActionCallError> {
                        let device = asking_key(who)?;
                        let request: EnrolRequest = unpack(&enrol.frame)?;
                        Polled::of(fabric.enrol_asked(device, request).as_ref())
                    }
                },
            )
            .act(
                "enrol",
                "poll",
                "Where an enrolment stands",
                Asker::Device,
                {
                    let fabric = at();
                    move |who, _: Empty| -> Result<Polled, ActionCallError> {
                        Polled::of(fabric.enrol_poll(asking_key(who)?).as_ref())
                    }
                },
            )
            .shared()
    }
}

fn invalid(error: &crate::FabricError) -> ActionCallError {
    ActionCallError::InvalidRequest {
        reason: error.to_string(),
    }
}

fn asking_key(who: &Who) -> Result<[u8; 32], ActionCallError> {
    asking_device(who)?
        .key()
        .ok_or(ActionCallError::NotPermitted)
}

// ---- what the actions take and answer

/// `presence#tell`: what a device says of itself.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Told {
    pub presence: DevicePresence,
}

/// `presence#say-display`: where this device's screens stand, or none offered.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SayDisplay {
    #[serde(default)]
    pub display: Option<DeviceDisplay>,
}

/// Why the person's session here changed, when it was a lock or unlock of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LockChange {
    /// The person locked or unlocked this device themselves.
    Person,
    /// The idle timer locked it (or the lock before an idle sleep).
    Idle,
}

/// `owner#say-lock`: where the person's session on this device stands.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct SayLock {
    pub locked: bool,
    /// The person's last input here.
    pub input_unix_ms: u64,
    /// Whether the person is using this device now.
    #[serde(default)]
    pub in_use: bool,
    /// What changed it, when it changed by the person or the idle timer.
    #[serde(default)]
    pub change: Option<LockChange>,
}

/// What `owner#say-lock` followed of the person's other devices.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Followed {
    pub followed: Option<Follow>,
}

/// The `owner` resource: the lock here and on the person's devices that are here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerReading {
    pub locked: bool,
    /// Whether the lock here is one followed from another device.
    pub followed_lock: bool,
    /// The person's last input on a device of theirs nearby: idle time here counts from it.
    pub used_nearby_unix_ms: Option<u64>,
    pub devices: Vec<OwnerRow>,
}

/// One device of the person's in the `owner` resource.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerRow {
    pub id: String,
    pub label: String,
    pub locked: bool,
    /// Whether it shares a network with this device.
    pub nearby: bool,
    pub active_unix_ms: u64,
}

/// `wake#wake`: the device to wake, by its id or its name.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WakeDevice {
    pub device: String,
}

/// What `wake#wake` started.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Woken {
    /// The device that sends the wake-up signal.
    pub waker: String,
    /// Whether another device was asked to send it.
    pub asked: bool,
    pub subnet: String,
}

/// `wake#ask`: wake `target` from the asked device's network.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Ask {
    pub target: DeviceId,
}

/// What `wake#ask` did.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Asked {
    pub outcome: WakeOutcome,
}

/// `wake#report`: how waking `target` went.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Report {
    pub target: DeviceId,
    pub outcome: WakeOutcome,
}

/// `wake#probe`, answered with itself by a device that is awake.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Probe {
    pub nonce: [u8; 16],
}

/// `unlock#answer`: the person's answer to one waiting device.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Answer {
    /// The question, as `unlock-asks` names it.
    pub ask: String,
    pub allow: bool,
}

/// `unlock#forget-device`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Forget {
    pub device: String,
}

/// One question in the `unlock-asks` resource.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnlockAsk {
    /// What `unlock#answer` names it by.
    pub ask: String,
    /// The device that asks, as the person's store names it.
    pub device: String,
    /// The code the boot screen shows, for an unlock; the person compares them.
    pub approval_code: Option<String>,
    /// The slot it wants unlocked.
    pub slot: Option<String>,
    /// For an enrolment: whether it asked to unlock without being asked each boot.
    pub unattended: bool,
}

/// The `unlock` resource.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnlockReading {
    /// Whether this device holds the person's root, and so releases halves.
    pub key_holder: bool,
    pub enrolled: Vec<EnrolledRow>,
    /// Answers to boots still in force across the person's devices.
    pub answers: usize,
}

/// One enrolled disk key in the `unlock` resource.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrolledRow {
    pub device: String,
    pub slot: String,
    pub unattended: bool,
}

/// A protocol value carried as the hex of its postcard bytes ([`pack`]).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Packed {
    pub frame: String,
}

/// `unlock#hello`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Hello {
    pub hello: String,
}

/// `unlock#request`, `#poll` and `#withdraw`: the boot's hello and its answer to the challenge.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BootRequest {
    pub hello: String,
    pub request: String,
}

impl BootRequest {
    /// # Errors
    /// [`ActionCallError::InvalidRequest`] for frames that do not read.
    pub fn unpack(&self) -> Result<(UnlockHello, UnlockRequest), ActionCallError> {
        Ok((unpack(&self.hello)?, unpack(&self.request)?))
    }
}

/// What `enrol#ask` and `#poll` answer: the reply, or none while the person has not answered.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Polled {
    pub reply: Option<String>,
}

impl Polled {
    fn of(reply: Option<&EnrolReply>) -> Result<Self, ActionCallError> {
        Ok(Self {
            reply: reply.map(pack).transpose()?,
        })
    }
}

/// An action that takes nothing.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Empty {}
