#![allow(
    clippy::doc_markdown,
    clippy::expect_used,
    clippy::items_after_statements,
    clippy::needless_pass_by_value,
    reason = "BlueZ is the upstream service name and the pane consumes a closed typed command/resource contract"
)]
//! Bluetooth settings backed directly by `BlueZ` over D-Bus. Settings starts
//! operations; the compositor-owned `BlueZ` Agent owns security prompts.

use ui::i18n::ArgValue;

use crate::i18n::{t, t_args};
use crate::widgets::{El, caption, card_with_action, rows, setting_row, value_row};
use automerge::{AutoCommit, ObjId, transaction::Transactable};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use taffy::{NodeId, TaffyTree};
use tilo_bluez::{DeviceInfo, PairOutcome};
use ui::doc::{read_bool, read_str_or_text};
use ui::theme::Theme;
use ui::widget::{AppModel, ModelPath};
use ui::widget::{Button, Column, Row, Spacer, Text, TextInput, Toggle, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

pub const BT_ENABLED_KEY: &str = "bluetooth.enabled";
pub const SCAN_KEY: &str = "bluetooth.scan";
pub const REFRESH_KEY: &str = "bluetooth.refresh";
fn token(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).replace('.', "_")
}
pub(crate) fn device_action_key(path: &str, action: &str) -> String {
    format!("bluetooth.device.{}.{}", token(path), action)
}
fn key(path: &str, action: &str) -> String {
    device_action_key(path, action)
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

#[allow(
    clippy::struct_excessive_bools,
    reason = "four independent lifecycle flags on a live pane (hardware powered, currently \
              scanning, needs its initial CRDT seed, layout needs a rebuild) — not a \
              flag-combination API, so an enum/bitflags would add indirection without \
              reducing complexity"
)]
pub struct BluetoothPane {
    control: Arc<dyn BluetoothControl>,
    devices: Vec<DeviceInfo>,
    powered: bool,
    scanning: bool,
    notice: Option<String>,
    expanded: Option<String>,
    confirm_forget: Option<String>,
    pending_pair: Option<String>,
    pair_result: Option<Receiver<(String, Result<PairOutcome, String>)>>,
    operation_status: ui::LocalState<crate::actions::LocalOperationStatus>,
    command_state: ui::LocalState<Option<crate::actions::LocalPaneCommand>>,
    local_doc: AutoCommit,
    isolate_document: bool,
    require_operation_token: bool,
    needs_seed: bool,
    structure_dirty: bool,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}
impl Default for BluetoothPane {
    fn default() -> Self {
        Self::new()
    }
}
impl BluetoothPane {
    pub fn new() -> Self {
        let c: Arc<dyn BluetoothControl> = Arc::new(RealBluetooth);
        let devices = c.devices().unwrap_or_default();
        let powered = c.powered().unwrap_or(false);
        let scanning = c.scanning().unwrap_or(false);
        Self::with_control(c, devices, powered, scanning)
    }
    pub fn with_control(
        control: Arc<dyn BluetoothControl>,
        devices: Vec<DeviceInfo>,
        powered: bool,
        scanning: bool,
    ) -> Self {
        Self {
            control,
            devices,
            powered,
            scanning,
            notice: None,
            expanded: None,
            confirm_forget: None,
            pending_pair: None,
            pair_result: None,
            operation_status: ui::LocalState::new(crate::actions::LocalOperationStatus::Idle),
            command_state: ui::LocalState::new(None),
            local_doc: AutoCommit::new(),
            isolate_document: false,
            require_operation_token: false,
            needs_seed: true,
            structure_dirty: false,
            composed: None,
            rect: Rect::ZERO,
        }
    }
    pub fn devices(&self) -> &[DeviceInfo] {
        &self.devices
    }
    #[must_use]
    pub fn with_operation_status(
        mut self,
        status: &ui::LocalState<crate::actions::LocalOperationStatus>,
    ) -> Self {
        self.operation_status = status.clone();
        self.require_operation_token = true;
        self
    }
    #[must_use]
    pub fn with_command_state(
        mut self,
        commands: &ui::LocalState<Option<crate::actions::LocalPaneCommand>>,
    ) -> Self {
        self.command_state = commands.clone();
        self.isolate_document = true;
        self
    }
    fn execute_local_command(
        &mut self,
        local: &mut AutoCommit,
        operation: crate::actions::BluetoothOperation,
    ) {
        let root = automerge::ROOT;
        use crate::actions::BluetoothOperation as Op;
        match &operation {
            Op::SetPowered { enabled } => {
                let _ = local.put(&root, BT_ENABLED_KEY, *enabled);
            }
            Op::StartScan => {
                let _ = local.put(&root, SCAN_KEY, true);
            }
            Op::Refresh => {
                let _ = local.put(&root, REFRESH_KEY, true);
            }
            Op::Pair { path } => {
                let _ = local.put(&root, key(path, "pair"), true);
            }
            Op::Connect { path } => {
                let _ = local.put(&root, key(path, "connect"), true);
            }
            Op::Disconnect { path } => {
                let _ = local.put(&root, key(path, "disconnect"), true);
            }
            Op::Forget { path } => {
                let _ = local.put(&root, key(path, "forget"), true);
            }
        }
        self.sync_power(local, &root);
        self.sync_scan(local, &root);
        self.sync_refresh(local, &root);
        self.sync_devices(local, &root);
    }
    #[allow(
        clippy::needless_pass_by_value,
        reason = "callers construct the exact typed request whose token is completed"
    )]
    fn finish_local_operation(
        &self,
        operation: crate::actions::BluetoothOperation,
        result: Result<String, String>,
    ) {
        let token = crate::actions::local_operation_token_for(
            crate::actions::BLUETOOTH_OPERATION_TYPE,
            &operation,
        );
        if !matches!(
            self.operation_status.get(),
            crate::actions::LocalOperationStatus::Running { token: running } if running == token
        ) {
            return;
        }
        self.operation_status.set(match result {
            Ok(message) => crate::actions::LocalOperationStatus::Succeeded { token, message },
            Err(message) => crate::actions::LocalOperationStatus::Failed { token, message },
        });
    }
    fn operation_is_running(&self, operation: &crate::actions::BluetoothOperation) -> bool {
        if !self.require_operation_token {
            return true;
        }
        let token = crate::actions::local_operation_token_for(
            crate::actions::BLUETOOTH_OPERATION_TYPE,
            operation,
        );
        matches!(
            self.operation_status.get(),
            crate::actions::LocalOperationStatus::Running { token: running } if running == token
        )
    }
    fn has_pending_operation(&self) -> bool {
        if !self.require_operation_token {
            return true;
        }
        matches!(
            self.operation_status.get(),
            crate::actions::LocalOperationStatus::Running { token }
                if token.starts_with(crate::actions::BLUETOOTH_OPERATION_TYPE)
        )
    }
    fn refresh(&mut self) {
        match self.control.devices() {
            Ok(v) => self.devices = v,
            Err(e) => {
                self.notice = Some(t_args(
                    "settings-bluetooth-refresh-failed",
                    &[("error", e.into())],
                ));
            }
        }
        self.scanning = self.control.scanning().unwrap_or(self.scanning);
        self.powered = self.control.powered().unwrap_or(self.powered);
        self.structure_dirty = true;
    }
    fn device_primary_action(&self, d: &DeviceInfo) -> (String, &'static str) {
        let pairing = self.pending_pair.as_deref() == Some(&d.path);
        if pairing {
            (t("settings-bluetooth-cancel-pairing"), "cancel")
        } else if !d.paired {
            (t("settings-bluetooth-pair"), "pair")
        } else if d.connected {
            (t("settings-bluetooth-disconnect"), "disconnect")
        } else {
            (t("settings-bluetooth-connect"), "connect")
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
    fn device_top_row(&self, d: &DeviceInfo) -> Row {
        let (primary, action) = self.device_primary_action(d);
        let details = t(if self.expanded.as_deref() == Some(&d.path) {
            "settings-bluetooth-less"
        } else {
            "settings-bluetooth-details"
        });
        Row::new()
            .push(Box::new(Text::new(&d.name).with_size(13.0)))
            .push(Box::new(Spacer::new()))
            .push(Box::new(
                Button::new(
                    &format!("settings.bluetooth.{action}.{}", token(&d.path)),
                    &primary,
                    Some(&key(&d.path, action)),
                )
                .content_sized(),
            ))
            .push(Box::new(
                Button::new(
                    &format!("settings.bluetooth.details.{}", token(&d.path)),
                    &details,
                    Some(&key(&d.path, "details")),
                )
                .content_sized(),
            ))
    }
    fn device_detail_rows(&self, d: &DeviceInfo) -> Vec<El> {
        let mut block: Vec<El> = Vec::new();
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
            let rename = t("settings-bluetooth-rename");
            let forget = t(if self.confirm_forget.as_deref() == Some(&d.path) {
                "settings-bluetooth-forget-confirm"
            } else {
                "settings-bluetooth-forget"
            });
            block.push(setting_row(
                &t("settings-bluetooth-trust"),
                Box::new(Toggle::new(
                    &format!("settings.bluetooth.trusted.{}", token(&d.path)),
                    &key(&d.path, "trusted"),
                )),
            ));
            block.push(caption(&t("settings-bluetooth-trust-caption")));
            block.push(Box::new(
                TextInput::new(Vec::new())
                    .with_id(&format!("bluetooth.alias.{}", token(&d.path)))
                    .with_on_change_path(&key(&d.path, "alias"))
                    .with_placeholder(&d.name),
            ));
            block.push(Box::new(
                Row::new()
                    .push(Box::new(
                        Button::new(
                            &format!("settings.bluetooth.rename.{}", token(&d.path)),
                            &rename,
                            Some(&key(&d.path, "rename")),
                        )
                        .content_sized(),
                    ))
                    .push(Box::new(
                        Button::new(
                            &format!("settings.bluetooth.forget.{}", token(&d.path)),
                            &forget,
                            Some(&key(&d.path, "forget")),
                        )
                        .content_sized(),
                    ))
                    .push(Box::new(Spacer::new())),
            ));
        }
        if d.icon.starts_with("audio-") {
            block.push(caption(&t("settings-bluetooth-audio-caption")));
        }
        block
    }
    fn device_row(&self, d: &DeviceInfo) -> El {
        let top = self.device_top_row(d);
        let status = Self::device_status_line(d);
        let mut block: Vec<El> = vec![Box::new(top), caption(&status)];
        if self.expanded.as_deref() == Some(&d.path) {
            block.extend(self.device_detail_rows(d));
        }
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .push(rows(block)),
        )
    }
    fn device_rows(&self) -> Vec<El> {
        let mut out: Vec<El> = self.devices.iter().map(|d| self.device_row(d)).collect();
        if out.is_empty() {
            out.push(caption(&t(if self.scanning {
                "settings-bluetooth-looking"
            } else {
                "settings-bluetooth-empty"
            })));
        }
        out
    }
    fn build_composition(&self) -> Box<dyn Widget> {
        let scan = t(if self.scanning {
            "settings-bluetooth-stop"
        } else {
            "settings-bluetooth-find"
        });
        let refresh = t("settings-bluetooth-refresh");
        let mut body: Vec<El> = vec![Box::new(
            Row::new()
                .push(Box::new(
                    Button::new("settings.bluetooth.scan", &scan, Some(SCAN_KEY)).content_sized(),
                ))
                .push(Box::new(
                    Button::new("settings.bluetooth.refresh", &refresh, Some(REFRESH_KEY))
                        .content_sized(),
                ))
                .push(Box::new(Spacer::new())),
        )];
        if let Some(n) = &self.notice {
            body.push(caption(n));
        }
        body.extend(self.device_rows());
        crate::widgets::cards(vec![card_with_action(
            &t("settings-bluetooth-card"),
            Box::new(
                Toggle::new("settings.bluetooth.toggle", BT_ENABLED_KEY)
                    .with_id("bluetooth.toggle")
                    .on_action(
                        crate::actions::bluetooth_bool_binding("bluetooth").expect(
                            "Bluetooth power action is part of the static resource contract",
                        ),
                    ),
            ),
            rows(body),
        )])
    }
    fn act(&mut self, state: &mut AutoCommit, root: &ObjId, d: &DeviceInfo, action: &str) {
        let k = key(&d.path, action);
        if !read_bool(state, root, &k).unwrap_or(false) {
            return;
        }
        let _ = state.put(root, &k, false);
        let result = match action {
            "connect" => self.control.connect(&d.path).map(|()| {
                t_args(
                    "settings-bluetooth-connected-name",
                    &[("name", d.name.clone().into())],
                )
            }),
            "disconnect" => self.control.disconnect(&d.path).map(|()| {
                t_args(
                    "settings-bluetooth-disconnected-name",
                    &[("name", d.name.clone().into())],
                )
            }),
            _ => return,
        };
        let result = result.map_err(|e| {
            t_args(
                if action == "connect" {
                    "settings-bluetooth-connect-failed"
                } else {
                    "settings-bluetooth-disconnect-failed"
                },
                &[("name", d.name.clone().into()), ("error", e.into())],
            )
        });
        self.notice = Some(result.clone().unwrap_or_else(|error| error));
        self.finish_local_operation(
            if action == "connect" {
                crate::actions::BluetoothOperation::Connect {
                    path: d.path.clone(),
                }
            } else {
                crate::actions::BluetoothOperation::Disconnect {
                    path: d.path.clone(),
                }
            },
            result,
        );
        self.refresh();
    }
    fn sync_pair_result(&mut self) {
        if let Some(rx) = &self.pair_result
            && let Ok((path, result)) = rx.try_recv()
        {
            let name = self
                .devices
                .iter()
                .find(|d| d.path == path)
                .map_or_else(|| t("settings-bluetooth-device"), |d| d.name.clone());
            let result = match result {
                Ok(PairOutcome::PairedAndConnected) => Ok(t_args(
                    "settings-bluetooth-paired-connected",
                    &[("name", name.clone().into())],
                )),
                Ok(PairOutcome::PairedNotConnected { reason }) => Ok(t_args(
                    "settings-bluetooth-paired-not-connected",
                    &[("name", name.clone().into()), ("reason", reason.into())],
                )),
                Err(error) => Err(t_args(
                    "settings-bluetooth-pair-failed",
                    &[("name", name.clone().into()), ("error", error.into())],
                )),
            };
            self.notice = Some(result.clone().unwrap_or_else(|error| error));
            self.finish_local_operation(crate::actions::BluetoothOperation::Pair { path }, result);
            self.pending_pair = None;
            self.pair_result = None;
            self.refresh();
        }
    }
    fn seed_if_needed(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if !self.needs_seed {
            return;
        }
        let _ = state.put(root, BT_ENABLED_KEY, self.powered);
        let _ = state.put(root, SCAN_KEY, false);
        let _ = state.put(root, REFRESH_KEY, false);
        for d in &self.devices {
            for a in [
                "pair",
                "connect",
                "cancel",
                "disconnect",
                "details",
                "rename",
                "forget",
            ] {
                let _ = state.put(root, key(&d.path, a), false);
            }
            let _ = state.put(root, key(&d.path, "trusted"), d.trusted);
            let _ = crate::ensure_text_object(state, root, &key(&d.path, "alias"));
        }
        self.needs_seed = false;
    }
    fn sync_power(&mut self, state: &mut AutoCommit, root: &ObjId) {
        let Some(on) = read_bool(state, root, BT_ENABLED_KEY) else {
            return;
        };
        if on == self.powered {
            return;
        }
        let operation = crate::actions::BluetoothOperation::SetPowered { enabled: on };
        if !self.operation_is_running(&operation) {
            let _ = state.put(root, BT_ENABLED_KEY, self.powered);
            return;
        }
        match self.control.set_powered(on) {
            Ok(()) => {
                self.powered = on;
                self.notice = Some(if on {
                    t("settings-bluetooth-on")
                } else {
                    t("settings-bluetooth-off")
                });
                self.finish_local_operation(
                    operation,
                    Ok(if on {
                        t("settings-bluetooth-enabled")
                    } else {
                        t("settings-bluetooth-disabled")
                    }),
                );
            }
            Err(e) => {
                self.notice = Some(t_args(
                    "settings-bluetooth-power-failed",
                    &[("error", e.clone().into())],
                ));
                let _ = state.put(root, BT_ENABLED_KEY, self.powered);
                self.finish_local_operation(
                    operation,
                    Err(t_args(
                        "settings-bluetooth-power-failed",
                        &[("error", e.into())],
                    )),
                );
            }
        }
        self.structure_dirty = true;
    }
    fn sync_scan(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if !read_bool(state, root, SCAN_KEY).unwrap_or(false) {
            return;
        }
        let _ = state.put(root, SCAN_KEY, false);
        let r = if self.scanning {
            self.control.stop_scan()
        } else {
            self.control.start_scan()
        };
        match r {
            Ok(()) => {
                self.scanning = !self.scanning;
                self.notice = Some(if self.scanning {
                    t("settings-bluetooth-discovery-started-notice")
                } else {
                    t("settings-bluetooth-discovery-stopped-notice")
                });
                self.finish_local_operation(
                    crate::actions::BluetoothOperation::StartScan,
                    Ok(if self.scanning {
                        t("settings-bluetooth-discovery-started")
                    } else {
                        t("settings-bluetooth-discovery-stopped")
                    }),
                );
            }
            Err(e) => {
                self.notice = Some(t_args(
                    "settings-bluetooth-discovery-failed",
                    &[("error", e.clone().into())],
                ));
                self.finish_local_operation(
                    crate::actions::BluetoothOperation::StartScan,
                    Err(t_args(
                        "settings-bluetooth-discovery-failed-op",
                        &[("error", e.into())],
                    )),
                );
            }
        }
        self.refresh();
    }
    fn sync_refresh(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if read_bool(state, root, REFRESH_KEY).unwrap_or(false) {
            let _ = state.put(root, REFRESH_KEY, false);
            match self.control.devices() {
                Ok(devices) => {
                    self.devices = devices;
                    self.scanning = self.control.scanning().unwrap_or(self.scanning);
                    self.powered = self.control.powered().unwrap_or(self.powered);
                    self.structure_dirty = true;
                    self.finish_local_operation(
                        crate::actions::BluetoothOperation::Refresh,
                        Ok(t("settings-bluetooth-refreshed")),
                    );
                }
                Err(error) => {
                    self.notice = Some(t_args(
                        "settings-bluetooth-refresh-failed",
                        &[("error", error.clone().into())],
                    ));
                    self.finish_local_operation(
                        crate::actions::BluetoothOperation::Refresh,
                        Err(t_args(
                            "settings-bluetooth-refresh-failed-op",
                            &[("error", error.into())],
                        )),
                    );
                }
            }
        }
    }
    #[allow(
        clippy::too_many_lines,
        reason = "the device snapshot loop keeps each live-ID command beside its revalidation"
    )]
    fn sync_devices(&mut self, state: &mut AutoCommit, root: &ObjId) {
        let snapshot = self.devices.clone();
        for d in &snapshot {
            if read_bool(state, root, key(&d.path, "pair")).unwrap_or(false) {
                let _ = state.put(root, key(&d.path, "pair"), false);
                if self.pending_pair.is_none() {
                    let control = Arc::clone(&self.control);
                    let path = d.path.clone();
                    let (tx, rx) = mpsc::channel();
                    self.pending_pair = Some(path.clone());
                    self.pair_result = Some(rx);
                    self.notice = Some(t_args(
                        "settings-bluetooth-pairing",
                        &[("name", d.name.clone().into())],
                    ));
                    std::thread::spawn(move || {
                        let result = control.pair(&path);
                        let _ = tx.send((path, result));
                    });
                    self.structure_dirty = true;
                }
            }
            if read_bool(state, root, key(&d.path, "cancel")).unwrap_or(false) {
                let _ = state.put(root, key(&d.path, "cancel"), false);
                match self.control.cancel_pairing(&d.path) {
                    Ok(()) => {
                        self.notice = Some(t_args(
                            "settings-bluetooth-cancelled",
                            &[("name", d.name.clone().into())],
                        ))
                    }
                    Err(e) => {
                        self.notice = Some(t_args(
                            "settings-bluetooth-cancel-failed",
                            &[("error", e.into())],
                        ))
                    }
                }
            }
            for a in ["connect", "disconnect"] {
                self.act(state, root, d, a);
            }
            if read_bool(state, root, key(&d.path, "details")).unwrap_or(false) {
                let _ = state.put(root, key(&d.path, "details"), false);
                self.expanded = (self.expanded.as_deref() != Some(&d.path)).then(|| d.path.clone());
                self.structure_dirty = true;
            }
            if let Some(v) = read_bool(state, root, key(&d.path, "trusted"))
                && v != d.trusted
            {
                match self.control.set_trusted(&d.path, v) {
                    Ok(()) => {
                        self.notice = Some(if v {
                            t_args(
                                "settings-bluetooth-now-trusted",
                                &[("name", d.name.clone().into())],
                            )
                        } else {
                            t_args(
                                "settings-bluetooth-now-untrusted",
                                &[("name", d.name.clone().into())],
                            )
                        });
                    }
                    Err(e) => {
                        self.notice = Some(t_args(
                            "settings-bluetooth-trust-failed",
                            &[("error", e.into())],
                        ));
                        let _ = state.put(root, key(&d.path, "trusted"), d.trusted);
                    }
                }
                self.refresh();
            }
            if read_bool(state, root, key(&d.path, "rename")).unwrap_or(false) {
                let _ = state.put(root, key(&d.path, "rename"), false);
                let alias =
                    read_str_or_text(state, root, key(&d.path, "alias")).unwrap_or_default();
                match self.control.set_alias(&d.path, &alias) {
                    Ok(()) => {
                        self.notice = Some(t_args(
                            "settings-bluetooth-renamed",
                            &[("alias", alias.trim().to_string().into())],
                        ))
                    }
                    Err(e) => {
                        self.notice = Some(t_args(
                            "settings-bluetooth-rename-failed",
                            &[("name", d.name.clone().into()), ("error", e.into())],
                        ))
                    }
                }
                self.refresh();
            }
            if read_bool(state, root, key(&d.path, "forget")).unwrap_or(false) {
                let _ = state.put(root, key(&d.path, "forget"), false);
                let local_forget = matches!(
                    self.operation_status.get(),
                    crate::actions::LocalOperationStatus::Running { token }
                        if token == crate::actions::local_operation_token_for(
                            crate::actions::BLUETOOTH_OPERATION_TYPE,
                            &crate::actions::BluetoothOperation::Forget { path: d.path.clone() },
                        )
                );
                if self.confirm_forget.as_deref() == Some(&d.path) || local_forget {
                    match self.control.forget(&d.path) {
                        Ok(()) => {
                            self.notice = Some(t_args(
                                "settings-bluetooth-forgot-notice",
                                &[("name", d.name.clone().into())],
                            ));
                            self.confirm_forget = None;
                            self.finish_local_operation(
                                crate::actions::BluetoothOperation::Forget {
                                    path: d.path.clone(),
                                },
                                Ok(t_args(
                                    "settings-bluetooth-forgot",
                                    &[("name", d.name.clone().into())],
                                )),
                            );
                        }
                        Err(e) => {
                            let message = t_args(
                                "settings-bluetooth-forget-failed",
                                &[("name", d.name.clone().into()), ("error", e.into())],
                            );
                            self.notice = Some(message.clone());
                            self.finish_local_operation(
                                crate::actions::BluetoothOperation::Forget {
                                    path: d.path.clone(),
                                },
                                Err(message),
                            );
                        }
                    }
                    self.refresh();
                } else {
                    self.confirm_forget = Some(d.path.clone());
                    self.notice = Some(t_args(
                        "settings-bluetooth-forget-ask",
                        &[("name", d.name.clone().into())],
                    ));
                    self.structure_dirty = true;
                }
            }
        }
    }
}
pub fn build() -> Box<dyn Widget> {
    Box::new(BluetoothPane::new())
}
impl Widget for BluetoothPane {
    fn sync(&mut self, shared: &mut AppModel, root: &ModelPath) {
        let (shared, _) = ui::doc::document(shared, root);
        let isolated = self.isolate_document;
        let mut state = if isolated {
            std::mem::take(&mut self.local_doc)
        } else {
            std::mem::take(shared)
        };
        let root = automerge::ROOT;
        self.sync_pair_result();
        self.seed_if_needed(&mut state, &root);
        if let Some(crate::actions::LocalPaneCommand::Bluetooth(operation)) =
            self.command_state.get()
        {
            self.command_state.set(None);
            self.execute_local_command(&mut state, operation);
        }
        if self.has_pending_operation() {
            self.sync_power(&mut state, &root);
            self.sync_scan(&mut state, &root);
            self.sync_refresh(&mut state, &root);
            self.sync_devices(&mut state, &root);
        }
        if let Some(c) = &mut self.composed {
            c.sync(&mut state, &root);
        }
        if isolated {
            self.local_doc = state;
        } else {
            *shared = state;
        }
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut c = self.build_composition();
        let child = c.build_layout(taffy, theme, node_map);
        self.composed = Some(c);
        let node = taffy
            .new_with_children(
                taffy::Style {
                    display: taffy::Display::Flex,
                    flex_direction: taffy::FlexDirection::Column,
                    size: taffy::Size {
                        width: taffy::Dimension::percent(1.0),
                        height: taffy::Dimension::auto(),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap_or(child);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        if let Some(c) = &mut self.composed
            && let Ok(ch) = taffy.children(node)
            && let Some(&cn) = ch.first()
        {
            c.apply_layout(taffy, cn, Point::new(self.rect.x0, self.rect.y0));
        }
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if let Some(c) = &mut self.composed {
            c.draw(scene, transform, text_renderer, theme, presence);
        }
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let Some(c) = &mut self.composed
            && let Ok(ch) = ctx.taffy.children(ctx.node_id)
            && let Some(&cn) = ch.first()
        {
            c.handle_event(event, &mut ctx.reborrow(ctx.path, cn));
        }
    }
    fn children(&self) -> Vec<&dyn Widget> {
        self.composed
            .as_ref()
            .map(|c| c.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }
    fn name(&self) -> &'static str {
        "BluetoothPane"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.structure_dirty)
            || self
                .composed
                .as_mut()
                .is_some_and(|c| c.take_structure_dirty())
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
    /// Object paths and zero battery, inert replicated latches, and confirmed forget.
    #[test]
    fn bluetooth_devices_are_addressed_by_path_and_guarded() {
        // Case `uses_object_path_and_preserves_zero_battery`.
        {
            let m = Arc::new(Mock {
                calls: Mutex::new(vec![]),
                devices: vec![dev()],
            });
            let mut p = BluetoothPane::with_control(m.clone(), vec![dev()], true, false);
            let mut d = AutoCommit::new();
            let r = automerge::ROOT;
            p.sync(&mut d, &r);
            let _ = d.put(&r, key(&dev().path, "connect"), true);
            p.sync(&mut d, &r);
            assert_eq!(
                m.calls.lock().unwrap().as_slice(),
                ["connect:/org/bluez/hci0/dev_AA"]
            );
            assert_eq!(p.devices()[0].battery, Some(0));
        }

        // Case `replicated_bluetooth_latches_are_inert_without_a_local_token`.
        {
            let mock = Arc::new(Mock {
                calls: Mutex::new(vec![]),
                devices: vec![dev()],
            });
            let status = ui::LocalState::new(crate::actions::LocalOperationStatus::Idle);
            let commands = ui::LocalState::new(None);
            let mut pane = BluetoothPane::with_control(mock.clone(), vec![dev()], true, false)
                .with_command_state(&commands)
                .with_operation_status(&status);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let initial_heads = doc.get_heads();
            pane.sync(&mut doc, &root);
            assert_eq!(doc.get_heads(), initial_heads);
            let _ = doc.put(&root, BT_ENABLED_KEY, false);
            let _ = doc.put(&root, key(&dev().path, "connect"), true);
            pane.sync(&mut doc, &root);
            assert!(mock.calls.lock().unwrap().is_empty());
            assert!(pane.powered);
        }

        // Case `forget_requires_confirmation`.
        {
            let mock = Arc::new(Mock {
                calls: Mutex::new(vec![]),
                devices: vec![dev()],
            });
            let mut pane = BluetoothPane::with_control(mock.clone(), vec![dev()], true, false);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            let forget_key = key(&dev().path, "forget");
            let _ = doc.put(&root, &forget_key, true);
            pane.sync(&mut doc, &root);
            assert!(mock.calls.lock().unwrap().is_empty());
            let _ = doc.put(&root, &forget_key, true);
            pane.sync(&mut doc, &root);
            assert_eq!(
                mock.calls.lock().unwrap().as_slice(),
                ["forget:/org/bluez/hci0/dev_AA"]
            );
        }
    }
}
