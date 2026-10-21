#![allow(clippy::doc_markdown, reason = "BlueZ is the upstream service name")]
//! Bluetooth settings backed directly by `BlueZ` over D-Bus. Settings starts
//! operations; the compositor-owned `BlueZ` Agent owns security prompts.
//! Every change is a [`BluetoothEffect`], offered on the device it names.
//! A device without a Bluetooth adapter offers none of them: the radio's
//! effects are published unavailable with the reason, and the pane says so.

use std::cell::OnceCell;
use std::rc::Rc;
use std::sync::Arc;

use tilo_bluez::{DeviceInfo, PairOutcome};
use ui::LocalState;
use ui::framework::{EffectAnswer, EffectCx, EffectStatus, Effected, LiveRow};
use ui::i18n::ArgValue;
use ui::tilo_resource::{self, ActionRef, Effect};
use ui::widget::{
    Button, ButtonRole, Column, DisclosureGroup, DisclosureRow, EditableLabel, ListRow, Row,
    Toggle, Widget,
};

use crate::effects::{SettingsEffect, effect};
use crate::host::PaneCx;
use crate::i18n::{t, t_args};
use crate::panes::model::{PaneModel, Shared};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::widgets::{
    Item, block, error_caption, note, row, rows, section, section_action, untitled,
};

crate::effects::effect_family!(BluetoothEffect => answer);

crate::area::pane_area! {
    BluetoothArea;
    fn poll(&self) {
        BluetoothArea::poll(self);
    }
    fn statuses(&self) -> Vec<ui::framework::EffectStatus> {
        BluetoothArea::statuses(self)
    }
}

/// The window's [`BluetoothArea`].
pub fn area(app: &crate::settings_app::SettingsApp) -> Option<&BluetoothArea> {
    app.areas().find()
}

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
/// becomes the pane's notice, and the devices are read again off the UI
/// thread: the answer comes when the change ran, and the new reading lands
/// at a later sync.
pub(crate) fn answer(
    app: &SettingsApp,
    effect: BluetoothEffect,
    cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    if effect == BluetoothEffect::RefreshBluetooth {
        // A dongle plugged in a moment ago is seen without waiting.
        app.host().radios().look_again();
    }
    let model = crate::panes::bluetooth::area(app)
        .ok_or_else(crate::area::missing)?
        .model();
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
    Ok(crate::device_read::job(
        cx,
        move || carry_out(control.as_ref(), &effect, &name),
        |app: &mut SettingsApp, outcome: Result<String, String>, _| {
            let model = crate::panes::bluetooth::area(app)
                .ok_or_else(crate::area::missing)?
                .model();
            let mut model = model.borrow_mut();
            model.pairing = None;
            model.notice = Some(outcome.clone().unwrap_or_else(|error| error));
            model.changed = true;
            model.read_again();
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
        crate::device_read::off_ui_thread();
        os_actions::bluez::devices()
    }
    fn powered(&self) -> Result<bool, String> {
        crate::device_read::off_ui_thread();
        os_actions::bluez::is_powered()
    }
    fn scanning(&self) -> Result<bool, String> {
        crate::device_read::off_ui_thread();
        os_actions::bluez::is_scanning()
    }
    fn set_powered(&self, on: bool) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        os_actions::bluez::set_powered(on)
    }
    fn start_scan(&self) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        os_actions::bluez::start_scan()
    }
    fn stop_scan(&self) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        os_actions::bluez::stop_scan()
    }
    fn pair(&self, path: &str) -> Result<PairOutcome, String> {
        crate::device_read::off_ui_thread();
        os_actions::bluez::pair(path)
    }
    fn cancel_pairing(&self, path: &str) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        os_actions::bluez::cancel_pairing(path)
    }
    fn connect(&self, path: &str) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        os_actions::bluez::connect(path)
    }
    fn disconnect(&self, path: &str) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        os_actions::bluez::disconnect(path)
    }
    fn set_trusted(&self, path: &str, v: bool) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        os_actions::bluez::set_trusted(path, v)
    }
    fn set_alias(&self, path: &str, alias: &str) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        os_actions::bluez::set_alias(path, alias)
    }
    fn forget(&self, path: &str) -> Result<(), String> {
        crate::device_read::off_ui_thread();
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
    /// its first frame ([`crate::panes::LIVE_AREAS`]).
    pub(crate) fn is_open(&self) -> bool {
        self.model.get().is_some()
    }

    /// Whether this device has no Bluetooth adapter, as last read.
    fn adapter_absent(&self) -> bool {
        adapter_absent(&self.cx)
    }

    /// What this device cannot do with Bluetooth now: anything, without an
    /// adapter; switch the radio or look for devices, while the adapter
    /// cannot be read. Reads nothing itself: only what the radios and the
    /// area's model have read, so it never opens the area.
    pub(crate) fn statuses(&self) -> Vec<EffectStatus> {
        let unavailable = |effects: &[&'static str], code: &str, reason: &str| {
            effects
                .iter()
                .map(|&id| {
                    EffectStatus::unavailable(
                        ActionRef::<SettingsEffect, ()>::new(id, &[]),
                        code,
                        reason,
                    )
                })
                .collect()
        };
        if self.adapter_absent() {
            return unavailable(
                &[
                    BluetoothEffect::SET_BLUETOOTH.id,
                    BluetoothEffect::SET_DISCOVERING.id,
                    BluetoothEffect::REFRESH_BLUETOOTH.id,
                ],
                "no_adapter",
                &t("settings-bluetooth-no-adapter"),
            );
        }
        let unread = self
            .model
            .get()
            .and_then(|model| model.try_borrow().ok()?.read_error.clone());
        match unread {
            // Reading again is how a restarted `BlueZ` is found.
            Some(error) => unavailable(
                &[
                    BluetoothEffect::SET_BLUETOOTH.id,
                    BluetoothEffect::SET_DISCOVERING.id,
                ],
                "unreachable",
                &t_args("settings-bluetooth-read-error", &[("error", error.into())]),
            ),
            None => Vec::new(),
        }
    }

    /// Follow the adapter while the pane is not shown: one plugged in is
    /// read, so the rows published name its devices.
    pub(crate) fn poll(&self) {
        if let Some(model) = self.model.get()
            && let Ok(mut model) = model.try_borrow_mut()
        {
            model.follow_adapter();
        }
    }

    /// One row per device, offering what can be done to it now.
    pub(crate) fn rows(&self) -> Vec<LiveRow> {
        if self.adapter_absent() {
            // No adapter, no devices: the last read says nothing now.
            return Vec::new();
        }
        let model = self.model().borrow();
        if model.read_error.is_some() {
            // The last devices read say nothing about the adapter now.
            return Vec::new();
        }
        model
            .devices
            .iter()
            .map(|device| device_live_row(device, model.pairing.as_deref()))
            .collect()
    }
}

/// One device as a row of the published surface, offering what can be
/// done to it now; the pane's controls for that device bind to it.
fn device_live_row(device: &DeviceInfo, pairing: Option<&str>) -> LiveRow {
    let path = device.path.clone();
    let offer = |effect: BluetoothEffect| SettingsEffect::from(effect).bound();
    let mut row = LiveRow::new(
        format!("settings.bluetooth-device:{path}"),
        "settings.bluetooth-device",
        device.name.clone(),
    );
    if pairing == Some(path.as_str()) {
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
    .operation(offer(BluetoothEffect::RenameDevice {
        device: path.clone(),
        name: device.name.clone(),
    }))
    .operation(offer(BluetoothEffect::ForgetDevice { device: path }))
}

/// Whether `cx`'s device has no Bluetooth adapter, as its radios were last
/// read; `false` until they have been.
fn adapter_absent(cx: &PaneCx) -> bool {
    cx.radios().now().is_some_and(|radios| !radios.bluetooth)
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent state of the adapter or its reads, never a parameter"
)]
pub struct BluetoothModel {
    cx: PaneCx,
    control: Arc<dyn BluetoothControl>,
    /// Whether the device had an adapter when the model last looked.
    adapter: Option<bool>,
    /// The adapter, read again off the UI thread once one is plugged in
    /// or a change ran.
    reread: crate::device_read::DeviceRead<AdapterRead>,
    /// A change ran while [`Self::reread`] was on its way, so that read may
    /// not show it: the adapter is read once more when it lands.
    reread_again: bool,
    devices: Vec<DeviceInfo>,
    powered: bool,
    scanning: bool,
    /// Why the adapter could not be read. While set, the power switch, the
    /// scan state and the device list say nothing about the machine.
    read_error: Option<String>,
    notice: Option<String>,
    /// The device being paired, by object path.
    pairing: Option<String>,
    /// Which device's details are open, by address: kept by the model so a
    /// read of the devices never closes them.
    details: DisclosureGroup,
    /// When the adapter was last read, so discovery's finds are read in
    /// as they come.
    last_read: ui::time::Instant,
    changed: bool,
}

/// How often the adapter is read again while it looks for devices: what
/// discovery finds appears without a refresh.
const DISCOVERY_READ_INTERVAL: ui::time::Duration = ui::time::Duration::from_secs(2);

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
        let adapter = cx.radios().now().map(|radios| radios.bluetooth);
        let mut model = Self {
            cx,
            control,
            adapter,
            reread: crate::device_read::DeviceRead::default(),
            reread_again: false,
            devices: Vec::new(),
            powered: false,
            scanning: false,
            read_error: None,
            notice: None,
            pairing: None,
            details: DisclosureGroup::new(),
            last_read: ui::time::now(),
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

    /// Follow the device's adapter: show it gone when it is pulled out, and
    /// read it, off the UI thread, when one is plugged in.
    fn follow_adapter(&mut self) {
        let adapter = self.cx.radios().now().map(|radios| radios.bluetooth);
        if adapter != self.adapter {
            let plugged_in = adapter == Some(true);
            self.adapter = adapter;
            self.changed = true;
            if plugged_in {
                let control = Arc::clone(&self.control);
                if !self.reread.start(move || read_adapter_of(control.as_ref())) {
                    self.refresh();
                }
            }
        }
        self.adopt_reread();
        if self.reread_again && self.reread.is_finished() {
            self.reread_again = false;
            self.read_again();
        }
    }

    /// Take in the read [`Self::reread`] made, once it has landed.
    fn adopt_reread(&mut self) {
        if self.reread.is_finished()
            && let Some(read) = self.reread.take()
        {
            // A failure is kept on the model and shown in place of the adapter.
            let _ = self.adopt(read);
            self.changed = true;
        }
    }

    /// Read the adapter again off the UI thread, as a change it took left
    /// it; the reading lands at a later sync, which its landing wakes. A
    /// read already on its way may predate the change, so the adapter is
    /// read once more after it.
    fn read_again(&mut self) {
        if !self.reread.is_finished() {
            self.reread_again = true;
            return;
        }
        // Finished stays finished until the next start: a read that landed
        // is taken in here, so the start below is not refused for it.
        self.adopt_reread();
        let control = Arc::clone(&self.control);
        if !self.reread.start(move || read_adapter_of(control.as_ref())) {
            // A host that runs no background work reads here.
            self.refresh();
        }
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
                self.last_read = ui::time::now();
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

    /// The device's main button: its label and the action it takes.
    fn primary_action(
        &self,
        d: &DeviceInfo,
    ) -> (
        String,
        ui::tilo_resource::ActionRef<BluetoothEffect, String>,
    ) {
        if self.pairing.as_deref() == Some(d.path.as_str()) {
            (
                t("settings-bluetooth-cancel-pairing"),
                BluetoothEffect::CANCEL_PAIRING,
            )
        } else if !d.paired {
            (t("settings-bluetooth-pair"), BluetoothEffect::PAIR_DEVICE)
        } else if d.connected {
            (
                t("settings-bluetooth-disconnect"),
                BluetoothEffect::DISCONNECT_DEVICE,
            )
        } else {
            (
                t("settings-bluetooth-connect"),
                BluetoothEffect::CONNECT_DEVICE,
            )
        }
    }

    /// `control` acting on `d`'s row of the published surface through
    /// `effect`, or shown unavailable where the row does not offer it.
    fn on_row<I: 'static, C: ui::framework::ActionControl<I>>(
        &self,
        d: &DeviceInfo,
        control: C,
        effect: impl ui::tilo_resource::Binds<SettingsEffect, I>,
    ) -> C {
        let resource = device_live_row(d, self.pairing.as_deref()).into_resource();
        match self.cx.row_binding(&resource, effect) {
            Some(binding) => control.bind_action(binding),
            None => control.unavailable(),
        }
    }

    /// What opens under a paired device: its name, which can be changed in
    /// place, whether it may reconnect on its own, its address and signal,
    /// and forgetting it.
    fn device_details(&self, d: &DeviceInfo) -> Box<dyn Widget> {
        let path = d.path.clone();
        let mut items: Vec<Box<dyn Widget>> = vec![Box::new(
            ListRow::new(
                format!("settings.bluetooth.name.{}", token(&d.path)),
                t("settings-bluetooth-name"),
            )
            .trailing(Box::new(
                self.on_row(
                    d,
                    EditableLabel::new(&d.name)
                        .with_id(&format!("bluetooth.alias.{}", token(&d.path)))
                        .with_accessible_name(t("settings-bluetooth-name"))
                        .edits_on_click(),
                    effect(BluetoothEffect::RENAME_DEVICE)
                        .map(move |name: String| (path.clone(), name.trim().to_owned())),
                ),
            )),
        )];
        let path = d.path.clone();
        items.push(Box::new(
            ListRow::new(
                format!("settings.bluetooth.trust.{}", token(&d.path)),
                t("settings-bluetooth-trust"),
            )
            .secondary(t("settings-bluetooth-trust-caption"))
            .trailing(Box::new(
                self.on_row(
                    d,
                    Toggle::from_state(&LocalState::new(d.trusted))
                        .with_id(&format!("settings.bluetooth.trusted.{}", token(&d.path))),
                    effect(BluetoothEffect::SET_DEVICE_TRUSTED)
                        .map(move |trusted: bool| (path.clone(), trusted)),
                ),
            )),
        ));
        items.push(Box::new(
            ListRow::new(
                format!("settings.bluetooth.address.{}", token(&d.path)),
                t("settings-bluetooth-address"),
            )
            .value(d.address.clone()),
        ));
        if let Some(rssi) = d.rssi {
            items.push(Box::new(
                ListRow::new(
                    format!("settings.bluetooth.signal.{}", token(&d.path)),
                    t("settings-bluetooth-signal"),
                )
                .value(t_args(
                    "settings-bluetooth-signal-dbm",
                    &[("rssi", ArgValue::Number(f64::from(rssi)))],
                )),
            ));
        }
        if d.icon.starts_with("audio-") {
            items.push(crate::widgets::caption(&t(
                "settings-bluetooth-audio-caption",
            )));
        }
        let path = d.path.clone();
        items.push(Box::new(
            Row::new().push(Box::new(
                self.on_row(
                    d,
                    Button::new(
                        &format!("settings.bluetooth.forget.{}", token(&d.path)),
                        &t("settings-bluetooth-forget"),
                    )
                    .role(ButtonRole::Destructive),
                    effect(BluetoothEffect::FORGET_DEVICE).map(move |()| path.clone()),
                ),
            )),
        ));
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_extra_bottom_padding(crate::widgets::spacing().rhythm.sibling)
                .push(Box::new(ui::widget::Section::new(items))),
        )
    }

    /// The device's main button (pair, connect, disconnect), acting on its
    /// row of the published surface.
    fn primary_button(&self, d: &DeviceInfo) -> Button {
        let (label, primary) = self.primary_action(d);
        let path = d.path.clone();
        self.on_row(
            d,
            Button::new(
                &format!("settings.bluetooth.primary.{}", token(&d.path)),
                &label,
            )
            .control_size(ui::widget::ControlSize::Small),
            effect(primary).map(move |()| path.clone()),
        )
    }

    /// A paired device: its name and state, its main button, and its
    /// details opening in place. The row is keyed by the device's address,
    /// so it stays open while discovery reads the devices again.
    fn paired_row(&self, d: &DeviceInfo) -> Box<dyn Widget> {
        Box::new(
            DisclosureRow::keyed(
                d.name.clone(),
                self.device_details(d),
                &self.details,
                &d.address,
            )
            .icon(device_icon(&d.icon))
            .secondary(Self::device_status_line(d))
            .trailing(Box::new(self.primary_button(d))),
        )
    }

    /// A device nearby, not paired: its name and Pair.
    fn nearby_row(&self, d: &DeviceInfo) -> Box<dyn Widget> {
        Box::new(
            ListRow::new(
                format!("settings.bluetooth.device.{}", token(&d.path)),
                d.name.clone(),
            )
            .icon(device_icon(&d.icon))
            .secondary(Self::device_status_line(d))
            .trailing(Box::new(self.primary_button(d))),
        )
    }

    /// The radio's switch: on, off, or unavailable while it cannot be read
    /// or there is no adapter.
    fn radio_row(&self, on: bool, enabled: bool, detail: Option<&str>) -> Item {
        let mut radio = ListRow::new("settings.bluetooth.radio", t("settings-bluetooth-card"))
            .trailing(Box::new(
                self.cx.bind(
                    Toggle::from_state(&LocalState::new(on))
                        .with_id("bluetooth.toggle")
                        .with_enabled(enabled),
                    effect(BluetoothEffect::SET_BLUETOOTH),
                ),
            ));
        if let Some(detail) = detail {
            radio = radio.secondary(detail);
        }
        row(Box::new(radio))
    }

    /// The pane of a device without an adapter: the switch off and
    /// disabled, and why.
    fn without_adapter(&self) -> Box<dyn Widget> {
        crate::widgets::page(vec![untitled(rows(vec![self.radio_row(
            false,
            false,
            Some(&t("settings-bluetooth-no-adapter")),
        )]))])
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        if adapter_absent(&self.cx) {
            return self.without_adapter();
        }
        // An unread power state is not offered as a switch position.
        let mut radio = vec![self.radio_row(self.powered, self.read_error.is_none(), None)];
        if let Some(notice) = &self.notice {
            radio.push(note(notice));
        }
        if let Some(error) = &self.read_error {
            radio.push(block(error_caption(&t_args(
                "settings-bluetooth-read-error",
                &[("error", error.as_str().into())],
            ))));
            return crate::widgets::page(vec![untitled(rows(radio))]);
        }
        let mut sections = vec![untitled(rows(radio))];
        let addresses: Vec<&str> = self
            .devices
            .iter()
            .filter(|d| d.paired)
            .map(|d| d.address.as_str())
            .collect();
        self.details.retain(&addresses);
        let paired: Vec<Item> = self
            .devices
            .iter()
            .filter(|d| d.paired)
            .map(|d| row(self.paired_row(d)))
            .collect();
        // The person's devices stay listed while the radio is off; what is
        // nearby is only known while it is on.
        if !paired.is_empty() {
            sections.push(section(&t("settings-bluetooth-my-devices"), rows(paired)));
        }
        if !self.powered {
            return crate::widgets::page(sections);
        }
        let mut nearby: Vec<Item> = self
            .devices
            .iter()
            .filter(|d| !d.paired)
            .map(|d| row(self.nearby_row(d)))
            .collect();
        if nearby.is_empty() {
            nearby.push(note(&t(if self.scanning {
                "settings-bluetooth-looking"
            } else {
                "settings-bluetooth-empty"
            })));
        }
        // What discovery finds is read in as it comes: no refresh.
        let scan = t(if self.scanning {
            "settings-bluetooth-stop"
        } else {
            "settings-bluetooth-find"
        });
        sections.push(section_action(
            &t("settings-bluetooth-nearby-devices"),
            self.cx.bind_invocation(
                Button::new("settings.bluetooth.scan", &scan),
                BluetoothEffect::SetDiscovering { on: !self.scanning },
            ),
            rows(nearby),
        ));
        crate::widgets::page(sections)
    }
}

impl PaneModel for BluetoothModel {
    fn compose(&self) -> Box<dyn Widget> {
        self.build_composition()
    }

    fn poll(&mut self) -> bool {
        self.follow_adapter();
        if self.scanning && ui::time::now() >= self.last_read + DISCOVERY_READ_INTERVAL {
            self.last_read = ui::time::now();
            self.read_again();
        }
        std::mem::take(&mut self.changed)
    }

    fn name(&self) -> &'static str {
        "BluetoothPane"
    }

    fn next_wake(&self) -> Option<ui::time::Instant> {
        self.scanning
            .then(|| self.last_read + DISCOVERY_READ_INTERVAL)
    }
}

/// The icon for a device from the kind BlueZ names (freedesktop icon
/// names: `audio-headset`, `input-keyboard`, `phone`…).
fn device_icon(kind: &str) -> ui::icons::Icon {
    use ui::icons::Icon;
    match kind {
        kind if kind.starts_with("audio-head") => Icon::Headphones,
        kind if kind.starts_with("audio") => Icon::Speaker,
        "input-keyboard" => Icon::Keyboard,
        "input-mouse" | "input-tablet" => Icon::Mouse,
        "phone" => Icon::Phone,
        "computer" => Icon::Monitor,
        _ => Icon::Bluetooth,
    }
}

/// The devices a screenshot and the crate's tests show: headphones paired
/// and a keyboard nearby. Every change succeeds and changes nothing.
struct SampleBluetooth;

impl BluetoothControl for SampleBluetooth {
    fn devices(&self) -> Result<Vec<DeviceInfo>, String> {
        crate::device_read::off_ui_thread();
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
        crate::device_read::off_ui_thread();
        Ok(true)
    }
    fn scanning(&self) -> Result<bool, String> {
        crate::device_read::off_ui_thread();
        Ok(false)
    }
    fn set_powered(&self, _: bool) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        Ok(())
    }
    fn start_scan(&self) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        Ok(())
    }
    fn stop_scan(&self) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        Ok(())
    }
    fn pair(&self, _: &str) -> Result<PairOutcome, String> {
        crate::device_read::off_ui_thread();
        Ok(PairOutcome::PairedAndConnected)
    }
    fn cancel_pairing(&self, _: &str) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        Ok(())
    }
    fn connect(&self, _: &str) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        Ok(())
    }
    fn disconnect(&self, _: &str) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        Ok(())
    }
    fn set_trusted(&self, _: &str, _: bool) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        Ok(())
    }
    fn set_alias(&self, _: &str, _: &str) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        Ok(())
    }
    fn forget(&self, _: &str) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

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

    /// The open disclosure rows in `widget`'s tree, by title.
    fn open_rows(widget: &dyn Widget, out: &mut Vec<String>) {
        if let Some(row) = widget
            .as_any()
            .and_then(|any| any.downcast_ref::<DisclosureRow>())
            && row.is_open()
        {
            out.extend(widget.accessibility().and_then(|node| node.name));
        }
        for child in widget.children() {
            open_rows(child, out);
        }
    }

    /// A paired device's details open in place, and stay open while
    /// discovery reads the devices again every two seconds and the page is
    /// rebuilt from the new reading.
    #[test]
    fn a_devices_details_stay_open_while_discovery_reads_the_devices_again() {
        let _settled = ui::motion::testing::settled();
        let profile = tempfile::tempdir().unwrap();
        let cx = PaneCx::in_file(profile.path().join("settings.json"));
        let control: Arc<dyn BluetoothControl> = Arc::new(Mock {
            calls: Mutex::new(vec![]),
            devices: vec![dev()],
        });
        let read = read_adapter_of(control.as_ref());
        let mut model = BluetoothModel::with_read(cx, control, read);
        let mut opened = Vec::new();
        open_rows(model.build_composition().as_ref(), &mut opened);
        assert!(opened.is_empty(), "closed until opened: {opened:?}");

        // The person opens Headphones' details.
        DisclosureRow::keyed("Headphones", Box::new(Row::new()), &model.details, "AA")
            .set_open(true);
        for _ in 0..3 {
            model.refresh();
            let mut opened = Vec::new();
            open_rows(model.build_composition().as_ref(), &mut opened);
            assert_eq!(opened.len(), 1, "still open after a read: {opened:?}");
            assert!(opened[0].starts_with("Headphones"), "{opened:?}");
        }
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

    /// The availability Settings publishes for each of `ids`, as agents,
    /// the palette and assistive technology read it.
    fn published(
        app: &mut ui::prelude::TestApp<crate::resource::SettingsShell>,
        ids: &[&str],
    ) -> Vec<(String, ui::tilo_actions::Availability)> {
        let actions = app.actions().expect("the actions are published");
        ids.iter()
            .map(|&id| {
                let action = actions
                    .iter()
                    .find(|action| action.address.action == id)
                    .unwrap_or_else(|| panic!("`{id}` is published"));
                (id.to_owned(), action.contract.availability.clone())
            })
            .collect()
    }

    /// The radio's own effects.
    const RADIO: [&str; 3] = ["set-bluetooth", "set-discovering", "refresh-bluetooth"];

    /// A device without an adapter publishes the radio's effects
    /// unavailable with why, offers no devices and says so in the pane; an
    /// adapter plugged in later makes them available again, and with one
    /// nothing changes.
    #[test]
    fn without_an_adapter_the_radio_is_unavailable_until_one_is_plugged_in() {
        use crate::radios::Present;
        use crate::resource::SettingsShell;
        use ui::prelude::TestApp;
        use ui::tilo_actions::Availability;

        let mut app = TestApp::<SettingsShell>::new().unwrap();
        let radios = app.surface_mut::<SettingsApp>().host().radios().clone();
        let device_rows = |app: &mut TestApp<SettingsShell>| {
            app.surface::<SettingsApp, _>(|settings| {
                crate::panes::bluetooth::area(settings)
                    .unwrap()
                    .rows()
                    .len()
            })
            .unwrap()
        };
        let pane = |app: &mut TestApp<SettingsShell>| {
            app.surface::<SettingsApp, _>(|settings| {
                let model = crate::panes::bluetooth::area(settings)
                    .unwrap()
                    .model()
                    .borrow();
                crate::widgets::announced_text(model.build_composition().as_ref())
            })
            .unwrap()
        };

        radios.set(Present::ALL);
        app.settle();
        assert!(
            published(&mut app, &RADIO)
                .iter()
                .all(|(_, availability)| *availability == Availability::Available),
            "with an adapter the radio is offered"
        );
        assert_eq!(
            device_rows(&mut app),
            2,
            "with an adapter its devices are rows"
        );

        radios.set(Present {
            bluetooth: false,
            ..Present::ALL
        });
        app.settle();
        for (id, availability) in published(&mut app, &RADIO) {
            assert_eq!(
                availability,
                Availability::Unavailable {
                    code: "no_adapter".into(),
                    reason: "No Bluetooth adapter on this device".into(),
                },
                "`{id}` without an adapter"
            );
        }
        assert_eq!(device_rows(&mut app), 0, "no adapter, no devices");
        let text = pane(&mut app);
        assert!(
            text.iter()
                .any(|line| line == "No Bluetooth adapter on this device"),
            "{text:#?}"
        );
        assert!(
            !text.iter().any(|line| line.contains("Headphones")),
            "{text:#?}"
        );
        app.effect(&SettingsEffect::from(BluetoothEffect::SetBluetooth {
            on: true,
        }))
        .expect_err("an unavailable radio is refused, not tried");

        radios.set(Present::ALL);
        app.settle();
        assert!(
            published(&mut app, &RADIO)
                .iter()
                .all(|(_, availability)| *availability == Availability::Available),
            "an adapter plugged in is offered without a restart"
        );
        assert_eq!(device_rows(&mut app), 2);
        let text = pane(&mut app);
        assert!(
            text.iter().any(|line| line.contains("Headphones")),
            "{text:#?}"
        );
    }

    /// An adapter whose reads do not answer until they are let go, as a
    /// wedged `BlueZ`; changes it takes at once. Once let go it knows a
    /// radio on the porch.
    struct StalledAdapter(Arc<crate::stall::Stall>);

    impl StalledAdapter {
        fn wait(&self) -> Result<(), String> {
            crate::device_read::off_ui_thread();
            if self.0.wait() {
                Ok(())
            } else {
                Err("BlueZ never answered".into())
            }
        }
    }

    impl BluetoothControl for StalledAdapter {
        fn devices(&self) -> Result<Vec<DeviceInfo>, String> {
            self.wait()?;
            Ok(vec![DeviceInfo {
                path: "/org/bluez/hci0/dev_SPEAKER".into(),
                name: "Porch Radio".into(),
                ..dev()
            }])
        }
        fn powered(&self) -> Result<bool, String> {
            self.wait().map(|()| false)
        }
        fn scanning(&self) -> Result<bool, String> {
            self.wait().map(|()| false)
        }
        fn set_powered(&self, _: bool) -> Result<(), String> {
            crate::device_read::off_ui_thread();
            Ok(())
        }
        fn start_scan(&self) -> Result<(), String> {
            crate::device_read::off_ui_thread();
            Ok(())
        }
        fn stop_scan(&self) -> Result<(), String> {
            crate::device_read::off_ui_thread();
            Ok(())
        }
        fn pair(&self, _: &str) -> Result<PairOutcome, String> {
            crate::device_read::off_ui_thread();
            Ok(PairOutcome::PairedAndConnected)
        }
        fn cancel_pairing(&self, _: &str) -> Result<(), String> {
            crate::device_read::off_ui_thread();
            Ok(())
        }
        fn connect(&self, _: &str) -> Result<(), String> {
            crate::device_read::off_ui_thread();
            Ok(())
        }
        fn disconnect(&self, _: &str) -> Result<(), String> {
            crate::device_read::off_ui_thread();
            Ok(())
        }
        fn set_trusted(&self, _: &str, _: bool) -> Result<(), String> {
            crate::device_read::off_ui_thread();
            Ok(())
        }
        fn set_alias(&self, _: &str, _: &str) -> Result<(), String> {
            crate::device_read::off_ui_thread();
            Ok(())
        }
        fn forget(&self, _: &str) -> Result<(), String> {
            crate::device_read::off_ui_thread();
            Ok(())
        }
    }

    /// A change is answered and the window keeps drawing while `BlueZ`
    /// takes its time to be read again: the adapter is re-read off the UI
    /// thread, and the reading lands once it returns.
    #[test]
    fn a_stalled_adapter_never_holds_the_window() {
        use crate::resource::SettingsShell;
        use ui::prelude::TestApp;

        let mut app = TestApp::<SettingsShell>::new().unwrap();
        crate::effects::show_pane(&mut app, crate::categories::Category::Bluetooth);
        let gate = crate::stall::Stall::new();
        let stalled = Arc::clone(&gate);
        app.with_widget::<SettingsApp, _>(move |settings| {
            crate::panes::bluetooth::area(settings)
                .unwrap()
                .model()
                .borrow_mut()
                .control = Arc::new(StalledAdapter(stalled));
        })
        .expect("the Settings window is mounted");

        app.effect(&SettingsEffect::from(BluetoothEffect::SetBluetooth {
            on: false,
        }))
        .expect("the radio is switched off");
        app.harness()
            .poll_frame()
            .expect("the window draws its next frame");
        gate.assert_held("the click");
        let frame = app.harness().inspection_json().unwrap();
        assert!(frame.contains(&t("settings-bluetooth-off")), "{frame}");
        assert!(
            frame.contains("Headphones"),
            "the last reading is shown: {frame}"
        );
        assert!(!frame.contains("Porch Radio"), "{frame}");

        gate.release();
        assert!(
            app.harness()
                .wait_until(tilo_test_env::hang_bound(), |harness| harness
                    .inspection_json()
                    .is_ok_and(|frame| frame.contains("Porch Radio"))),
            "the devices read again are shown"
        );
    }

    /// The radio's availability is read without opening the area: its
    /// adapter is read only when its pane, a row or an effect needs it.
    #[test]
    fn availability_never_reads_the_adapter() {
        use crate::radios::Present;

        let profile = tempfile::tempdir().unwrap();
        let cx = PaneCx::in_file(profile.path().join("settings.json"));
        let area = BluetoothArea::new(&cx);
        assert!(
            area.statuses().is_empty(),
            "radios not read offer everything"
        );
        cx.radios().set(Present::NONE);
        assert_eq!(area.statuses().len(), RADIO.len());
        assert!(!area.is_open(), "the adapter was not read");
    }
}
