//! The person's devices as VPNs (docs/device-vpn.md): each of their devices
//! that shares its connection is a "Through <device>" row of the VPN list,
//! and this device shares its own connection with theirs by a switch, with a
//! Stop for each device using it.
//!
//! Settings only writes the person's choices (`settings_store::device_vpn`)
//! and shows what the session publishes it is doing, read off the UI thread
//! whenever the session's status file changes.

use std::any::Any;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use settings_store::device_vpn::{
    self, ClientState, ClientStatus, DeviceVpnRow, DeviceVpnStatus, EXIT, ExitUser, OfferedExit,
    Problem, Refusal, STOP, StopRequest, THROUGH, Through,
};
use ui::LocalState;
use ui::framework::LiveRow;
use ui::icons::Icon;
use ui::tilo_resource::Effect as _;
use ui::widget::{Button, ListRow, Toggle};

use super::super::{Begun, NetworkModel};
use super::VpnEffect;
use crate::effects::{SettingsEffect, effect};
use crate::i18n::{t, t_args};
use crate::watch::Watched;
use crate::widgets::{El, SettingsSection, block, card, rows, setting_row_described};

/// What the VPN list knows of the person's devices as exits.
#[derive(Default)]
pub(crate) struct DeviceVpnState {
    /// What the session last published; `None` while no session publishes,
    /// and the list then offers none of this.
    pub(crate) status: Option<DeviceVpnStatus>,
    /// The watch on the session's status file while a pane follows it.
    following: Weak<Watched>,
    /// The status being read.
    reading: Option<crate::channel::Receiver<Option<DeviceVpnStatus>>>,
    /// The file changed again while it was being read.
    again: bool,
}

#[cfg(test)]
impl DeviceVpnState {
    /// Follow `watched` in place of the session's file.
    pub(crate) fn follow_by_hand(&mut self, watched: &Rc<Watched>) {
        self.following = Rc::downgrade(watched);
    }
}

/// "Through" and the device's name.
fn through_name(label: &str) -> String {
    t_args(
        "net-device-vpn-through",
        &[("device", shown_label(label).into())],
    )
}

fn shown_label(label: &str) -> String {
    if label.trim().is_empty() {
        t("net-device-vpn-unnamed")
    } else {
        label.to_owned()
    }
}

/// Where a route through a device stands, in words.
fn state_words(state: Option<&ClientState>) -> String {
    t(match state {
        None => "net-vpn-off",
        Some(ClientState::Connecting) => "net-vpn-connecting-short",
        Some(ClientState::Connected { .. }) => "net-connected",
        Some(ClientState::Reconnecting { blocking: false }) => "net-device-vpn-reconnecting",
        Some(ClientState::Reconnecting { blocking: true }) => "net-device-vpn-blocking",
        Some(ClientState::Refused {
            reason: Refusal::NotYours,
        }) => "net-device-vpn-not-yours",
        Some(ClientState::Refused {
            reason: Refusal::ExitOff,
        }) => "net-device-vpn-exit-off",
        Some(ClientState::Refused {
            reason: Refusal::Full,
        }) => "net-device-vpn-full",
        Some(ClientState::Failed {
            problem: Problem::NotInstalled,
        }) => "net-device-vpn-not-installed",
        Some(ClientState::Failed {
            problem: Problem::Setup,
        }) => "net-device-vpn-setup",
        Some(ClientState::Failed {
            problem: Problem::NoHomeNetwork,
        }) => "net-device-vpn-no-home",
    })
}

fn through_row_id(device: &str) -> String {
    format!("vpn.device:{device}")
}

fn exit_user_row_id(device: &str) -> String {
    format!("vpn.exit-user:{device}")
}

impl NetworkModel {
    /// Follow the session's status file, on a device.
    pub(crate) fn follow_device_vpn(&mut self) -> Option<Rc<dyn Any>> {
        if !self.cx.reaches_devices() {
            return None;
        }
        let path = device_vpn::status_path()?;
        let watched =
            crate::watch::shared(&mut self.vpn.devices.following, || Watched::files(&[path]));
        Some(watched)
    }

    /// Take in a status read, and read it again when the file changed.
    pub(crate) fn poll_device_vpn(&mut self) {
        let devices = &mut self.vpn.devices;
        if let Some(receiver) = &devices.reading {
            match receiver.try_recv() {
                Ok(status) => {
                    devices.reading = None;
                    if status != devices.status {
                        devices.status = status;
                        self.changed = true;
                    }
                }
                Err(crate::channel::TryRecvError::Disconnected) => devices.reading = None,
                Err(crate::channel::TryRecvError::Empty) => {}
            }
        }
        let devices = &mut self.vpn.devices;
        if devices
            .following
            .upgrade()
            .is_some_and(|watched| watched.take())
        {
            devices.again = true;
        }
        if devices.again && devices.reading.is_none() {
            devices.again = false;
            let (sender, receiver) = crate::channel::channel();
            let service = Arc::clone(&self.service);
            let wake = ui::WakeHandle::new();
            let started = crate::device_read::thread(
                "settings-device-vpn-read",
                Box::new(move || {
                    if sender.send(service.device_vpn()).is_ok() {
                        wake.wake();
                    }
                }),
            );
            if started.is_ok() {
                self.vpn.devices.reading = Some(receiver);
            }
        }
    }

    /// The rows of the person's devices: one per device to route through,
    /// one per device using this one.
    pub(crate) fn device_vpn_rows(&self) -> Vec<LiveRow> {
        let Some(status) = &self.vpn.devices.status else {
            return Vec::new();
        };
        status
            .rows()
            .iter()
            .map(Self::through_live)
            .chain(status.exit_users.iter().map(Self::exit_user_live))
            .collect()
    }

    fn through_live(row: &DeviceVpnRow) -> LiveRow {
        LiveRow::new(
            through_row_id(&row.device),
            "vpn.device",
            through_name(&row.label),
        )
        .operation(
            SettingsEffect::from(VpnEffect::SetDeviceVpn {
                device: row.device.clone(),
                on: !row.is_on(),
            })
            .bound_asking(&["on"]),
        )
    }

    fn exit_user_live(user: &ExitUser) -> LiveRow {
        LiveRow::new(
            exit_user_row_id(&user.device),
            "vpn.exit-user",
            shown_label(&user.label),
        )
        .operation(
            SettingsEffect::from(VpnEffect::StopDeviceExit {
                device: user.device.clone(),
            })
            .bound(),
        )
    }

    /// The VPN list's rows for the person's devices to route through.
    pub(super) fn through_lines(&self) -> Vec<El> {
        let Some(status) = &self.vpn.devices.status else {
            return Vec::new();
        };
        status
            .rows()
            .iter()
            .map(|row| {
                let row_live = Self::through_live(row).into_resource();
                let name = through_name(&row.label);
                let device = row.device.clone();
                let toggle = self.cx.bind_on_row(
                    &row_live,
                    Toggle::from_state(&LocalState::new(row.is_on()))
                        .with_id(&format!(
                            "settings.network.vpn.device.switch.{}",
                            row.device
                        ))
                        .with_accessible_name(&name),
                    effect(VpnEffect::SET_DEVICE_VPN).map(move |on: bool| (device.clone(), on)),
                );
                let line =
                    ListRow::new(format!("settings.network.vpn.device.{}", row.device), name)
                        .icon(Icon::MonitorSmartphone)
                        .secondary(state_words(row.state.as_ref()))
                        .trailing(Box::new(toggle));
                let choose = VpnEffect::SetDeviceVpn {
                    device: row.device.clone(),
                    on: !row.is_on(),
                };
                Box::new(match self.cx.row_invocation(&row_live, choose) {
                    Some(binding) => line.on_action(binding),
                    None => line.with_enabled(false),
                }) as El
            })
            .collect()
    }

    /// This device as a VPN for the person's others: the switch, and each
    /// device using it with Stop. Only where this device can be one.
    pub(super) fn device_exit_section(&self) -> Option<SettingsSection> {
        let status = self
            .vpn
            .devices
            .status
            .as_ref()
            .filter(|status| status.exit_available)?;
        let share = t("net-device-vpn-share");
        let toggle = self.cx.bind(
            Toggle::from_state(&LocalState::new(status.exit_on))
                .with_id("settings.network.vpn.exit")
                .with_accessible_name(&share),
            effect(VpnEffect::SET_DEVICE_EXIT),
        );
        let mut content = vec![setting_row_described(
            &share,
            &t("net-device-vpn-share-caption"),
            Box::new(toggle),
        )];
        if !status.exit_users.is_empty() {
            let lines: Vec<El> = status
                .exit_users
                .iter()
                .map(|user| {
                    let user_row = Self::exit_user_live(user).into_resource();
                    let stop = self.cx.bind_row(
                        Button::new(
                            &format!("settings.network.vpn.exit-user.stop.{}", user.device),
                            &t("net-device-vpn-stop"),
                        ),
                        &user_row,
                        VpnEffect::StopDeviceExit {
                            device: user.device.clone(),
                        },
                    );
                    Box::new(
                        ListRow::new(
                            format!("settings.network.vpn.exit-user.{}", user.device),
                            shown_label(&user.label),
                        )
                        .icon(Icon::MonitorSmartphone)
                        .secondary(t("net-device-vpn-using"))
                        .trailing(Box::new(stop)),
                    ) as El
                })
                .collect();
            content.push(block(Box::new(ui::widget::Section::new(lines))));
        }
        Some(card(&t("net-device-vpn-exit"), rows(content)))
    }

    /// Merge `value` into the choice `name` of this device's VPN section.
    fn write_device_vpn(
        &self,
        name: &str,
        value: Result<serde_json::Value, serde_json::Error>,
    ) -> Result<(), String> {
        let value = value.map_err(|error| error.to_string())?;
        let mut values = serde_json::Map::new();
        values.insert(name.to_owned(), value);
        self.cx
            .storage()
            .settings_document()
            .update_section(device_vpn::SECTION, values)
            .map_err(|error| error.to_string())
    }

    /// Route through `device`, keeping the options chosen before, or stop
    /// routing through it.
    pub(super) fn route_through_device(&mut self, device: &str, on: bool) -> Result<Begun, String> {
        let status = self
            .vpn
            .devices
            .status
            .as_ref()
            .ok_or_else(|| t("net-device-vpn-unavailable"))?;
        let label = status
            .rows()
            .into_iter()
            .find(|row| row.device == device)
            .map(|row| row.label)
            .ok_or_else(|| t("net-device-vpn-unknown"))?;
        let document = self.cx.storage().settings_document();
        let previous = THROUGH.from_section(&document.section(device_vpn::SECTION));
        let next = if on {
            Some(Through {
                device: device.to_owned(),
                ..previous.clone().unwrap_or_default()
            })
        } else {
            previous.clone().filter(|through| through.device != device)
        };
        if next != previous {
            self.write_device_vpn(THROUGH.name(), serde_json::to_value(&next))?;
        }
        // Shown as asked until the session publishes where it stands.
        if let Some(status) = &mut self.vpn.devices.status {
            if let Some(client) = status.client.take()
                && !status
                    .offered
                    .iter()
                    .any(|offered| offered.device == client.device)
            {
                status.offered.insert(
                    0,
                    OfferedExit {
                        device: client.device,
                        label: client.label,
                    },
                );
            }
            status.client = next.map(|through| ClientStatus {
                device: through.device,
                label,
                state: ClientState::Connecting,
                routes: through.routes,
                kill_switch: through.kill_switch,
            });
        }
        self.changed = true;
        Ok(Begun::Done)
    }

    /// Let the person's other devices use this one as a VPN, or not.
    pub(super) fn share_device(&mut self, on: bool) -> Result<Begun, String> {
        if !self
            .vpn
            .devices
            .status
            .as_ref()
            .is_some_and(|status| status.exit_available)
        {
            return Err(t("net-device-vpn-cannot-share"));
        }
        self.write_device_vpn(EXIT.name(), serde_json::to_value(on))?;
        if let Some(status) = &mut self.vpn.devices.status {
            status.exit_on = on;
        }
        self.changed = true;
        Ok(Begun::Done)
    }

    /// Stop `device` using this one as a VPN.
    pub(super) fn stop_exit_user(&mut self, device: &str) -> Result<Begun, String> {
        let user = self
            .vpn
            .devices
            .status
            .as_ref()
            .and_then(|status| status.exit_users.iter().find(|user| user.device == device))
            .cloned()
            .ok_or_else(|| t("net-device-vpn-unknown"))?;
        let document = self.cx.storage().settings_document();
        let previous = STOP.from_section(&document.section(device_vpn::SECTION));
        let request = StopRequest::after(previous.as_ref(), device);
        self.write_device_vpn(STOP.name(), serde_json::to_value(Some(request)))?;
        if let Some(status) = &mut self.vpn.devices.status {
            status.exit_users.retain(|using| using.device != device);
        }
        self.notice = Some(t_args(
            "net-device-vpn-stopped",
            &[("device", shown_label(&user.label).into())],
        ));
        self.changed = true;
        Ok(Begun::Done)
    }
}
