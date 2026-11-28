//! Using one of the person's phones' connection on this device for a while, and sharing this
//! phone's with another of their devices.
//!
//! The share is asked and carried out in the record of the person's devices
//! ([`tilo_workspace_service::workspaces::person_workspace::tethering`]): this session asks the
//! shares the person starts here, reads its part of each share from the record, and does it. As
//! the phone, it asks its person whether to share (an asked record, like every other question
//! here), starts its hotspot under a fresh passphrase and shares that passphrase with the person's
//! devices the way a saved network's is shared ([`crate::wifi_continuity`]), and stops the hotspot
//! once the share ends. As the device that asked, it joins that hotspot with the passphrase the
//! daemon opens for it, and leaves it once the share ends. Radio work runs on a worker of its own,
//! so no pass waits on `NetworkManager`; it tells the person how each share went.
//!
//! Both ends need the person's Wi-Fi continuity (Settings, Devices), since the passphrase travels
//! as a shared Wi-Fi passphrase does.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::sync::{Arc, mpsc};

use serde::{Deserialize, Serialize};
use tilo_connectivity_service::device::{Change, Device, OsDevice};
use tilo_device_surfaces::devices::ConnectionView;
use tilo_service::{Asked, Question};
use tilo_workspace_service::workspaces::person_workspace::devices::{Devices, Tether, TetherStage};
use tilo_workspace_service::workspaces::person_workspace::tethering::{
    self, Outcome, Part, SHARE_MS, Step, TetherRefusal, Turn,
};

use crate::Shared;
use crate::followed_resource::Sending;
use crate::i18n::{t, t_args};
use crate::platform::fabric::SharedLink;
use crate::platform::notice::Notice;
use crate::wifi_continuity::WifiSharing;

/// The question's kind, as the phone's person is asked it ([`crate::program_asks`]).
pub(crate) const KIND: &str = "devices.share-connection";

/// What the question shows: the device that asks for this phone's connection.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ShareQuestion {
    pub(crate) device: String,
}

/// What the person asked of the share on this device.
enum Wanted {
    /// Use the connection of the phone by this key.
    Use(String),
    /// Stop using a phone's connection, or stop sharing this one.
    Stop,
}

/// What a worker did.
struct Done {
    turn: Turn,
    outcome: Outcome,
}

/// This session's part in the shares of the person's phones' connections.
pub(crate) struct Tethering {
    /// When this session started, ms since the epoch: a share asked before it was told by the
    /// session then.
    since_ms: i64,
    sharing: WifiSharing,
    wanted: RefCell<Vec<Wanted>>,
    part: RefCell<Part>,
    /// The questions this phone's person was asked, until they answer.
    asking: RefCell<Vec<(Turn, Asked)>>,
    /// The shares it told the person the end of, by device and ask.
    told: RefCell<BTreeSet<(String, i64, TetherStage)>>,
    sender: mpsc::Sender<Done>,
    results: RefCell<mpsc::Receiver<Done>>,
    /// The shares given up or ended, on their way to the record.
    ending: RefCell<Option<Sending>>,
}

impl Tethering {
    /// The shares of this session, whose passphrases travel through `sharing`.
    pub(crate) fn new(sharing: WifiSharing) -> Self {
        let (sender, results) = mpsc::channel();
        Self {
            since_ms: now_ms(),
            sharing,
            wanted: RefCell::default(),
            part: RefCell::default(),
            asking: RefCell::default(),
            told: RefCell::default(),
            sender,
            results: RefCell::new(results),
            ending: RefCell::default(),
        }
    }

    /// Use the connection of the person's phone `phone` here, for a while.
    pub(crate) fn use_connection(&self, phone: &str) {
        self.wanted.borrow_mut().push(Wanted::Use(phone.to_owned()));
    }

    /// Stop the share this device takes part in.
    pub(crate) fn stop(&self) {
        self.wanted.borrow_mut().push(Wanted::Stop);
    }

    /// Do what the person, the workers and the record left for this pass.
    pub(crate) fn pump(&self, shared: &Shared, fabric: &SharedLink) {
        let Some(devices) = shared.owner_devices.as_ref() else {
            self.wanted.borrow_mut().clear();
            return;
        };
        let (record, local) = devices.record();
        let now = now_ms();
        for wanted in std::mem::take(&mut *self.wanted.borrow_mut()) {
            let held = record.get();
            let write = match wanted {
                Wanted::Use(phone) => {
                    if let Err(why) = tilo_runtime::device::wifi::permitted() {
                        tell(shared, 1, t("session-tether-not-shared"), why);
                        continue;
                    }
                    match tethering::asking(&held, &local, &phone, now, SHARE_MS) {
                        Ok(ask) => {
                            let label = crate::wake::label(&held, &key(&phone));
                            tell(
                                shared,
                                0,
                                t_args("session-tether-asked", &[("device", label.into())]),
                                t("session-tether-asked-body"),
                            );
                            ask
                        }
                        Err(refusal) => {
                            tell(
                                shared,
                                1,
                                t("session-tether-not-shared"),
                                refusal_words(refusal),
                            );
                            continue;
                        }
                    }
                }
                Wanted::Stop => match own_share(&held, &local, now) {
                    Some((device, kept)) => tethering::ending(&device, &kept, now),
                    None => continue,
                },
            };
            record.invoke_unwaited(&write, |written| {
                if let Err(error) = written {
                    tracing::warn!(%error, "a share of a phone's connection was not written");
                }
            });
        }

        self.answered(shared);
        let done: Vec<Done> = self.results.borrow().try_iter().collect();
        for Done { turn, outcome } in done {
            if let Some(write) =
                tethering::finished(&turn, &outcome, &mut self.part.borrow_mut(), now_ms())
            {
                record.invoke_unwaited(&write, |written| {
                    if let Err(error) = written {
                        tracing::warn!(%error, "how a share went was not written");
                    }
                });
            }
        }

        let online: BTreeSet<String> = fabric
            .online_device_ids()
            .iter()
            .map(|device| device.as_str().to_owned())
            .collect();
        let held = record.get();
        // A question whose share ended meanwhile (given up, or asked again) is taken back.
        self.asking.borrow_mut().retain(|(turn, _)| {
            held.tethers.get(&key(&turn.device)).is_some_and(|kept| {
                kept.asked_ms == turn.asked_ms && kept.stage == TetherStage::Asked
            })
        });
        let (writes, turns) = tethering::turns(
            &held,
            &local,
            |key| online.contains(key),
            now,
            &mut self.part.borrow_mut(),
        );
        let ending = self
            .ending
            .borrow()
            .as_ref()
            .is_some_and(|sending| sending.went().is_none());
        if !writes.is_empty() && !ending {
            *self.ending.borrow_mut() = Some(record.send(&writes));
        }
        for turn in turns {
            self.begin(shared, &held, turn);
        }
        self.tell_ended(shared, &held, &local);
    }

    /// Begin one piece of this device's part of a share.
    fn begin(&self, shared: &Shared, record: &Devices, turn: Turn) {
        match turn.step {
            Step::Ask => {
                // Sharing is Wi-Fi continuity: with it off, the phone shares nothing and asks
                // its person nothing.
                if tilo_runtime::device::wifi::permitted().is_err() {
                    let _ = self.sender.send(Done {
                        turn,
                        outcome: Outcome::Declined,
                    });
                    return;
                }
                let label = crate::wake::label(record, &key(&turn.device));
                let asked = shared.asker.ask(Question {
                    handle: format!("{KIND}:{}:{}", turn.device, turn.asked_ms),
                    kind: KIND.to_owned(),
                    app_id: None,
                    label: label.clone(),
                    details: serde_json::to_value(ShareQuestion { device: label })
                        .unwrap_or_default(),
                });
                self.asking.borrow_mut().push((turn, asked));
            }
            Step::Join => {
                let (sender, sharing) = (self.sender.clone(), self.sharing.clone());
                std::thread::spawn(move || {
                    let joined = sharing.device(radio()).change(&Change::Join {
                        network: turn.network.clone(),
                        password: None,
                        hidden: false,
                    });
                    if let Err(error) = &joined {
                        tracing::warn!(%error, "the phone's hotspot was not joined");
                    }
                    let _ = sender.send(Done {
                        turn,
                        outcome: Outcome::Joined(joined.is_ok()),
                    });
                });
            }
            Step::StopSharing => {
                let sender = self.sender.clone();
                std::thread::spawn(move || {
                    if let Err(error) = stop_hotspot() {
                        tracing::warn!(%error, "the shared hotspot did not stop");
                    }
                    let _ = sender.send(Done {
                        turn,
                        outcome: Outcome::Stopped,
                    });
                });
            }
            Step::Leave => {
                let sender = self.sender.clone();
                std::thread::spawn(move || {
                    if let Err(error) = radio().change(&Change::Leave {
                        network: turn.network.clone(),
                    }) {
                        tracing::warn!(%error, "the phone's hotspot was not left");
                    }
                    let _ = sender.send(Done {
                        turn,
                        outcome: Outcome::Stopped,
                    });
                });
            }
        }
    }

    /// Take in the phone's person's answers: a yes starts the hotspot and shares its passphrase
    /// on a worker; a no says so.
    fn answered(&self, shared: &Shared) {
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        let mut answered = Vec::new();
        self.asking.borrow_mut().retain_mut(|(turn, asked)| {
            match std::pin::Pin::new(asked).poll(&mut context) {
                std::task::Poll::Ready(answer) => {
                    answered.push((turn.clone(), answer.is_some()));
                    false
                }
                std::task::Poll::Pending => true,
            }
        });
        for (turn, yes) in answered {
            if !yes {
                let _ = self.sender.send(Done {
                    turn,
                    outcome: Outcome::Declined,
                });
                continue;
            }
            let label = shared
                .owner_devices
                .as_ref()
                .map(|devices| devices.cert().device_label.clone())
                .unwrap_or_default();
            let (sender, sharing) = (self.sender.clone(), self.sharing.clone());
            std::thread::spawn(move || {
                let network = tethering::network_name(&label);
                let outcome = match start_sharing(&sharing, &network) {
                    Ok(()) => Outcome::Started { network },
                    Err(error) => {
                        tracing::warn!(%error, "the phone's hotspot did not start");
                        let _ = os_actions::set_hotspot(false);
                        Outcome::NotStarted
                    }
                };
                let _ = sender.send(Done { turn, outcome });
            });
        }
    }

    /// Tell the person how each share this device took part in went, once the record says so.
    fn tell_ended(&self, shared: &Shared, record: &Devices, local: &str) {
        let mut told = self.told.borrow_mut();
        for (device, kept) in record.tethers.iter() {
            let asking = device.as_str() == local;
            if (!asking && kept.phone != local) || kept.asked_ms < self.since_ms {
                continue;
            }
            let worth = matches!(
                (kept.stage, asking),
                (
                    TetherStage::Joined | TetherStage::Declined | TetherStage::NotStarted,
                    true
                ) | (
                    TetherStage::NotJoined | TetherStage::Ended | TetherStage::GivenUp,
                    _
                ) | (TetherStage::Shared, false)
            );
            if !worth || !told.insert((device.as_str().to_owned(), kept.asked_ms, kept.stage)) {
                continue;
            }
            let other = if asking {
                kept.phone.as_str()
            } else {
                device.as_str()
            };
            let label = crate::wake::label(record, &key(other));
            let args = [("device", label.into())];
            let (urgency, title, body) = match (kept.stage, asking) {
                (TetherStage::Joined, _) => (
                    0,
                    t_args("session-tether-joined", &args),
                    t("session-tether-joined-body"),
                ),
                (TetherStage::Shared, _) => (
                    0,
                    t_args("session-tether-sharing", &args),
                    t("session-tether-sharing-body"),
                ),
                (TetherStage::Declined, _) => (
                    1,
                    t("session-tether-not-shared"),
                    t_args("session-tether-declined", &args),
                ),
                (TetherStage::NotStarted, _) => (
                    1,
                    t("session-tether-not-shared"),
                    t_args("session-tether-not-started", &args),
                ),
                (TetherStage::NotJoined, _) => (
                    1,
                    t("session-tether-not-shared"),
                    t_args("session-tether-not-joined", &args),
                ),
                (TetherStage::GivenUp, _) => (
                    1,
                    t("session-tether-not-shared"),
                    t_args("session-tether-given-up", &args),
                ),
                _ => (
                    0,
                    t("session-tether-ended"),
                    t_args("session-tether-ended-body", &args),
                ),
            };
            tell(shared, urgency, title, body);
        }
    }
}

/// The share the device `local` (hex) uses or gives in `record` now, as the Devices section shows
/// it.
pub(crate) fn connection(record: &Devices, local: &str) -> Option<ConnectionView> {
    let (device, kept) = own_share(record, local, now_ms())?;
    let asking = device == local;
    let other = if asking {
        kept.phone.as_str()
    } else {
        device.as_str()
    };
    Some(ConnectionView {
        device: other.to_owned(),
        label: crate::wake::label(record, &key(other)),
        sharing: !asking,
        network: kept.network.clone(),
        joined: kept.stage == TetherStage::Joined,
        until_ms: kept.until_ms,
    })
}

/// The environment a development build's tests set to keep a share off this machine's
/// `NetworkManager`: the hotspot and the joins happen on a radio in memory.
const TEST_RADIO_ENV: &str = "TILO_TETHER_RADIO";

/// Whether this build runs a share on the radio in memory: a development build, told by its
/// environment, and never a released one.
fn test_radio() -> bool {
    cfg!(debug_assertions) && std::env::var_os(TEST_RADIO_ENV).is_some_and(|value| value == "test")
}

/// This device's own radio, whose changes join and leave networks.
fn radio() -> Arc<dyn Device> {
    #[cfg(debug_assertions)]
    if test_radio() {
        return Arc::new(TestRadio);
    }
    Arc::new(OsDevice)
}

/// Start this phone's hotspot as `network` under a fresh passphrase, and share that passphrase
/// with the person's devices as a saved network's is shared.
fn start_sharing(sharing: &WifiSharing, network: &str) -> Result<(), String> {
    let password = hex::encode(tilo_network_actor::fabric::nonzero::<12>());
    if !test_radio() {
        os_actions::configure_hotspot(&os_actions::HotspotConfiguration {
            ssid: network.to_owned(),
            password: password.clone(),
            band: os_actions::HotspotBand::Automatic,
        })?;
    }
    sharing.share_hotspot(network, &password)
}

/// Stop the hotspot this phone shared.
fn stop_hotspot() -> Result<(), String> {
    if test_radio() {
        return Ok(());
    }
    os_actions::set_hotspot(false)
}

/// A radio in memory for a development build's tests: the phone's hotspot is in range, and
/// leaving it always works. The passphrase a protected hotspot needs is proven by the Wi-Fi
/// continuity checks, which keep this seam about the share itself.
#[cfg(debug_assertions)]
struct TestRadio;

#[cfg(debug_assertions)]
impl Device for TestRadio {
    fn read(&self) -> tilo_connectivity_service::device::Readings {
        tilo_connectivity_service::device::Readings::default()
    }

    fn change(&self, change: &Change) -> Result<(), String> {
        match change {
            Change::Join { .. } | Change::Leave { .. } => Ok(()),
            _ => Err("the test's radio joins and leaves networks only".to_owned()),
        }
    }

    fn scan_operators(&self, _modem: &str) -> Result<Vec<os_actions::Operator>, String> {
        Err("this radio has no modem".to_owned())
    }

    fn send_code(&self, _modem: &str, _command: &str) -> Result<os_actions::CodeAnswer, String> {
        Err("this radio has no modem".to_owned())
    }

    fn answer_code(&self, _modem: &str, _response: &str) -> Result<os_actions::CodeAnswer, String> {
        Err("this radio has no modem".to_owned())
    }

    fn cancel_code(&self, _modem: &str) -> Result<(), String> {
        Err("this radio has no modem".to_owned())
    }
}

/// The share this device takes part in that is under way at `now_ms`: the key of the device the
/// phone shares with, and the share.
fn own_share(record: &Devices, local: &str, now_ms: i64) -> Option<(String, Tether)> {
    record.tethers.iter().find_map(|(device, kept)| {
        (kept.under_way(now_ms) && (device.as_str() == local || kept.phone == local))
            .then(|| (device.as_str().to_owned(), kept.clone()))
    })
}

/// Why a share could not be asked, in the person's words.
fn refusal_words(refusal: TetherRefusal) -> String {
    match refusal {
        TetherRefusal::UnknownDevice => t("session-tether-unknown"),
        TetherRefusal::SameDevice => t("session-tether-same"),
        TetherRefusal::Sharing => t("session-tether-already"),
    }
}

/// The record's key of the device `device` (lowercase hex).
fn key(device: &str) -> tilo_resource::Id {
    tilo_resource::Id::from_raw(device.to_owned())
}

/// Tell the person how one share went.
fn tell(shared: &Shared, urgency: u8, title: String, body: String) {
    shared
        .notifications
        .borrow()
        .tell(&Notice::new(t("session-tether-app"), urgency, title, body));
}

fn now_ms() -> i64 {
    i64::try_from(tilo_clock::unix_now_ns() / 1_000_000).unwrap_or(i64::MAX)
}
