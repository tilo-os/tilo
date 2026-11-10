//! Moving one of the person's Bluetooth accessories to another of their devices.
//! A bond belongs to the accessory, not to a device: moving it means the device that has it lets
//! go, and the device it moves to connects it with the bond it already holds. Nothing is copied.
//! The two sessions agree over the device fabric — prepare, commit, finished, accepted — and each
//! does its own Bluetooth work on a worker of its own, so no pass of either session ever waits on
//! `BlueZ` .
//! Only an accessory the destination already holds a bond with moves this way: a destination that
//! has none says so, and the accessory stays where it is rather than asking the person to pair it
//! again.
//! A move that the two devices do not finish — the destination leaves, a worked step fails, or
//! [`WAIT`] runs out — puts the accessory back on the device it came from, so a failed move never
//! leaves it connected to neither.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::platform::fabric::SharedLink;
use crate::platform::fabric::{DeviceId, PersonalFabricAction, PersonalFabricActionEnvelope};

use crate::Shared;
use crate::i18n::{t, t_args};
use crate::notices::Notice;

/// How long one move may take before both devices give it up and the accessory goes back where it
/// was.
const WAIT: Duration = Duration::from_secs(75);

/// How many moves one session may have under way at once.
const MOST: usize = 32;

/// Where one move stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
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

/// One move, as this device knows it.
#[derive(Clone, Debug)]
struct Transaction {
    peer: DeviceId,
    address: String,
    name: String,
    /// Whether this device started the move.
    outbound: bool,
    phase: Phase,
    started: Instant,
}

impl Transaction {
    /// Whether this is the move `peer` made about `address` , on the `outbound` side, in `phase` .
    fn is(&self, peer: &DeviceId, address: &str, outbound: bool, phase: Phase) -> bool {
        &self.peer == peer && self.is_side(address, outbound, phase)
    }

    /// Whether this is the move about `address` , on the `outbound` side, in `phase` .
    fn is_side(&self, address: &str, outbound: bool, phase: Phase) -> bool {
        self.outbound == outbound
            && self.address.eq_ignore_ascii_case(address)
            && self.phase == phase
    }
}

/// The Bluetooth work one worker does, away from the session's pass.
enum Operation {
    /// On the origin: is the accessory connected here, what is it called,
    /// and does the destination exist.
    Begin { target: DeviceId },
    /// On the destination: is the accessory already paired here.
    Inspect,
    /// Let go of the accessory here, keeping its bond.
    Release,
    /// Connect the accessory here, with the bond this device already holds.
    Take,
    /// Connect the accessory again here, because the move stopped after this device let go of it.
    GiveBack,
}

/// What a worker was asked to do.
struct Work {
    /// The move it belongs to, where one was made already.
    request_id: Option<[u8; 16]>,
    address: String,
    operation: Operation,
}

/// What a worker found.
enum Outcome {
    Begun {
        target: DeviceId,
        name: Result<String, String>,
    },
    Inspected(bool),
    Released(Result<(), String>),
    Taken(Result<(), String>),
    GivenBack(Result<(), String>),
}

/// What a worker finished.
struct Done {
    request_id: Option<[u8; 16]>,
    address: String,
    outcome: Outcome,
}

/// The moves this session takes part in: what the panel asked of it, what the person's other
/// devices said, and the workers' outcomes, which the session's pass takes in.
pub(crate) struct Handoffs {
    transactions: RefCell<BTreeMap<[u8; 16], Transaction>>,
    /// What the panel asked for, waiting for the pass that holds the fabric.
    asked: RefCell<Vec<(String, String)>>,
    sender: mpsc::Sender<Done>,
    results: RefCell<mpsc::Receiver<Done>>,
}

impl Default for Handoffs {
    fn default() -> Self {
        let (sender, results) = mpsc::channel();
        Self {
            transactions: RefCell::default(),
            asked: RefCell::default(),
            sender,
            results: RefCell::new(results),
        }
    }
}

impl Handoffs {
    /// Move the accessory at `address` to the person's device `device` .
    pub(crate) fn ask(&self, address: &str, device: &str) {
        self.asked
            .borrow_mut()
            .push((address.to_owned(), device.to_owned()));
    }

    /// Take one action from one of the person's devices, and answer whether it was this one.
    pub(crate) fn arrived(
        &self,
        shared: &Shared,
        fabric: &SharedLink,
        envelope: &PersonalFabricActionEnvelope,
    ) -> bool {
        match &envelope.action {
            PersonalFabricAction::BluetoothHandoffPrepare { address, name } => {
                self.prepare(fabric, envelope, address, name);
            }
            PersonalFabricAction::BluetoothHandoffPrepared {
                request_id,
                address,
                ready,
            } => self.prepared(shared, fabric, envelope, request_id, address, *ready),
            PersonalFabricAction::BluetoothHandoffCommit {
                request_id,
                address,
            } => self.committed(fabric, envelope, request_id, address),
            PersonalFabricAction::BluetoothHandoffFinished {
                request_id,
                address,
                connected,
            } => self.finished(shared, fabric, envelope, request_id, address, *connected),
            PersonalFabricAction::BluetoothHandoffAccepted {
                request_id,
                address,
            } => self.accepted(shared, fabric, envelope, request_id, address),
            PersonalFabricAction::BluetoothHandoffCancel {
                request_id,
                address,
            } => self.cancelled(shared, fabric, envelope, request_id, address),
            // A destination that wants the accessory paired with this approval has none to give
            // here: this session asks the person for no pairing, so nothing ever offers one and an
            // offer that arrives anyway is dropped.
            PersonalFabricAction::BluetoothHandoffPairingOffered { .. }
            | PersonalFabricAction::BluetoothHandoffPairingCommit { .. } => {}
            _ => return false,
        }
        true
    }

    /// Do what the workers and the person's other devices left for this pass: what the panel asked
    /// for, what the workers found, and the moves that stop here because the destination left or
    /// took too long.
    pub(crate) fn pump(&self, shared: &Shared, fabric: &SharedLink) {
        let asked = std::mem::take(&mut *self.asked.borrow_mut());
        for (address, device) in asked {
            self.begin(shared, fabric, &address, &device);
        }
        let done: Vec<Done> = self.results.borrow().try_iter().collect();
        for done in done {
            self.done(shared, fabric, done);
        }
        self.follow(shared, fabric);
    }

    /// Ask the destination whether it can take the accessory the person named: the first step,
    /// which answers the person when it cannot.
    fn begin(&self, shared: &Shared, fabric: &SharedLink, address: &str, device: &str) {
        if !os_actions::bluez::valid_address(address) {
            refused(shared, address, &t("session-bluetooth-unknown"));
            return;
        }
        let Ok(target) = DeviceId::new(device) else {
            refused(shared, address, &t("session-bluetooth-target-unknown"));
            return;
        };
        let Some((local, _)) = fabric.local_device_fabric_identity() else {
            refused(shared, address, &t("session-bluetooth-no-identity"));
            return;
        };
        if target == local {
            return;
        }
        let converged = fabric.device_fabric();
        let Some(destination) = converged.snapshots().get(&target) else {
            refused(shared, address, &t("session-bluetooth-target-unknown"));
            return;
        };
        if !destination.presence.online {
            refused(shared, address, &t("session-bluetooth-target-away"));
            return;
        }
        if self.transactions.borrow().len() >= MOST {
            return;
        }
        self.spawn(Work {
            request_id: None,
            address: address.to_owned(),
            operation: Operation::Begin { target },
        });
    }

    /// Take in what a worker finished.
    fn done(&self, shared: &Shared, fabric: &SharedLink, done: Done) {
        match done.outcome {
            Outcome::Begun { target, name } => {
                let name = match name {
                    Ok(name) => name,
                    Err(error) => {
                        tracing::warn!(%error, "a Bluetooth accessory did not begin its move");
                        refused(shared, &done.address, &t("session-bluetooth-not-connected"));
                        return;
                    }
                };
                let action = PersonalFabricAction::BluetoothHandoffPrepare {
                    address: done.address.to_ascii_uppercase(),
                    name: name.clone(),
                };
                let Ok(request_id) = fabric.try_send_personal_fabric_action(&target, action) else {
                    tracing::warn!("a Bluetooth handoff was not asked of the destination");
                    refused(shared, &name, &t("session-bluetooth-target-away"));
                    return;
                };
                self.transactions.borrow_mut().insert(
                    request_id,
                    Transaction {
                        peer: target.clone(),
                        address: done.address.to_ascii_uppercase(),
                        name: name.clone(),
                        outbound: true,
                        phase: Phase::AwaitingReady,
                        started: Instant::now(),
                    },
                );
                let device = label(fabric, &target);
                tell(
                    shared,
                    0,
                    t_args(
                        "session-bluetooth-moving",
                        &[("accessory", name.as_str().into())],
                    ),
                    t_args(
                        "session-bluetooth-moving-body",
                        &[("device", device.as_str().into())],
                    ),
                );
            }
            Outcome::Inspected(ready) => {
                let Some(request_id) = done.request_id else {
                    return;
                };
                let Some(transaction) = self.transactions.borrow().get(&request_id).cloned() else {
                    return;
                };
                if transaction.outbound || transaction.phase != Phase::Inspecting {
                    return;
                }
                if ready {
                    if let Some(held) = self.transactions.borrow_mut().get_mut(&request_id) {
                        held.phase = Phase::Ready;
                    }
                } else {
                    self.transactions.borrow_mut().remove(&request_id);
                }
                let action = PersonalFabricAction::BluetoothHandoffPrepared {
                    request_id,
                    address: done.address.clone(),
                    ready,
                };
                if fabric
                    .try_send_personal_fabric_action(&transaction.peer, action)
                    .is_err()
                    && ready
                {
                    self.transactions.borrow_mut().remove(&request_id);
                }
            }
            Outcome::Released(result) => {
                self.released(shared, fabric, done.request_id, &done.address, &result);
            }
            Outcome::Taken(result) => self.taken(fabric, done.request_id, &done.address, &result),
            Outcome::GivenBack(result) => {
                if let Err(error) = result {
                    tracing::error!(%error, "a Bluetooth accessory was not taken back");
                }
            }
        }
    }

    /// The origin let go of the accessory: tell the destination to take it,
    /// or, when it could not let go, stop and say so.
    fn released(
        &self,
        shared: &Shared,
        fabric: &SharedLink,
        request_id: Option<[u8; 16]>,
        address: &str,
        result: &Result<(), String>,
    ) {
        let Some(request_id) = request_id else {
            return;
        };
        let Some(transaction) = self.transactions.borrow().get(&request_id).cloned() else {
            // It let go after the move stopped: take the accessory back.
            if result.is_ok() {
                self.spawn(give_back(request_id, address));
            }
            return;
        };
        if !transaction.is_side(address, true, Phase::Disconnecting) {
            if result.is_ok() {
                self.spawn(give_back(request_id, address));
            }
            return;
        }
        if let Err(error) = result {
            tracing::warn!(%error, "a Bluetooth accessory was not released");
            self.transactions.borrow_mut().remove(&request_id);
            refused(
                shared,
                &transaction.name,
                &t("session-bluetooth-not-released"),
            );
            return;
        }
        if let Some(held) = self.transactions.borrow_mut().get_mut(&request_id) {
            held.phase = Phase::AwaitingFinish;
        }
        let action = PersonalFabricAction::BluetoothHandoffCommit {
            request_id,
            address: address.to_owned(),
        };
        if fabric
            .try_send_personal_fabric_action(&transaction.peer, action)
            .is_err()
        {
            self.transactions.borrow_mut().remove(&request_id);
            self.spawn(give_back(request_id, address));
            refused(
                shared,
                &transaction.name,
                &t("session-bluetooth-target-away"),
            );
        }
    }

    /// The destination took the accessory, or could not: tell the origin either way.
    fn taken(
        &self,
        fabric: &SharedLink,
        request_id: Option<[u8; 16]>,
        address: &str,
        result: &Result<(), String>,
    ) {
        let Some(request_id) = request_id else {
            return;
        };
        let Some(transaction) = self.transactions.borrow().get(&request_id).cloned() else {
            // It connected after the move stopped: let go again.
            if result.is_ok() {
                self.spawn(release(request_id, address));
            }
            return;
        };
        if !transaction.is_side(address, false, Phase::Connecting) {
            if result.is_ok() {
                self.spawn(release(request_id, address));
            }
            return;
        }
        let connected = result.is_ok();
        if let Err(error) = &result {
            tracing::warn!(%error, "a Bluetooth accessory did not connect on the destination");
        }
        if connected {
            if let Some(held) = self.transactions.borrow_mut().get_mut(&request_id) {
                held.phase = Phase::AwaitingAcceptance;
            }
        } else {
            self.transactions.borrow_mut().remove(&request_id);
        }
        let action = PersonalFabricAction::BluetoothHandoffFinished {
            request_id,
            address: address.to_owned(),
            connected,
        };
        if fabric
            .try_send_personal_fabric_action(&transaction.peer, action)
            .is_err()
        {
            self.transactions.borrow_mut().remove(&request_id);
            if connected {
                self.spawn(release(request_id, address));
            }
        }
    }

    /// The person's other device asked this one to take an accessory: check whether this device
    /// holds its bond, and answer.
    fn prepare(
        &self,
        fabric: &SharedLink,
        envelope: &PersonalFabricActionEnvelope,
        address: &str,
        name: &str,
    ) {
        let request_id = envelope.request_id;
        let accepted = {
            let mut transactions = self.transactions.borrow_mut();
            if transactions.len() >= MOST || transactions.contains_key(&request_id) {
                false
            } else {
                transactions.insert(
                    request_id,
                    Transaction {
                        peer: envelope.source.clone(),
                        address: address.to_ascii_uppercase(),
                        name: name.to_owned(),
                        outbound: false,
                        phase: Phase::Inspecting,
                        started: Instant::now(),
                    },
                );
                true
            }
        };
        if accepted {
            self.spawn(Work {
                request_id: Some(request_id),
                address: address.to_owned(),
                operation: Operation::Inspect,
            });
        } else {
            let _ = fabric.try_send_personal_fabric_action(
                &envelope.source,
                PersonalFabricAction::BluetoothHandoffPrepared {
                    request_id,
                    address: address.to_owned(),
                    ready: false,
                },
            );
        }
    }

    /// The destination answered: let go of the accessory here, or stop.
    fn prepared(
        &self,
        shared: &Shared,
        fabric: &SharedLink,
        envelope: &PersonalFabricActionEnvelope,
        request_id: &[u8; 16],
        address: &str,
        ready: bool,
    ) {
        let Some(transaction) = self.transactions.borrow().get(request_id).cloned() else {
            return;
        };
        if !transaction.is(&envelope.source, address, true, Phase::AwaitingReady) {
            return;
        }
        if !ready {
            self.transactions.borrow_mut().remove(request_id);
            let device = label(fabric, &transaction.peer);
            refused(
                shared,
                &transaction.name,
                &t_args(
                    "session-bluetooth-not-paired-there",
                    &[("device", device.as_str().into())],
                ),
            );
            return;
        }
        if let Some(held) = self.transactions.borrow_mut().get_mut(request_id) {
            held.phase = Phase::Disconnecting;
        }
        self.spawn(Work {
            request_id: Some(*request_id),
            address: address.to_owned(),
            operation: Operation::Release,
        });
    }

    /// The origin let go: connect the accessory here.
    fn committed(
        &self,
        fabric: &SharedLink,
        envelope: &PersonalFabricActionEnvelope,
        request_id: &[u8; 16],
        address: &str,
    ) {
        let accepts = self
            .transactions
            .borrow()
            .get(request_id)
            .is_some_and(|transaction| {
                transaction.is(&envelope.source, address, false, Phase::Ready)
            });
        if !accepts {
            let _ = fabric.try_send_personal_fabric_action(
                &envelope.source,
                PersonalFabricAction::BluetoothHandoffFinished {
                    request_id: *request_id,
                    address: address.to_owned(),
                    connected: false,
                },
            );
            return;
        }
        if let Some(held) = self.transactions.borrow_mut().get_mut(request_id) {
            held.phase = Phase::Connecting;
        }
        self.spawn(Work {
            request_id: Some(*request_id),
            address: address.to_owned(),
            operation: Operation::Take,
        });
    }

    /// The destination finished: confirm it took the accessory, or take the accessory back here.
    fn finished(
        &self,
        shared: &Shared,
        fabric: &SharedLink,
        envelope: &PersonalFabricActionEnvelope,
        request_id: &[u8; 16],
        address: &str,
        connected: bool,
    ) {
        let Some(transaction) = self.transactions.borrow().get(request_id).cloned() else {
            return;
        };
        if !transaction.is(&envelope.source, address, true, Phase::AwaitingFinish) {
            return;
        }
        if !connected {
            self.transactions.borrow_mut().remove(request_id);
            self.spawn(give_back(*request_id, address));
            let device = label(fabric, &transaction.peer);
            refused(
                shared,
                &transaction.name,
                &t_args(
                    "session-bluetooth-not-connected-there",
                    &[("device", device.as_str().into())],
                ),
            );
            return;
        }
        let confirmed = fabric
            .try_send_personal_fabric_action(
                &transaction.peer,
                PersonalFabricAction::BluetoothHandoffAccepted {
                    request_id: *request_id,
                    address: address.to_owned(),
                },
            )
            .is_ok();
        self.transactions.borrow_mut().remove(request_id);
        if confirmed {
            let device = label(fabric, &transaction.peer);
            tell(
                shared,
                0,
                t_args(
                    "session-bluetooth-moved",
                    &[("accessory", transaction.name.as_str().into())],
                ),
                t_args(
                    "session-bluetooth-moved-body",
                    &[("device", device.as_str().into())],
                ),
            );
        } else {
            self.spawn(give_back(*request_id, address));
            refused(
                shared,
                &transaction.name,
                &t("session-bluetooth-target-away"),
            );
        }
    }

    /// The origin confirmed the move: the accessory is this device's now.
    fn accepted(
        &self,
        shared: &Shared,
        fabric: &SharedLink,
        envelope: &PersonalFabricActionEnvelope,
        request_id: &[u8; 16],
        address: &str,
    ) {
        let Some(transaction) = self.transactions.borrow().get(request_id).cloned() else {
            return;
        };
        if !transaction.is(&envelope.source, address, false, Phase::AwaitingAcceptance) {
            return;
        }
        self.transactions.borrow_mut().remove(request_id);
        let device = label(fabric, &transaction.peer);
        tell(
            shared,
            0,
            t_args(
                "session-bluetooth-taken",
                &[("accessory", transaction.name.as_str().into())],
            ),
            t_args(
                "session-bluetooth-taken-body",
                &[("device", device.as_str().into())],
            ),
        );
    }

    /// The other device gave the move up: give it up here too, and put the accessory back where it
    /// was.
    fn cancelled(
        &self,
        shared: &Shared,
        fabric: &SharedLink,
        envelope: &PersonalFabricActionEnvelope,
        request_id: &[u8; 16],
        address: &str,
    ) {
        let Some(transaction) = self.transactions.borrow().get(request_id).cloned() else {
            return;
        };
        if transaction.peer != envelope.source || !transaction.address.eq_ignore_ascii_case(address)
        {
            return;
        }
        self.transactions.borrow_mut().remove(request_id);
        self.put_back(*request_id, &transaction);
        if transaction.outbound {
            refused(
                shared,
                &transaction.name,
                &t_args(
                    "session-bluetooth-cancelled",
                    &[("device", label(fabric, &envelope.source).as_str().into())],
                ),
            );
        }
    }

    /// Give up on moves whose destination left the fabric or that took too long, and put what they
    /// let go of back.
    fn follow(&self, shared: &Shared, fabric: &SharedLink) {
        let online = fabric.online_device_ids();
        let now = Instant::now();
        let stopped: Vec<([u8; 16], Transaction, bool)> = self
            .transactions
            .borrow()
            .iter()
            .filter_map(|(request_id, transaction)| {
                let away = !online.contains(&transaction.peer);
                let late = now.saturating_duration_since(transaction.started) >= WAIT;
                (away || late).then(|| (*request_id, transaction.clone(), away))
            })
            .collect();
        for (request_id, transaction, away) in stopped {
            self.transactions.borrow_mut().remove(&request_id);
            self.put_back(request_id, &transaction);
            let _ = fabric.try_send_personal_fabric_action(
                &transaction.peer,
                PersonalFabricAction::BluetoothHandoffCancel {
                    request_id,
                    address: transaction.address.clone(),
                },
            );
            if transaction.outbound {
                let detail = if away {
                    t("session-bluetooth-target-away")
                } else {
                    t("session-bluetooth-timed-out")
                };
                refused(shared, &transaction.name, &detail);
            }
        }
    }

    /// Put back what `transaction` let go of, where it let go of anything.
    fn put_back(&self, request_id: [u8; 16], transaction: &Transaction) {
        match (transaction.outbound, transaction.phase) {
            (true, Phase::Disconnecting | Phase::AwaitingFinish) => {
                self.spawn(give_back(request_id, &transaction.address));
            }
            (false, Phase::Connecting | Phase::AwaitingAcceptance) => {
                self.spawn(release(request_id, &transaction.address));
            }
            _ => {}
        }
    }

    /// Do `work` on a thread of its own, so the pass never waits on `BlueZ` .
    fn spawn(&self, work: Work) {
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let _ = sender.send(work.run());
        });
    }
}

impl Work {
    /// Do the work, and say what came of it.
    fn run(self) -> Done {
        let address = self.address;
        let request_id = self.request_id;
        let outcome = match self.operation {
            Operation::Begin { target } => Outcome::Begun {
                target,
                name: connected_name(&address),
            },
            Operation::Inspect => Outcome::Inspected(paired(&address)),
            Operation::Release => {
                Outcome::Released(os_actions::bluez::disconnect_address(&address))
            }
            Operation::Take => Outcome::Taken(os_actions::bluez::connect_address(&address)),
            Operation::GiveBack => Outcome::GivenBack(os_actions::bluez::connect_address(&address)),
        };
        Done {
            request_id,
            address,
            outcome,
        }
    }
}

/// The name of the accessory at `address` , while it is connected here.
fn connected_name(address: &str) -> Result<String, String> {
    let accessory = os_actions::bluez::paired_devices()?
        .into_iter()
        .find(|device| device.address.eq_ignore_ascii_case(address) && device.connected)
        .ok_or_else(|| "the accessory is not connected here".to_owned())?;
    Ok(accessory.name)
}

/// Whether this device holds the bond of the accessory at `address` .
fn paired(address: &str) -> bool {
    os_actions::bluez::paired_devices().is_ok_and(|devices| {
        devices
            .iter()
            .any(|device| device.address.eq_ignore_ascii_case(address) && device.paired)
    })
}

fn give_back(request_id: [u8; 16], address: &str) -> Work {
    Work {
        request_id: Some(request_id),
        address: address.to_owned(),
        operation: Operation::GiveBack,
    }
}

fn release(request_id: [u8; 16], address: &str) -> Work {
    Work {
        request_id: Some(request_id),
        address: address.to_owned(),
        operation: Operation::Release,
    }
}

/// What the person's other device is called, as its own session says.
fn label(fabric: &SharedLink, peer: &DeviceId) -> String {
    fabric.device_fabric().snapshots().get(peer).map_or_else(
        || peer.as_str().to_owned(),
        |snapshot| snapshot.presence.label.clone(),
    )
}

/// Tell the person how one move went.
fn tell(shared: &Shared, urgency: u8, title: String, body: String) {
    shared.notifications.borrow().tell(Notice::new(
        t("session-bluetooth-app"),
        urgency,
        title,
        body,
    ));
}

/// Tell the person an accessory stayed where it was, and why.
fn refused(shared: &Shared, accessory: &str, detail: &str) {
    tell(
        shared,
        1,
        t_args(
            "session-bluetooth-stayed",
            &[("accessory", accessory.into())],
        ),
        detail.to_owned(),
    );
}
