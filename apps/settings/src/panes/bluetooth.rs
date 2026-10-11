#![allow(clippy::doc_markdown, reason = "BlueZ is the upstream service name")]
//! Bluetooth settings backed directly by `BlueZ` over D-Bus. Settings starts
//! operations; the compositor-owned `BlueZ` Agent owns security prompts.
//! Every change is a [`BluetoothEffect`], offered on the device it names.

use std::cell::OnceCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use tilo_bluez::{DeviceInfo, PairOutcome};
use ui::LocalState;
use ui::framework::{EffectAnswer, EffectCx, Effected, LiveRow};
use ui::i18n::ArgValue;
use ui::tilo_resource::{self, Effect};
use ui::widget::{Button, Row, TextInput, Toggle, Widget};

use crate::effects::{SettingsEffect, effect};
use crate::host::PaneCx;
use crate::i18n::{t, t_args};
use crate::panes::model::{PaneModel, Shared};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::widgets::{
    Item, card_with_action, error_caption, note, row, rows, setting_row, setting_row_described,
    value_row,
};

fn token(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).replace('.', "_")
}

/// Bluetooth on this device: the radio, discovery, and the devices it
/// knows, each by its `BlueZ` object path.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum BluetoothEffect {
    /// Turn the Bluetooth radio on or off.
    #[effect(job, idempotent)]
    SetBluetooth { on: bool },
    /// Look for nearby devices, or stop looking.
    #[effect(job, idempotent)]
    SetDiscovering { on: bool },
    /// Read the devices again.
    #[effect(job, idempotent)]
    RefreshBluetooth,
    /// Pair with a nearby device; the device may ask to confirm a code.
    #[effect(job, row, label = "Pair")]
    PairDevice {
        #[param("The device's object path")]
        device: String,
    },
    /// Stop pairing with a device.
    #[effect(job, row, label = "Cancel pairing")]
    CancelPairing {
        #[param("The device's object path")]
        device: String,
    },
    /// Connect a paired device.
    #[effect(job, row, idempotent, label = "Connect")]
    ConnectDevice {
        #[param("The device's object path")]
        device: String,
    },
    /// Disconnect a device.
    #[effect(job, row, idempotent, label = "Disconnect")]
    DisconnectDevice {
        #[param("The device's object path")]
        device: String,
    },
    /// Let a paired device connect without asking.
    #[effect(job, row, idempotent)]
    SetDeviceTrusted {
        #[param("The device's object path")]
        device: String,
        trusted: bool,
    },
    /// Give a paired device another name on this device.
    #[effect(job, row)]
    RenameDevice {
        #[param("The device's object path")]
        device: String,
        #[param("The new name")]
        name: String,
    },
    /// Forget a paired device; it must pair again to connect.
    #[effect(job, row, destructive, label = "Forget")]
    ForgetDevice {
        #[param("The device's object path")]
        device: String,
    },
}

/// Answer `effect` on a worker: `BlueZ` may take seconds. What it reports
/// becomes the pane's notice, and the devices are read again.
pub(crate) fn answer(
    app: &SettingsApp,
    effect: BluetoothEffect,
    cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    let model = app.bluetooth().model();
    let (control, name) = {
        let mut model = model.borrow_mut();
        let name = effect_device(&effect).and_then(|path| model.name_of(path));
        if let BluetoothEffect::PairDevice { device } = &effect {
            if model.pairing.is_some() {
                return Err(t("settings-bluetooth-pairing-busy"));
            }
            model.pairing = Some(device.clone());
            model.notice = Some(t_args(
                "settings-bluetooth-pairing",
                &[("name", name.clone().unwrap_or_default().into())],
            ));
            model.changed = true;
        }
        (Arc::clone(&model.control), name)
    };
    if effect_device(&effect).is_some() && name.is_none() {
        return Err(t("settings-bluetooth-device-gone"));
    }
    let name = name.unwrap_or_default();
    Ok(cx.thread(
        move |_| carry_out(control.as_ref(), &effect, &name),
        |app: &mut SettingsApp, outcome: Result<String, String>, _| {
            let model = app.bluetooth().model();
            let mut model = model.borrow_mut();
            model.pairing = None;
            model.notice = Some(outcome.clone().unwrap_or_else(|error| error));
            model.refresh();
            outcome.map(|_| Effected::done())
        },
    ))
}

fn effect_device(effect: &BluetoothEffect) -> Option<&str> {
    match effect {
        BluetoothEffect::SetBluetooth { .. }
        | BluetoothEffect::SetDiscovering { .. }
        | BluetoothEffect::RefreshBluetooth => None,
        BluetoothEffect::PairDevice { device }
        | BluetoothEffect::CancelPairing { device }
        | BluetoothEffect::ConnectDevice { device }
        | BluetoothEffect::DisconnectDevice { device }
        | BluetoothEffect::SetDeviceTrusted { device, .. }
        | BluetoothEffect::RenameDevice { device, .. }
        | BluetoothEffect::ForgetDevice { device } => Some(device),
    }
}

/// Carry out `effect` on `control`, for the device called `name`, and say
/// what happened. Never called on the UI thread.
fn carry_out(
    control: &dyn BluetoothControl,
    effect: &BluetoothEffect,
    name: &str,
) -> Result<String, String> {
    let named = |key: &str| t_args(key, &[("name", name.to_owned().into())]);
    let failed = |key: &str, error: String| {
        t_args(
            key,
            &[("name", name.to_owned().into()), ("error", error.into())],
        )
    };
    match effect {
        BluetoothEffect::SetBluetooth { on } => control
            .set_powered(*on)
            .map(|()| {
                t(if *on {
                    "settings-bluetooth-on"
                } else {
                    "settings-bluetooth-off"
                })
            })
            .map_err(|error| failed("settings-bluetooth-power-failed", error)),
        BluetoothEffect::SetDiscovering { on } => {
            let started = if *on {
                control.start_scan()
            } else {
                control.stop_scan()
            };
            started
                .map(|()| {
                    t(if *on {
                        "settings-bluetooth-discovery-started-notice"
                    } else {
                        "settings-bluetooth-discovery-stopped-notice"
                    })
                })
                .map_err(|error| failed("settings-bluetooth-discovery-failed", error))
        }
        BluetoothEffect::RefreshBluetooth => control
            .devices()
            .map(|_| t("settings-bluetooth-refreshed"))
            .map_err(|error| failed("settings-bluetooth-refresh-failed", error)),
        BluetoothEffect::PairDevice { device } => match control.pair(device) {
            Ok(PairOutcome::PairedAndConnected) => Ok(named("settings-bluetooth-paired-connected")),
            Ok(PairOutcome::PairedNotConnected { reason }) => Ok(t_args(
                "settings-bluetooth-paired-not-connected",
                &[("name", name.to_owned().into()), ("reason", reason.into())],
            )),
            Err(error) => Err(failed("settings-bluetooth-pair-failed", error)),
        },
        BluetoothEffect::CancelPairing { device } => control
            .cancel_pairing(device)
            .map(|()| named("settings-bluetooth-cancelled"))
            .map_err(|error| failed("settings-bluetooth-cancel-failed", error)),
        BluetoothEffect::ConnectDevice { device } => control
            .connect(device)
            .map(|()| named("settings-bluetooth-connected-name"))
            .map_err(|error| failed("settings-bluetooth-connect-failed", error)),
        BluetoothEffect::DisconnectDevice { device } => control
            .disconnect(device)
            .map(|()| named("settings-bluetooth-disconnected-name"))
            .map_err(|error| failed("settings-bluetooth-disconnect-failed", error)),
        BluetoothEffect::SetDeviceTrusted { device, trusted } => control
            .set_trusted(device, *trusted)
            .map(|()| {
                named(if *trusted {
                    "settings-bluetooth-now-trusted"
                } else {
                    "settings-bluetooth-now-untrusted"
                })
            })
            .map_err(|error| failed("settings-bluetooth-trust-failed", error)),
        BluetoothEffect::RenameDevice {
            device,
            name: alias,
        } => control
            .set_alias(device, alias)
            .map(|()| {
                t_args(
                    "settings-bluetooth-renamed",
                    &[("alias", alias.trim().to_owned().into())],
                )
            })
            .map_err(|error| failed("settings-bluetooth-rename-failed", error)),
        BluetoothEffect::ForgetDevice { device } => control
            .forget(device)
            .map(|()| named("settings-bluetooth-forgot"))
            .map_err(|error| failed("settings-bluetooth-forget-failed", error)),
    }
}

pub trait BluetoothControl: Send + Sync {
    /// # Errors
    ///
    /// Returns the backend's error message if `BlueZ` cannot be reached over
    /// D-Bus or the known device list cannot be enumerated.
    fn devices(&self) -> Result<Vec<DeviceInfo>, String>;
    /// # Errors
    ///
    /// Returns the backend's error message if the adapter's powered state
    /// cannot be read over D-Bus.
    fn powered(&self) -> Result<bool, String>;
    /// # Errors
    ///
    /// Returns the backend's error message if the adapter's discovery state
    /// cannot be read over D-Bus.
    fn scanning(&self) -> Result<bool, String>;
    /// # Errors
    ///
    /// Returns the backend's error message if `BlueZ` refuses or fails to
    /// change the adapter's powered state.
    fn set_powered(&self, on: bool) -> Result<(), String>;
    /// # Errors
    ///
    /// Returns the backend's error message if `BlueZ` fails to start device
    /// discovery.
    fn start_scan(&self) -> Result<(), String>;
    /// # Errors
    ///
    /// Returns the backend's error message if `BlueZ` fails to stop device
    /// discovery.
    fn stop_scan(&self) -> Result<(), String>;
    /// # Errors
    ///
    /// Returns the backend's error message if the D-Bus pairing call fails
    /// outright (a pairing that starts but does not connect is still `Ok`).
    fn pair(&self, path: &str) -> Result<PairOutcome, String>;
    /// # Errors
    ///
    /// Returns the backend's error message if `BlueZ` fails to cancel an
    /// in-progress pairing.
    fn cancel_pairing(&self, path: &str) -> Result<(), String>;
    /// # Errors
    ///
    /// Returns the backend's error message if the device cannot be connected
    /// to (e.g. it is out of range or rejects the connection).
    fn connect(&self, path: &str) -> Result<(), String>;
    /// # Errors
    ///
    /// Returns the backend's error message if the device cannot be
    /// disconnected.
    fn disconnect(&self, path: &str) -> Result<(), String>;
    /// # Errors
    ///
    /// Returns the backend's error message if `BlueZ` fails to update the
    /// device's trusted flag.
    fn set_trusted(&self, path: &str, trusted: bool) -> Result<(), String>;
    /// # Errors
    ///
    /// Returns the backend's error message if `BlueZ` fails to rename the
    /// device.
    fn set_alias(&self, path: &str, alias: &str) -> Result<(), String>;
    /// # Errors
    ///
    /// Returns the backend's error message if `BlueZ` fails to remove the
    /// device's pairing and keys.
    fn forget(&self, path: &str) -> Result<(), String>;
}

pub struct RealBluetooth;
impl BluetoothControl for RealBluetooth {
    fn devices(&self) -> Result<Vec<DeviceInfo>, String> {
        os_actions::bluez::devices()
    }
    fn powered(&self) -> Result<bool, String> {
        os_actions::bluez::is_powered()
    }
    fn scanning(&self) -> Result<bool, String> {
        os_actions::bluez::is_scanning()
    }
    fn set_powered(&self, on: bool) -> Result<(), String> {
        os_actions::bluez::set_powered(on)
    }
    fn start_scan(&self) -> Result<(), String> {
        os_actions::bluez::start_scan()
    }
    fn stop_scan(&self) -> Result<(), String> {
        os_actions::bluez::stop_scan()
    }
    fn pair(&self, path: &str) -> Result<PairOutcome, String> {
        os_actions::bluez::pair(path)
    }
    fn cancel_pairing(&self, path: &str) -> Result<(), String> {
        os_actions::bluez::cancel_pairing(path)
    }
    fn connect(&self, path: &str) -> Result<(), String> {
        os_actions::bluez::connect(path)
    }
    fn disconnect(&self, path: &str) -> Result<(), String> {
        os_actions::bluez::disconnect(path)
    }
    fn set_trusted(&self, path: &str, v: bool) -> Result<(), String> {
        os_actions::bluez::set_trusted(path, v)
    }
    fn set_alias(&self, path: &str, alias: &str) -> Result<(), String> {
        os_actions::bluez::set_alias(path, alias)
    }
    fn forget(&self, path: &str) -> Result<(), String> {
        os_actions::bluez::forget(path)
    }
}

/// What the Bluetooth area keeps for the window: the model, read when
/// first needed.
pub struct BluetoothArea {
    cx: PaneCx,
    model: OnceCell<Shared<BluetoothModel>>,
    /// The adapter, read off the UI thread after the window's first frame
    /// ([`Self::start_read`]).
    read: crate::device_read::DeviceRead<AdapterRead>,
}

impl BluetoothArea {
    pub fn new(cx: &PaneCx) -> Self {
        Self {
            cx: cx.clone(),
            model: OnceCell::new(),
            read: crate::device_read::DeviceRead::default(),
        }
    }

    /// `BlueZ`, or sample devices for a screenshot.
    fn control(&self) -> Arc<dyn BluetoothControl> {
        if self.cx.reaches_devices() {
            Arc::new(RealBluetooth)
        } else {
            Arc::new(SampleBluetooth)
        }
    }

    /// The model, over `BlueZ`, or over sample devices for a screenshot.
    pub fn model(&self) -> &Shared<BluetoothModel> {
        self.model.get_or_init(|| {
            let control = self.control();
            let read = self
                .read
                .take()
                .unwrap_or_else(|| read_adapter_of(control.as_ref()));
            Rc::new(std::cell::RefCell::new(BluetoothModel::with_read(
                self.cx.clone(),
                control,
                read,
            )))
        })
    }

    /// Read the adapter off the UI thread; `false` when it has been read or
    /// is being read.
    pub(crate) fn start_read(&self) -> bool {
        if self.is_open() {
            return false;
        }
        let control = self.control();
        self.read.start(move || read_adapter_of(control.as_ref()))
    }

    /// Whether the read [`Self::start_read`] started has ended.
    pub(crate) fn read_finished(&self) -> bool {
        self.read.is_finished()
    }

    /// Whether the area has read what its rows show: its pane was shown,
    /// an effect or a row lookup reached it, or the window read it after
    /// its first frame ([`crate::effects::LIVE_AREAS`]).
    pub(crate) fn is_open(&self) -> bool {
        self.model.get().is_some()
    }

    /// One row per device, offering what can be done to it now.
    pub(crate) fn rows(&self) -> Vec<LiveRow> {
        let model = self.model().borrow();
        if model.read_error.is_some() {
            // The last devices read say nothing about the adapter now.
            return Vec::new();
        }
        model
            .devices
            .iter()
            .map(|device| {
                let path = device.path.clone();
                let offer = |effect: BluetoothEffect| SettingsEffect::from(effect).bound();
                let mut row = LiveRow::new(
                    format!("settings.bluetooth-device:{path}"),
                    "settings.bluetooth-device",
                    device.name.clone(),
                );
                if model.pairing.as_deref() == Some(path.as_str()) {
                    return row.operation(offer(BluetoothEffect::CancelPairing { device: path }));
                }
                if !device.paired {
                    return row.operation(offer(BluetoothEffect::PairDevice { device: path }));
                }
                row = row.operation(offer(if device.connected {
                    BluetoothEffect::DisconnectDevice {
                        device: path.clone(),
                    }
                } else {
                    BluetoothEffect::ConnectDevice {
                        device: path.clone(),
                    }
                }));
                row.operation(offer(BluetoothEffect::SetDeviceTrusted {
                    device: path.clone(),
                    trusted: !device.trusted,
                }))
                .operation(offer(BluetoothEffect::ForgetDevice { device: path }))
            })
            .collect()
    }
}

pub struct BluetoothModel {
    cx: PaneCx,
    control: Arc<dyn BluetoothControl>,
    devices: Vec<DeviceInfo>,
    powered: bool,
    scanning: bool,
    /// Why the adapter could not be read. While set, the power switch, the
    /// scan state and the device list say nothing about the machine.
    read_error: Option<String>,
    notice: Option<String>,
    /// The device being paired, by object path.
    pairing: Option<String>,
    /// The name typed for each paired device, by object path.
    aliases: HashMap<String, Arc<Mutex<String>>>,
    changed: bool,
}

/// The adapter's devices, power and discovery state as one read found
/// them, or why it could not.
type AdapterRead = Result<(Vec<DeviceInfo>, bool, bool), String>;

/// Read `control`'s adapter: a few D-Bus round trips on a device, so the
/// window's warm-up makes this read off the UI thread.
fn read_adapter_of(control: &dyn BluetoothControl) -> AdapterRead {
    control
        .devices()
        .and_then(|devices| Ok((devices, control.powered()?, control.scanning()?)))
}

impl BluetoothModel {
    /// The model over `control`'s adapter as `read` found it.
    fn with_read(cx: PaneCx, control: Arc<dyn BluetoothControl>, read: AdapterRead) -> Self {
        let mut model = Self {
            cx,
            control,
            devices: Vec::new(),
            powered: false,
            scanning: false,
            read_error: None,
            notice: None,
            pairing: None,
            aliases: HashMap::new(),
            changed: false,
        };
        // A failure is kept on the model and shown in place of the adapter.
        let _ = model.adopt(read);
        model.changed = true;
        model
    }

    fn name_of(&self, path: &str) -> Option<String> {
        self.devices
            .iter()
            .find(|device| device.path == path)
            .map(|device| device.name.clone())
    }

    /// Read the devices, the radio and discovery again.
    fn refresh(&mut self) {
        // A failure is kept on the model and shown in place of the adapter.
        let _ = self.read_adapter();
        self.changed = true;
    }

    /// Read the adapter's power, scan state and devices. A read that fails
    /// keeps the last values and records why, so they are not shown as the
    /// adapter's state.
    fn read_adapter(&mut self) -> Result<(), String> {
        let read = read_adapter_of(self.control.as_ref());
        self.adopt(read)
    }

    /// Take in what a read of the adapter found; a failed read keeps the
    /// last values and records why.
    fn adopt(&mut self, read: AdapterRead) -> Result<(), String> {
        match read {
            Ok((devices, powered, scanning)) => {
                self.devices = devices;
                self.powered = powered;
                self.scanning = scanning;
                self.read_error = None;
                Ok(())
            }
            Err(error) => {
                tracing::warn!(target: "tilo::settings", %error, "Bluetooth adapter could not be read");
                self.read_error = Some(error.clone());
                Err(error)
            }
        }
    }

    fn device_status_line(d: &DeviceInfo) -> String {
        let status = t(if d.connected {
            "settings-bluetooth-connected"
        } else if d.paired {
            "settings-bluetooth-paired"
        } else {
            "settings-bluetooth-nearby"
        });
        match d.battery {
            Some(level) => t_args(
                "settings-bluetooth-status-battery",
                &[
                    ("status", status.into()),
                    ("level", ArgValue::Number(f64::from(level))),
                ],
            ),
            None => status,
        }
    }

    fn primary_action(&self, d: &DeviceInfo) -> (String, BluetoothEffect) {
        let device = d.path.clone();
        if self.pairing.as_deref() == Some(d.path.as_str()) {
            (
                t("settings-bluetooth-cancel-pairing"),
                BluetoothEffect::CancelPairing { device },
            )
        } else if !d.paired {
            (
                t("settings-bluetooth-pair"),
                BluetoothEffect::PairDevice { device },
            )
        } else if d.connected {
            (
                t("settings-bluetooth-disconnect"),
                BluetoothEffect::DisconnectDevice { device },
            )
        } else {
            (
                t("settings-bluetooth-connect"),
                BluetoothEffect::ConnectDevice { device },
            )
        }
    }

    fn device_detail_rows(&self, d: &DeviceInfo) -> Vec<Item> {
        let mut block: Vec<Item> = Vec::new();
        block.push(value_row(&t("settings-bluetooth-address"), &d.address));
        if let Some(rssi) = d.rssi {
            block.push(value_row(
                &t("settings-bluetooth-signal"),
                &t_args(
                    "settings-bluetooth-signal-dbm",
                    &[("rssi", ArgValue::Number(f64::from(rssi)))],
                ),
            ));
        }
        if d.paired {
            let path = d.path.clone();
            block.push(setting_row(
                &t("settings-bluetooth-trust"),
                Box::new(
                    self.cx.bind(
                        Toggle::from_state(&LocalState::new(d.trusted))
                            .with_id(&format!("settings.bluetooth.trusted.{}", token(&d.path))),
                        effect(BluetoothEffect::SET_DEVICE_TRUSTED)
                            .map(move |trusted: bool| (path.clone(), trusted)),
                    ),
                ),
            ));
            block.push(note(&t("settings-bluetooth-trust-caption")));
            let alias = self.aliases.get(&d.path).cloned().unwrap_or_default();
            // A name field has no label of its own: a free row, not a labelled one.
            block.push(row(Box::new(
                TextInput::local()
                    .with_id(&format!("bluetooth.alias.{}", token(&d.path)))
                    .with_external_buffer(Arc::clone(&alias))
                    .with_placeholder(&d.name),
            )));
            let path = d.path.clone();
            block.push(row(Box::new(
                Row::new()
                    .push(Box::new(self.cx.bind(
                        Button::new(
                            &format!("settings.bluetooth.rename.{}", token(&d.path)),
                            &t("settings-bluetooth-rename"),
                        ),
                        effect(BluetoothEffect::RENAME_DEVICE).map(move |()| {
                            (
                                path.clone(),
                                alias
                                    .lock()
                                    .map(|name| name.trim().to_owned())
                                    .unwrap_or_default(),
                            )
                        }),
                    )))
                    .push(Box::new(self.cx.bind_invocation(
                        Button::new(
                            &format!("settings.bluetooth.forget.{}", token(&d.path)),
                            &t("settings-bluetooth-forget"),
                        ),
                        BluetoothEffect::ForgetDevice {
                            device: d.path.clone(),
                        },
                    ))),
            )));
        }
        if d.icon.starts_with("audio-") {
            block.push(note(&t("settings-bluetooth-audio-caption")));
        }
        block
    }

    fn device_row(&self, d: &DeviceInfo) -> Vec<Item> {
        let (label, primary) = self.primary_action(d);
        let mut items = vec![setting_row_described(
            &d.name,
            &Self::device_status_line(d),
            Box::new(self.cx.bind_invocation(
                Button::new(
                    &format!("settings.bluetooth.primary.{}", token(&d.path)),
                    &label,
                ),
                primary,
            )),
        )];
        if d.paired {
            items.extend(self.device_detail_rows(d));
        }
        items
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let scan = t(if self.scanning {
            "settings-bluetooth-stop"
        } else {
            "settings-bluetooth-find"
        });
        let refresh = t("settings-bluetooth-refresh");
        let mut body: Vec<Item> = vec![row(Box::new(
            Row::new()
                .push(Box::new(self.cx.bind_invocation(
                    Button::new("settings.bluetooth.scan", &scan),
                    BluetoothEffect::SetDiscovering { on: !self.scanning },
                )))
                .push(Box::new(self.cx.bind_invocation(
                    Button::new("settings.bluetooth.refresh", &refresh),
                    BluetoothEffect::RefreshBluetooth,
                ))),
        ))];
        if let Some(notice) = &self.notice {
            body.push(note(notice));
        }
        if let Some(error) = &self.read_error {
            body.push(row(error_caption(&t_args(
                "settings-bluetooth-read-error",
                &[("error", error.as_str().into())],
            ))));
        } else {
            body.extend(self.devices.iter().flat_map(|d| self.device_row(d)));
            if self.devices.is_empty() {
                body.push(note(&t(if self.scanning {
                    "settings-bluetooth-looking"
                } else {
                    "settings-bluetooth-empty"
                })));
            }
        }
        crate::widgets::cards(vec![card_with_action(
            &t("settings-bluetooth-card"),
            Box::new(
                self.cx.bind(
                    Toggle::from_state(&LocalState::new(self.powered))
                        .with_id("bluetooth.toggle")
                        // An unread power state is not offered as a switch position.
                        .with_enabled(self.read_error.is_none()),
                    effect(BluetoothEffect::SET_BLUETOOTH),
                ),
            ),
            rows(body),
        )])
    }
}

impl PaneModel for BluetoothModel {
    fn compose(&self) -> Box<dyn Widget> {
        self.build_composition()
    }

    fn poll(&mut self) -> bool {
        let paired: Vec<String> = self
            .devices
            .iter()
            .filter(|device| device.paired)
            .map(|device| device.path.clone())
            .collect();
        self.aliases.retain(|path, _| paired.contains(path));
        for path in paired {
            self.aliases.entry(path).or_default();
        }
        std::mem::take(&mut self.changed)
    }

    fn name(&self) -> &'static str {
        "BluetoothPane"
    }
}

/// The devices a screenshot and the crate's tests show: headphones paired
/// and a keyboard nearby. Every change succeeds and changes nothing.
struct SampleBluetooth;

impl BluetoothControl for SampleBluetooth {
    fn devices(&self) -> Result<Vec<DeviceInfo>, String> {
        Ok(vec![
            DeviceInfo {
                path: "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_01".into(),
                address: "AA:BB:CC:DD:EE:01".into(),
                name: "Headphones".into(),
                icon: "audio-headset".into(),
                paired: true,
                trusted: true,
                connected: true,
                battery: Some(80),
                rssi: Some(-48),
            },
            DeviceInfo {
                path: "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_02".into(),
                address: "AA:BB:CC:DD:EE:02".into(),
                name: "Keyboard".into(),
                icon: "input-keyboard".into(),
                paired: false,
                trusted: false,
                connected: false,
                battery: None,
                rssi: Some(-63),
            },
        ])
    }
    fn powered(&self) -> Result<bool, String> {
        Ok(true)
    }
    fn scanning(&self) -> Result<bool, String> {
        Ok(false)
    }
    fn set_powered(&self, _: bool) -> Result<(), String> {
        Ok(())
    }
    fn start_scan(&self) -> Result<(), String> {
        Ok(())
    }
    fn stop_scan(&self) -> Result<(), String> {
        Ok(())
    }
    fn pair(&self, _: &str) -> Result<PairOutcome, String> {
        Ok(PairOutcome::PairedAndConnected)
    }
    fn cancel_pairing(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn connect(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn disconnect(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn set_trusted(&self, _: &str, _: bool) -> Result<(), String> {
        Ok(())
    }
    fn set_alias(&self, _: &str, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn forget(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Mock {
        calls: Mutex<Vec<String>>,
        devices: Vec<DeviceInfo>,
    }
    impl BluetoothControl for Mock {
        fn devices(&self) -> Result<Vec<DeviceInfo>, String> {
            Ok(self.devices.clone())
        }
        fn powered(&self) -> Result<bool, String> {
            Ok(true)
        }
        fn scanning(&self) -> Result<bool, String> {
            Ok(false)
        }
        fn set_powered(&self, v: bool) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("power:{v}"));
            Ok(())
        }
        fn start_scan(&self) -> Result<(), String> {
            Ok(())
        }
        fn stop_scan(&self) -> Result<(), String> {
            Ok(())
        }
        fn pair(&self, _: &str) -> Result<PairOutcome, String> {
            Ok(PairOutcome::PairedAndConnected)
        }
        fn cancel_pairing(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn connect(&self, p: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("connect:{p}"));
            Ok(())
        }
        fn disconnect(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn set_trusted(&self, _: &str, _: bool) -> Result<(), String> {
            Ok(())
        }
        fn set_alias(&self, _: &str, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn forget(&self, p: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("forget:{p}"));
            Ok(())
        }
    }
    /// An adapter that cannot be asked anything, as from a sandbox without
    /// the system bus.
    struct Unreachable;
    impl BluetoothControl for Unreachable {
        fn devices(&self) -> Result<Vec<DeviceInfo>, String> {
            Err("org.bluez is not reachable".into())
        }
        fn powered(&self) -> Result<bool, String> {
            Err("org.bluez is not reachable".into())
        }
        fn scanning(&self) -> Result<bool, String> {
            Err("org.bluez is not reachable".into())
        }
        fn set_powered(&self, _: bool) -> Result<(), String> {
            Err("org.bluez is not reachable".into())
        }
        fn start_scan(&self) -> Result<(), String> {
            Err("org.bluez is not reachable".into())
        }
        fn stop_scan(&self) -> Result<(), String> {
            Err("org.bluez is not reachable".into())
        }
        fn pair(&self, _: &str) -> Result<PairOutcome, String> {
            Err("org.bluez is not reachable".into())
        }
        fn cancel_pairing(&self, _: &str) -> Result<(), String> {
            Err("org.bluez is not reachable".into())
        }
        fn connect(&self, _: &str) -> Result<(), String> {
            Err("org.bluez is not reachable".into())
        }
        fn disconnect(&self, _: &str) -> Result<(), String> {
            Err("org.bluez is not reachable".into())
        }
        fn set_trusted(&self, _: &str, _: bool) -> Result<(), String> {
            Err("org.bluez is not reachable".into())
        }
        fn set_alias(&self, _: &str, _: &str) -> Result<(), String> {
            Err("org.bluez is not reachable".into())
        }
        fn forget(&self, _: &str) -> Result<(), String> {
            Err("org.bluez is not reachable".into())
        }
    }

    /// An adapter that cannot be read is reported as such: no "no devices"
    /// list and no power switch at a guessed position.
    #[test]
    fn an_unreadable_adapter_is_shown_as_an_error() {
        let profile = tempfile::tempdir().unwrap();
        let cx = PaneCx::in_file(profile.path().join("settings.json"));
        let control: Arc<dyn BluetoothControl> = Arc::new(Mock {
            calls: Mutex::new(vec![]),
            devices: vec![dev()],
        });
        let read = read_adapter_of(control.as_ref());
        let mut model = BluetoothModel::with_read(cx, control, read);
        model.control = Arc::new(Unreachable);
        assert!(model.read_adapter().is_err());
        assert_eq!(model.devices.len(), 1, "the last read is kept");
        let text = crate::widgets::announced_text(model.build_composition().as_ref());
        assert!(
            text.iter()
                .any(|line| line == "Can't read Bluetooth: org.bluez is not reachable"),
            "{text:#?}"
        );
        assert!(
            !text.iter().any(|line| line.contains("Headphones")),
            "{text:#?}"
        );
    }

    fn dev() -> DeviceInfo {
        DeviceInfo {
            path: "/org/bluez/hci0/dev_AA".into(),
            address: "AA".into(),
            name: "Headphones".into(),
            icon: "audio-headset".into(),
            paired: true,
            trusted: true,
            connected: false,
            battery: Some(0),
            rssi: Some(-42),
        }
    }

    /// Each change reaches `BlueZ` for the device's object path, and a zero
    /// battery reads as zero.
    #[test]
    fn changes_reach_the_device_by_its_path() {
        let mock = Mock {
            calls: Mutex::new(vec![]),
            devices: vec![dev()],
        };
        let path = dev().path;
        carry_out(
            &mock,
            &BluetoothEffect::ConnectDevice {
                device: path.clone(),
            },
            "Headphones",
        )
        .unwrap();
        carry_out(&mock, &BluetoothEffect::SetBluetooth { on: false }, "").unwrap();
        carry_out(
            &mock,
            &BluetoothEffect::ForgetDevice { device: path },
            "Headphones",
        )
        .unwrap();
        assert_eq!(
            mock.calls.lock().unwrap().as_slice(),
            [
                "connect:/org/bluez/hci0/dev_AA",
                "power:false",
                "forget:/org/bluez/hci0/dev_AA"
            ]
        );
        assert_eq!(mock.devices().unwrap()[0].battery, Some(0));
    }

    /// Devices are rows; forgetting one asks first, and a device the
    /// adapter does not know is refused.
    #[test]
    fn devices_are_rows_and_forgetting_asks_first() {
        use crate::resource::SettingsShell;
        use ui::prelude::TestApp;

        let mut app = TestApp::<SettingsShell>::new().unwrap();
        let headphones = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_01";
        let row = format!("settings.bluetooth-device:{headphones}");
        app.effect_on(
            &row,
            &SettingsEffect::from(BluetoothEffect::ForgetDevice {
                device: headphones.into(),
            }),
        )
        .expect_err("forgetting asks first");
        app.effect_on(
            &row,
            &SettingsEffect::from(BluetoothEffect::DisconnectDevice {
                device: headphones.into(),
            }),
        )
        .expect("the headphones disconnect");
        assert!(
            app.effect(&SettingsEffect::from(BluetoothEffect::ConnectDevice {
                device: "/org/bluez/hci0/dev_gone".into(),
            }))
            .is_err()
        );
    }
}
