//! Moving one of the person's Bluetooth accessories to another of their devices, published as
//! [`PROVIDER`].
//!
//! A bond belongs to the accessory, not to a device: moving it means the device that has it lets
//! go, and the device it moves to connects it with the bond it already holds. Nothing is copied.
//! The two devices agree through each other's actions — prepare, prepared, commit, finished,
//! accepted — and each does its own Bluetooth work away from the call that asked for it
//! ([`Bluetooth::run`]), so no call ever waits on `BlueZ`.
//!
//! Only an accessory the destination already holds a bond with moves this way: a destination that
//! has none says so, and the accessory stays where it is rather than asking the person to pair it
//! again. A move that the two devices do not finish — the destination leaves, a worked step fails,
//! or [`WAIT`] runs out — puts the accessory back on the device it came from, so a failed move never
//! leaves it connected to neither. What the person is told of each move goes to the session here
//! as a typed notice (`notices#tell`, [`Notice`]).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tilo_actions::remote::{ActionCallError, ActionProvider, PendingPoll};
use tilo_fabric::actions::{Asker, Provider, asking_device};
use tilo_fabric::{DeviceId, Fabric, nonzero, spend};
use tilo_lock::MutexRecover as _;

/// The provider accessory moves are published under.
pub const PROVIDER: &str = "so.tilo.accessories";

/// How long one move may take before both devices give it up and the accessory goes back where it
/// was.
pub const WAIT: Duration = Duration::from_secs(75);

/// How many moves one device may have under way at once.
pub const MOST: usize = 32;

/// The Bluetooth work a move does on this device.
pub trait Bluetooth: Send + Sync {
    /// The name of the accessory at `address`, while it is connected here.
    ///
    /// # Errors
    /// It is not connected here.
    fn connected_name(&self, address: &str) -> Result<String, String>;

    /// Whether this device holds the bond of the accessory at `address`.
    fn paired(&self, address: &str) -> bool;

    /// Let go of the accessory, keeping its bond.
    ///
    /// # Errors
    /// Why it did not let go.
    fn disconnect(&self, address: &str) -> Result<(), String>;

    /// Connect the accessory with the bond this device holds.
    ///
    /// # Errors
    /// Why it did not connect.
    fn connect(&self, address: &str) -> Result<(), String>;

    /// Run `work` away from the call that asked for it.
    fn run(&self, work: Box<dyn FnOnce() + Send>) {
        std::thread::spawn(work);
    }
}

/// The Bluetooth stack of this machine.
#[derive(Clone, Copy, Debug, Default)]
pub struct Bluez;

impl Bluetooth for Bluez {
    fn connected_name(&self, address: &str) -> Result<String, String> {
        let accessory = os_actions::bluez::paired_devices()?
            .into_iter()
            .find(|device| device.address.eq_ignore_ascii_case(address) && device.connected)
            .ok_or_else(|| "the accessory is not connected here".to_owned())?;
        Ok(accessory.name)
    }

    fn paired(&self, address: &str) -> bool {
        os_actions::bluez::paired_devices().is_ok_and(|devices| {
            devices
                .iter()
                .any(|device| device.address.eq_ignore_ascii_case(address) && device.paired)
        })
    }

    fn disconnect(&self, address: &str) -> Result<(), String> {
        os_actions::bluez::disconnect_address(address)
    }

    fn connect(&self, address: &str) -> Result<(), String> {
        os_actions::bluez::connect_address(address)
    }
}

/// Where one move stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// The destination is checking it holds the accessory's bond.
    Inspecting,
    /// The origin waits for the destination's answer.
    AwaitingReady,
    /// The destination knows it holds the bond and waits to be told to take the accessory.
    Ready,
    /// The origin is letting go of the accessory.
    Disconnecting,
    /// The origin let go and waits for the destination's outcome.
    AwaitingFinish,
    /// The destination is connecting the accessory.
    Connecting,
    /// The destination connected it and waits for the origin to confirm.
    AwaitingAcceptance,
}

/// Why an accessory stayed where it was.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum Stayed {
    /// No accessory has that address.
    Unknown,
    /// The device it was to move to is not one of the person's.
    TargetUnknown,
    /// The device it was to move to is not here.
    TargetAway,
    /// It is not connected here.
    NotConnected,
    /// This device did not let go of it.
    NotReleased,
    /// The destination holds no bond with it.
    NotPairedThere { device: String },
    /// The destination did not connect it.
    NotConnectedThere { device: String },
    /// The destination gave the move up.
    Cancelled { device: String },
    /// The move took too long.
    TimedOut,
}

/// What the person is told of a move.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "notice")]
pub enum Notice {
    /// The accessory is moving to `device`.
    Moving { accessory: String, device: String },
    /// The accessory moved to `device`.
    Moved { accessory: String, device: String },
    /// The accessory came here from `device`.
    Taken { accessory: String, device: String },
    /// The accessory stayed where it was.
    Stayed { accessory: String, why: Stayed },
}

/// One move, as this device knows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Move {
    pub peer: DeviceId,
    pub address: String,
    pub name: String,
    /// Whether this device started the move.
    pub outbound: bool,
    pub phase: Phase,
    #[serde(skip, default = "Instant::now")]
    started: Instant,
}

impl Move {
    /// Whether this is the move `peer` made about `address`, on the `outbound` side, in `phase`.
    fn is(&self, peer: &DeviceId, address: &str, outbound: bool, phase: Phase) -> bool {
        &self.peer == peer && self.is_side(address, outbound, phase)
    }

    /// Whether this is the move about `address`, on the `outbound` side, in `phase`.
    fn is_side(&self, address: &str, outbound: bool, phase: Phase) -> bool {
        self.outbound == outbound
            && self.address.eq_ignore_ascii_case(address)
            && self.phase == phase
    }
}

/// `accessories#move`: move the accessory at `address` to the person's device `device`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MoveTo {
    pub address: String,
    pub device: String,
}

/// What two devices tell each other about one move.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Step {
    pub request_id: [u8; 16],
    pub address: String,
    /// The accessory's name, on `handoff#prepare`.
    #[serde(default)]
    pub name: Option<String>,
    /// On `handoff#prepared`: whether the destination holds the bond. On `handoff#finished`:
    /// whether it connected the accessory.
    #[serde(default)]
    pub ok: bool,
}

impl Step {
    fn new(request_id: [u8; 16], address: &str) -> Self {
        Self {
            request_id,
            address: address.to_owned(),
            name: None,
            ok: false,
        }
    }

    fn ok(mut self, ok: bool) -> Self {
        self.ok = ok;
        self
    }
}

/// Moving accessories between the person's devices, on this one. Cloning shares it.
#[derive(Clone)]
pub struct Accessories {
    fabric: Fabric,
    bluetooth: Arc<dyn Bluetooth>,
    moves: Arc<Mutex<BTreeMap<[u8; 16], Move>>>,
}

impl Accessories {
    #[must_use]
    pub fn new(fabric: Fabric, bluetooth: Arc<dyn Bluetooth>) -> Self {
        Self {
            fabric,
            bluetooth,
            moves: Arc::default(),
        }
    }

    /// The moves under way.
    #[must_use]
    pub fn moves(&self) -> Vec<Move> {
        self.moves.lock_recover().values().cloned().collect()
    }

    fn tell(&self, notice: &Notice) {
        spend(
            self.fabric.seat().call(
                "notices",
                "tell",
                serde_json::to_value(notice).unwrap_or_default(),
            ),
            "how an accessory's move went",
        );
    }

    fn refused(&self, accessory: &str, why: Stayed) {
        self.tell(&Notice::Stayed {
            accessory: accessory.to_owned(),
            why,
        });
    }

    /// Tell `peer` the `step` of a move. `None` when the link refused the call at once: the device
    /// is not reachable. A call still under way counts as sent; one refused later is only logged,
    /// and the move's wait gives it up.
    fn send(&self, peer: &DeviceId, action: &'static str, step: Step) -> Option<()> {
        let mut call = self.fabric.call(peer, PROVIDER, "handoff", action, step);
        match call.poll_ready() {
            PendingPoll::Ready(Ok(_)) => Some(()),
            PendingPoll::Ready(Err(error)) => {
                tracing::debug!(%error, action, "an accessory's move did not reach the other device");
                None
            }
            PendingPoll::Abandoned => None,
            PendingPoll::Waiting => {
                spend(call, "an accessory's move");
                Some(())
            }
        }
    }

    fn work(&self, work: impl FnOnce(&Self) + Send + 'static) {
        let me = self.clone();
        self.bluetooth.run(Box::new(move || work(&me)));
    }

    fn update(&self, request_id: &[u8; 16], phase: Phase) {
        if let Some(held) = self.moves.lock_recover().get_mut(request_id) {
            held.phase = phase;
        }
    }

    fn forget(&self, request_id: &[u8; 16]) -> Option<Move> {
        self.moves.lock_recover().remove(request_id)
    }

    fn held(&self, request_id: &[u8; 16]) -> Option<Move> {
        self.moves.lock_recover().get(request_id).cloned()
    }

    /// Move the accessory at `address` to the person's device `device`: the first step, which
    /// answers the person when it cannot.
    fn begin(&self, address: &str, device: &str) -> Result<(), ActionCallError> {
        let stay = |why| {
            self.refused(address, why);
            Ok(())
        };
        if !os_actions::bluez::valid_address(address) {
            return stay(Stayed::Unknown);
        }
        let Ok(target) = DeviceId::new(device) else {
            return stay(Stayed::TargetUnknown);
        };
        if &target == self.fabric.local() {
            return Ok(());
        }
        if !self.fabric.devices().contains(&target) {
            return stay(Stayed::TargetUnknown);
        }
        if !self.fabric.online().contains(&target) {
            return stay(Stayed::TargetAway);
        }
        if self.moves.lock_recover().len() >= MOST {
            return Ok(());
        }
        let address = address.to_ascii_uppercase();
        self.work(move |me| {
            let name = me.bluetooth.connected_name(&address);
            me.begun(&target, &address, name);
        });
        Ok(())
    }

    /// This device knows what the accessory is called: ask the destination to take it.
    fn begun(&self, target: &DeviceId, address: &str, name: Result<String, String>) {
        let name = match name {
            Ok(name) => name,
            Err(error) => {
                tracing::warn!(%error, "a Bluetooth accessory did not begin its move");
                self.refused(address, Stayed::NotConnected);
                return;
            }
        };
        let request_id = nonzero::<16>();
        self.moves.lock_recover().insert(
            request_id,
            Move {
                peer: target.clone(),
                address: address.to_owned(),
                name: name.clone(),
                outbound: true,
                phase: Phase::AwaitingReady,
                started: Instant::now(),
            },
        );
        self.tell(&Notice::Moving {
            accessory: name.clone(),
            device: self.fabric.label_of(target),
        });
        let mut step = Step::new(request_id, address);
        step.name = Some(name.clone());
        if self.send(target, "prepare", step).is_none() && self.forget(&request_id).is_some() {
            tracing::warn!("a Bluetooth handoff was not asked of the destination");
            self.refused(&name, Stayed::TargetAway);
        }
    }

    /// The person's other device asked this one to take an accessory: check whether this device
    /// holds its bond, and answer.
    fn prepare(&self, from: &DeviceId, step: Step) -> Result<(), ActionCallError> {
        let name = step.name.clone().unwrap_or_default();
        if !os_actions::bluez::valid_address(&step.address) || name.is_empty() || name.len() > 128 {
            return Err(ActionCallError::InvalidRequest {
                reason: "not an accessory".to_owned(),
            });
        }
        let accepted = {
            let mut moves = self.moves.lock_recover();
            if moves.len() >= MOST || moves.contains_key(&step.request_id) {
                false
            } else {
                moves.insert(
                    step.request_id,
                    Move {
                        peer: from.clone(),
                        address: step.address.to_ascii_uppercase(),
                        name,
                        outbound: false,
                        phase: Phase::Inspecting,
                        started: Instant::now(),
                    },
                );
                true
            }
        };
        if accepted {
            let request_id = step.request_id;
            let address = step.address;
            self.work(move |me| {
                let ready = me.bluetooth.paired(&address);
                me.inspected(request_id, &address, ready);
            });
        } else {
            let _ = self.send(
                from,
                "prepared",
                Step::new(step.request_id, &step.address).ok(false),
            );
        }
        Ok(())
    }

    /// This device knows whether it holds the bond: tell the origin.
    fn inspected(&self, request_id: [u8; 16], address: &str, ready: bool) {
        let Some(held) = self.held(&request_id) else {
            return;
        };
        if held.outbound || held.phase != Phase::Inspecting {
            return;
        }
        if ready {
            self.update(&request_id, Phase::Ready);
        } else {
            self.forget(&request_id);
        }
        if self
            .send(
                &held.peer,
                "prepared",
                Step::new(request_id, address).ok(ready),
            )
            .is_none()
            && ready
        {
            self.forget(&request_id);
        }
    }

    /// The destination answered: let go of the accessory here, or stop.
    fn prepared(&self, from: &DeviceId, step: &Step) {
        let Some(held) = self.held(&step.request_id) else {
            return;
        };
        if !held.is(from, &step.address, true, Phase::AwaitingReady) {
            return;
        }
        if !step.ok {
            self.forget(&step.request_id);
            self.refused(
                &held.name,
                Stayed::NotPairedThere {
                    device: self.fabric.label_of(&held.peer),
                },
            );
            return;
        }
        self.update(&step.request_id, Phase::Disconnecting);
        let request_id = step.request_id;
        let address = step.address.clone();
        self.work(move |me| {
            let result = me.bluetooth.disconnect(&address);
            me.released(request_id, &address, &result);
        });
    }

    /// The origin let go of the accessory: tell the destination to take it, or, when it could not
    /// let go, stop and say so.
    fn released(&self, request_id: [u8; 16], address: &str, result: &Result<(), String>) {
        let held = self
            .held(&request_id)
            .filter(|held| held.is_side(address, true, Phase::Disconnecting));
        let Some(held) = held else {
            // It let go after the move stopped: take the accessory back.
            if result.is_ok() {
                self.give_back(address);
            }
            return;
        };
        if let Err(error) = result {
            tracing::warn!(%error, "a Bluetooth accessory was not released");
            self.forget(&request_id);
            self.refused(&held.name, Stayed::NotReleased);
            return;
        }
        self.update(&request_id, Phase::AwaitingFinish);
        if self
            .send(&held.peer, "commit", Step::new(request_id, address))
            .is_none()
            && self.forget(&request_id).is_some()
        {
            self.give_back(address);
            self.refused(&held.name, Stayed::TargetAway);
        }
    }

    /// The origin let go: connect the accessory here.
    fn committed(&self, from: &DeviceId, step: &Step) {
        let accepts = self
            .held(&step.request_id)
            .is_some_and(|held| held.is(from, &step.address, false, Phase::Ready));
        if !accepts {
            let _ = self.send(
                from,
                "finished",
                Step::new(step.request_id, &step.address).ok(false),
            );
            return;
        }
        self.update(&step.request_id, Phase::Connecting);
        let request_id = step.request_id;
        let address = step.address.clone();
        self.work(move |me| {
            let result = me.bluetooth.connect(&address);
            me.taken(request_id, &address, &result);
        });
    }

    /// The destination took the accessory, or could not: tell the origin either way.
    fn taken(&self, request_id: [u8; 16], address: &str, result: &Result<(), String>) {
        let held = self
            .held(&request_id)
            .filter(|held| held.is_side(address, false, Phase::Connecting));
        let Some(held) = held else {
            // It connected after the move stopped: let go again.
            if result.is_ok() {
                self.release(address);
            }
            return;
        };
        let connected = result.is_ok();
        if let Err(error) = result {
            tracing::warn!(%error, "a Bluetooth accessory did not connect on the destination");
        }
        if connected {
            self.update(&request_id, Phase::AwaitingAcceptance);
        } else {
            self.forget(&request_id);
        }
        if self
            .send(
                &held.peer,
                "finished",
                Step::new(request_id, address).ok(connected),
            )
            .is_none()
            && self.forget(&request_id).is_some()
            && connected
        {
            self.release(address);
        }
    }

    /// The destination finished: confirm it took the accessory, or take the accessory back here.
    fn finished(&self, from: &DeviceId, step: &Step) {
        let Some(held) = self.held(&step.request_id) else {
            return;
        };
        if !held.is(from, &step.address, true, Phase::AwaitingFinish) {
            return;
        }
        let device = self.fabric.label_of(&held.peer);
        if !step.ok {
            self.forget(&step.request_id);
            self.give_back(&step.address);
            self.refused(&held.name, Stayed::NotConnectedThere { device });
            return;
        }
        self.forget(&step.request_id);
        let confirmed = self
            .send(
                &held.peer,
                "accepted",
                Step::new(step.request_id, &step.address),
            )
            .is_some();
        if confirmed {
            self.tell(&Notice::Moved {
                accessory: held.name,
                device,
            });
        } else {
            self.give_back(&step.address);
            self.refused(&held.name, Stayed::TargetAway);
        }
    }

    /// The origin confirmed the move: the accessory is this device's now.
    fn accepted(&self, from: &DeviceId, step: &Step) {
        let Some(held) = self.held(&step.request_id) else {
            return;
        };
        if !held.is(from, &step.address, false, Phase::AwaitingAcceptance) {
            return;
        }
        self.forget(&step.request_id);
        self.tell(&Notice::Taken {
            accessory: held.name,
            device: self.fabric.label_of(from),
        });
    }

    /// The other device gave the move up: give it up here too, and put the accessory back where it
    /// was.
    fn cancelled(&self, from: &DeviceId, step: &Step) {
        let Some(held) = self.held(&step.request_id) else {
            return;
        };
        if &held.peer != from || !held.address.eq_ignore_ascii_case(&step.address) {
            return;
        }
        self.forget(&step.request_id);
        self.put_back(&held);
        if held.outbound {
            self.refused(
                &held.name,
                Stayed::Cancelled {
                    device: self.fabric.label_of(from),
                },
            );
        }
    }

    /// Give up on moves whose destination left or that took too long, and put what they let go of
    /// back. The host calls this about once a second.
    pub fn tick(&self, now: Instant) {
        let online = self.fabric.online();
        let stopped: Vec<([u8; 16], Move, bool)> = self
            .moves
            .lock_recover()
            .iter()
            .filter_map(|(request_id, held)| {
                let away = !online.contains(&held.peer);
                let late = now.saturating_duration_since(held.started) >= WAIT;
                (away || late).then(|| (*request_id, held.clone(), away))
            })
            .collect();
        for (request_id, held, away) in stopped {
            self.forget(&request_id);
            self.put_back(&held);
            let _ = self.send(&held.peer, "cancel", Step::new(request_id, &held.address));
            if held.outbound {
                self.refused(
                    &held.name,
                    if away {
                        Stayed::TargetAway
                    } else {
                        Stayed::TimedOut
                    },
                );
            }
        }
    }

    /// Put back what `held` let go of, where it let go of anything.
    fn put_back(&self, held: &Move) {
        match (held.outbound, held.phase) {
            (true, Phase::Disconnecting | Phase::AwaitingFinish) => self.give_back(&held.address),
            (false, Phase::Connecting | Phase::AwaitingAcceptance) => self.release(&held.address),
            _ => {}
        }
    }

    /// Connect the accessory here again, because the move stopped after this device let go of it.
    fn give_back(&self, address: &str) {
        let address = address.to_owned();
        self.work(move |me| {
            if let Err(error) = me.bluetooth.connect(&address) {
                tracing::error!(%error, "a Bluetooth accessory was not taken back");
            }
        });
    }

    /// Let go of the accessory here again, because the move stopped after this device took it.
    fn release(&self, address: &str) {
        let address = address.to_owned();
        self.work(move |me| {
            if let Err(error) = me.bluetooth.disconnect(&address) {
                tracing::warn!(%error, "a Bluetooth accessory was not let go again");
            }
        });
    }

    /// The accessories' resources and actions, as the device's action host mounts them under
    /// [`PROVIDER`].
    #[must_use]
    pub fn provider(&self) -> Arc<dyn ActionProvider + Send + Sync> {
        let at = || self.clone();
        let step =
            |action: &'static str, label: &'static str, then: fn(&Self, &DeviceId, &Step)| {
                let accessories = at();
                (
                    action,
                    label,
                    move |who: &tilo_fabric::actions::Who, step: Step| {
                        then(&accessories, asking_device(who)?, &step);
                        Ok(())
                    },
                )
            };
        let mut provider =
            Provider::new(PROVIDER, self.fabric.local().clone(), *self.fabric.person())
                .read(
                    "moves",
                    "Accessories moving between the person's devices",
                    {
                        let accessories = at();
                        move || accessories.moves()
                    },
                )
                .act(
                    "accessories",
                    "move",
                    "Move a Bluetooth accessory to another of the person's devices",
                    Asker::Here,
                    {
                        let accessories = at();
                        move |_, asked: MoveTo| accessories.begin(&asked.address, &asked.device)
                    },
                )
                .act(
                    "handoff",
                    "prepare",
                    "Another device asks this one to take an accessory",
                    Asker::Device,
                    {
                        let accessories = at();
                        move |who, step: Step| accessories.prepare(asking_device(who)?, step)
                    },
                );
        for (action, label, act) in [
            step(
                "prepared",
                "Whether the destination holds the bond",
                Self::prepared,
            ),
            step(
                "commit",
                "The origin let go of the accessory",
                Self::committed,
            ),
            step(
                "finished",
                "Whether the destination connected it",
                Self::finished,
            ),
            step("accepted", "The origin confirmed the move", Self::accepted),
            step(
                "cancel",
                "The other device gave the move up",
                Self::cancelled,
            ),
        ] {
            provider = provider.act("handoff", action, label, Asker::Device, act);
        }
        provider.shared()
    }
}
