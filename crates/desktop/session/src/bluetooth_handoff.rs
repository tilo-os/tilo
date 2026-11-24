//! Moving one of the person's Bluetooth accessories to another of their devices.
//!
//! The move is asked and carried out in the record of the person's devices
//! ([`tilo_workspace_service::workspaces::person_workspace::accessory`]): this session asks the
//! moves the person makes of an accessory connected here, reads its part of each move from the
//! record, does that part with this machine's Bluetooth on a worker of its own, so no pass waits on
//! `BlueZ`, and writes how it went. It tells the person how each move it asked ended, and that an
//! accessory moved here, read from the same record.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::sync::mpsc;

use tilo_workspace_service::workspaces::person_workspace::accessory::{
    self, MoveRefusal, Part, Radio, Turn,
};
use tilo_workspace_service::workspaces::person_workspace::devices::{
    AccessoryMove, Devices, MoveStage,
};

use crate::Shared;
use crate::i18n::{t, t_args};
use crate::platform::fabric::{DeviceId, SharedLink};
use crate::platform::notice::Notice;

/// What a worker found.
enum Done {
    /// Whether the accessory at `address` is connected here, and what it is called, for a move to
    /// `target`.
    Begun {
        address: String,
        target: String,
        name: Result<String, String>,
    },
    /// How one piece of this device's part of a move went.
    Turned {
        turn: Turn,
        worked: Result<(), String>,
    },
}

/// This session's part in the moves of the person's accessories.
pub(crate) struct Handoffs {
    /// When this session started, ns since the epoch: a move asked before it was told by the
    /// session then.
    since_ns: i64,
    /// What the panel asked for, waiting for the pass that holds the fabric.
    asked: RefCell<Vec<(String, String)>>,
    /// The moves' work this device took up.
    part: RefCell<Part>,
    /// The moves it told the person the end of, by accessory and when each was asked.
    told: RefCell<BTreeSet<(String, i64)>>,
    sender: mpsc::Sender<Done>,
    results: RefCell<mpsc::Receiver<Done>>,
}

impl Default for Handoffs {
    fn default() -> Self {
        let (sender, results) = mpsc::channel();
        Self {
            since_ns: now_ns(),
            asked: RefCell::default(),
            part: RefCell::default(),
            told: RefCell::default(),
            sender,
            results: RefCell::new(results),
        }
    }
}

impl Handoffs {
    /// Move the accessory at `address` to the person's device `device`.
    pub(crate) fn ask(&self, address: &str, device: &str) {
        self.asked
            .borrow_mut()
            .push((address.to_owned(), device.to_owned()));
    }

    /// Do what the panel, the workers and the record left for this pass.
    pub(crate) fn pump(&self, shared: &Shared, fabric: &SharedLink) {
        let asked = std::mem::take(&mut *self.asked.borrow_mut());
        for (address, device) in asked {
            self.begin(shared, fabric, &address, &device);
        }
        let Some(devices) = shared.owner_devices.as_ref() else {
            return;
        };
        let (record, local) = devices.record();
        let done: Vec<Done> = self.results.borrow().try_iter().collect();
        for done in done {
            let write = match done {
                Done::Begun {
                    address,
                    target,
                    name,
                } => {
                    let name = match name {
                        Ok(name) => name,
                        Err(error) => {
                            tracing::warn!(%error, "a Bluetooth accessory did not begin its move");
                            refused(shared, &address, &t("session-bluetooth-not-connected"));
                            continue;
                        }
                    };
                    let held = record.get();
                    match accessory::asking(&held, &local, &address, &name, &target, now_ns()) {
                        Ok(ask) => {
                            if let Err(error) = record.invoke(&ask) {
                                tracing::warn!(%error, "a Bluetooth handoff was not asked");
                                refused(shared, &name, &t("session-bluetooth-target-away"));
                                continue;
                            }
                            let device = crate::wake::label(&held, &key(&target));
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
                        Err(MoveRefusal::SameDevice) => {}
                        Err(MoveRefusal::UnknownDevice) => {
                            refused(shared, &name, &t("session-bluetooth-target-unknown"));
                        }
                        Err(MoveRefusal::Moving) => {
                            refused(shared, &name, &t("session-bluetooth-moving-already"));
                        }
                    }
                    continue;
                }
                Done::Turned { turn, worked } => {
                    // A device that holds no bond of the accessory says so; it is no failure.
                    if let Err(error) = &worked
                        && turn.radio != Radio::Inspect
                    {
                        tracing::warn!(%error, radio = ?turn.radio, "a Bluetooth step did not work");
                    }
                    accessory::finished(
                        &turn,
                        worked.is_ok(),
                        &mut self.part.borrow_mut(),
                        now_ns(),
                    )
                }
            };
            if let Some(write) = write
                && let Err(error) = record.invoke(&write)
            {
                tracing::warn!(%error, "how a Bluetooth step went was not written");
            }
        }

        let online: BTreeSet<String> = fabric
            .online_device_ids()
            .iter()
            .map(|device| device.as_str().to_owned())
            .collect();
        let held = record.get();
        let (writes, turns) = accessory::turns(
            &held,
            &local,
            |key| online.contains(key),
            now_ns(),
            &mut self.part.borrow_mut(),
        );
        for write in writes {
            if let Err(error) = record.invoke(&write) {
                tracing::warn!(%error, "a Bluetooth move was not given up");
            }
        }
        for turn in turns {
            self.spawn_turn(turn);
        }
        self.tell_ended(shared, &held, &local);
    }

    /// Check that the accessory the person named can move to `device`, and read what it is called
    /// on a worker: the first step, which answers the person when it cannot.
    fn begin(&self, shared: &Shared, fabric: &SharedLink, address: &str, device: &str) {
        if !os_actions::bluez::valid_address(address) {
            refused(shared, address, &t("session-bluetooth-unknown"));
            return;
        }
        let Ok(target) = DeviceId::new(device) else {
            refused(shared, address, &t("session-bluetooth-target-unknown"));
            return;
        };
        if shared.owner_devices.is_none() {
            refused(shared, address, &t("session-bluetooth-no-identity"));
            return;
        }
        let Some(destination) = fabric.devices().get(&target).cloned() else {
            refused(shared, address, &t("session-bluetooth-target-unknown"));
            return;
        };
        if !destination.online {
            refused(shared, address, &t("session-bluetooth-target-away"));
            return;
        }
        let sender = self.sender.clone();
        let (address, target) = (address.to_ascii_uppercase(), target.as_str().to_owned());
        std::thread::spawn(move || {
            let name = connected_name(&address);
            let _ = sender.send(Done::Begun {
                address,
                target,
                name,
            });
        });
    }

    /// Do one piece of this device's part of a move on a thread of its own, so the pass never
    /// waits on `BlueZ`.
    fn spawn_turn(&self, turn: Turn) {
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let worked = match turn.radio {
                Radio::Inspect => {
                    if paired(&turn.address) {
                        Ok(())
                    } else {
                        Err("this device holds no bond of the accessory".to_owned())
                    }
                }
                Radio::Release | Radio::LetGo => {
                    os_actions::bluez::disconnect_address(&turn.address)
                }
                Radio::Take | Radio::GiveBack => os_actions::bluez::connect_address(&turn.address),
            };
            let _ = sender.send(Done::Turned { turn, worked });
        });
    }

    /// Tell the person how each move this device asked ended, and that an accessory moved here,
    /// once the record says so.
    fn tell_ended(&self, shared: &Shared, record: &Devices, local: &str) {
        let mut told = self.told.borrow_mut();
        for (address, kept) in record.moves.iter() {
            let origin = kept.from == local;
            let ended_here = origin && kept.stage.ended();
            let came_here = kept.to == local && kept.stage == MoveStage::Moved;
            if kept.asked_ns < self.since_ns
                || !(ended_here || came_here)
                || !told.insert((address.as_str().to_owned(), kept.asked_ns))
            {
                continue;
            }
            if origin {
                tell_end(shared, record, kept);
            } else {
                let device = crate::wake::label(record, &key(&kept.from));
                tell(
                    shared,
                    0,
                    t_args(
                        "session-bluetooth-taken",
                        &[("accessory", kept.name.as_str().into())],
                    ),
                    t_args(
                        "session-bluetooth-taken-body",
                        &[("device", device.as_str().into())],
                    ),
                );
            }
        }
    }
}

/// Tell the person how the move they asked of `kept` ended.
fn tell_end(shared: &Shared, record: &Devices, kept: &AccessoryMove) {
    let device = crate::wake::label(record, &key(&kept.to));
    let detail = match kept.stage {
        MoveStage::Moved => {
            tell(
                shared,
                0,
                t_args(
                    "session-bluetooth-moved",
                    &[("accessory", kept.name.as_str().into())],
                ),
                t_args(
                    "session-bluetooth-moved-body",
                    &[("device", device.as_str().into())],
                ),
            );
            return;
        }
        MoveStage::NotPaired => t_args(
            "session-bluetooth-not-paired-there",
            &[("device", device.as_str().into())],
        ),
        MoveStage::NotReleased => t("session-bluetooth-not-released"),
        MoveStage::NotConnected => t_args(
            "session-bluetooth-not-connected-there",
            &[("device", device.as_str().into())],
        ),
        MoveStage::GivenUp if kept.ran_out() => t("session-bluetooth-timed-out"),
        MoveStage::GivenUp => t("session-bluetooth-target-away"),
        MoveStage::Asked | MoveStage::Ready | MoveStage::Released => return,
    };
    refused(shared, &kept.name, &detail);
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

/// The record's key of the device `device` (lowercase hex).
fn key(device: &str) -> tilo_resource::Id {
    tilo_resource::Id::from_raw(device.to_owned())
}

/// Tell the person how one move went.
fn tell(shared: &Shared, urgency: u8, title: String, body: String) {
    shared.notifications.borrow().tell(&Notice::new(
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

fn now_ns() -> i64 {
    i64::try_from(tilo_clock::unix_now_ns()).unwrap_or(i64::MAX)
}
