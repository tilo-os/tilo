//! The person's devices, each once, in one list: a row per device whose
//! settings open under it, renaming a home server in place, and removing a
//! device after asking.

use tilo_relay_protocol::{DeviceRole, ReplicationIntent};
use ui::widget::{Button, ButtonRole, DisclosureRow, Dropdown, EditableLabel, Widget};

use super::{
    DeviceMeshEffect, HomePeerHealth, Page, PairingTarget, RelayRuntimeStatus, home_server_row,
    last_seen_label, peer_health_badge, peer_health_detail, peer_key, personal_device_key,
    replication_description, replication_shown, role_shown, runtime_status_tone,
    shows_advanced_services,
};
use crate::effects::effect;
use crate::i18n::{t, t_args};
use provider_settings_ui::widgets::{
    SettingsSection, StatusTone, error_caption, note, row, rows, section_with,
    semantic_status_pill, setting_row_described, value_row,
};

const THIS_DEVICE_KEY: &str = "this-device";

struct DeviceEntry<'a> {
    key: String,
    label: String,
    personal: Option<&'a crate::device_fleet::DeviceView>,
    peer: Option<&'a tilo_relay_protocol::HomePeerRegistration>,
    this: bool,
}

impl Page<'_> {
    fn device_entries(&self) -> Vec<DeviceEntry<'_>> {
        let peer_of = |device: &crate::device_fleet::DeviceView| {
            let key = hex::encode(device.key);
            self.home_peers
                .iter()
                .find(|peer| peer.device_node_id.eq_ignore_ascii_case(&key))
        };
        let mut entries = Vec::new();
        match self
            .personal_devices
            .iter()
            .find(|device| device.current && !device.revoked)
        {
            Some(device) => entries.push(DeviceEntry {
                key: hex::encode(device.key),
                label: device.label.clone(),
                personal: Some(device),
                peer: None,
                this: true,
            }),
            // Before the device list answers, this device still has its
            // settings.
            None => entries.push(DeviceEntry {
                key: THIS_DEVICE_KEY.to_owned(),
                label: t("mesh-status-this-device"),
                personal: None,
                peer: None,
                this: true,
            }),
        }
        for device in self
            .personal_devices
            .iter()
            .filter(|device| !device.current && !device.revoked)
        {
            entries.push(DeviceEntry {
                key: hex::encode(device.key),
                label: device.label.clone(),
                personal: Some(device),
                peer: peer_of(device),
                this: false,
            });
        }
        for peer in &self.home_peers {
            if !entries.iter().any(|entry| {
                entry
                    .peer
                    .is_some_and(|known| known.device_node_id == peer.device_node_id)
            }) {
                entries.push(DeviceEntry {
                    key: peer.device_node_id.clone(),
                    label: peer.label.clone(),
                    personal: None,
                    peer: Some(peer),
                    this: false,
                });
            }
        }
        for device in self.personal_devices.iter().filter(|device| device.revoked) {
            entries.push(DeviceEntry {
                key: hex::encode(device.key),
                label: device.label.clone(),
                personal: Some(device),
                peer: None,
                this: false,
            });
        }
        entries
    }

    pub(super) fn devices_section(&self) -> SettingsSection {
        let pairing = self.pairing_call.is_some();
        let personal_label = if pairing && self.pairing_target == PairingTarget::PersonalDevice {
            t("mesh-pairing")
        } else if self.pairing_target == PairingTarget::PersonalDevice {
            self.pairing_candidate.as_ref().map_or_else(
                || t("mesh-add-device"),
                |candidate| {
                    t_args(
                        "mesh-approve",
                        &[
                            ("label", candidate.label.as_str().into()),
                            ("code", candidate.verification_code.as_str().into()),
                        ],
                    )
                },
            )
        } else {
            t("mesh-add-device")
        };
        let add_device = Box::new(
            self.bind_invocation(
                Button::new("device_mesh.approve_personal_device", &personal_label)
                    .with_enabled(!pairing),
                DeviceMeshEffect::AddPersonalDevice,
            ),
        );

        let mut items = Vec::new();
        for entry in self.device_entries() {
            items.push(row(Box::new(
                DisclosureRow::keyed(
                    entry.label.clone(),
                    self.device_details(&entry),
                    &self.device_rows,
                    &entry.key,
                )
                .icon(if entry.this {
                    ui::icons::Icon::Monitor
                } else if entry.peer.is_some() || entry.personal.is_some_and(|d| d.can_host) {
                    ui::icons::Icon::HardDrive
                } else {
                    ui::icons::Icon::MonitorSmartphone
                })
                .secondary(self.device_state(&entry)),
            )));
        }
        if self.personal_devices_unavailable {
            items.push(row(error_caption(&t(
                "mesh-status-device-list-unavailable",
            ))));
        }

        let home_label = if pairing && self.pairing_target == PairingTarget::HomePeer {
            t("mesh-pairing")
        } else if let Some(candidate) = self
            .pairing_candidate
            .as_ref()
            .filter(|_| self.pairing_target == PairingTarget::HomePeer)
        {
            t_args(
                "mesh-approve",
                &[
                    ("label", candidate.label.as_str().into()),
                    ("code", candidate.verification_code.as_str().into()),
                ],
            )
        } else {
            t("mesh-pair-home")
        };
        items.push(setting_row_described(
            &t("mesh-home-nas"),
            &t("mesh-home-caption"),
            Box::new(self.bind_invocation(
                Button::new("device_mesh.approve_enrollment", &home_label).with_enabled(!pairing),
                DeviceMeshEffect::PairHomeServer,
            )),
        ));
        items.push(note(&t("mesh-personal-caption")));
        if let Some(status) = self.status.pairing_shown() {
            items.push(note(&status));
        }
        section_with(&t("mesh-trusted-title"), add_device, rows(items))
    }

    fn device_state(&self, entry: &DeviceEntry<'_>) -> String {
        if entry.this {
            return t("mesh-status-this-device");
        }
        if entry.personal.is_some_and(|device| device.revoked) {
            return t("mesh-status-revoked");
        }
        if let Some(peer) = entry.peer {
            return self.peer_health(peer);
        }
        t(if entry.personal.is_some_and(|device| device.can_host) {
            "mesh-status-home-server"
        } else {
            "mesh-status-trusted"
        })
    }

    /// A home server's line under its name: how it is doing, and when it
    /// was last seen if it is not answering.
    fn peer_health(&self, peer: &tilo_relay_protocol::HomePeerRegistration) -> String {
        let device = &peer.device_node_id;
        let health = self.home_peer_health.get(device).copied();
        match self.home_peer_last_seen.get(device) {
            Some(last_seen) if matches!(health, None | Some(HomePeerHealth::Unreachable)) => {
                last_seen_label(*last_seen)
            }
            _ => health.map_or_else(|| t("mesh-checking"), peer_health_badge),
        }
    }

    fn device_details(&self, entry: &DeviceEntry<'_>) -> Box<dyn Widget> {
        let mut items = Vec::new();
        if let Some(device) = entry.personal {
            items.push(value_row(&t("mesh-fingerprint-label"), &device.fingerprint));
        }
        if entry.this {
            items.extend(self.this_device_rows());
        }
        if let Some(peer) = entry.peer {
            items.extend(self.home_peer_rows(peer));
        }
        let removable = entry
            .personal
            .is_some_and(|device| !device.current && !device.revoked && !device.can_host);
        if removable || entry.peer.is_some() {
            items.extend(self.removal_rows(entry));
        }
        provider_settings_ui::widgets::page(vec![provider_settings_ui::widgets::untitled(rows(
            items,
        ))])
    }

    fn this_device_rows(&self) -> Vec<provider_settings_ui::widgets::Item> {
        let role_shown_value = role_shown(self.config.device_role);
        let role_state = ui::LocalState::new(role_shown_value.clone());
        let role = Dropdown::from_state(
            [
                DeviceRole::Interactive,
                DeviceRole::Hybrid,
                DeviceRole::AlwaysOnServer,
            ]
            .into_iter()
            .map(role_shown)
            .collect(),
            &role_state,
        )
        .with_id("device_mesh.role")
        .with_placeholder(&role_shown_value);
        let role = self.bind(role, effect(DeviceMeshEffect::SET_DEVICE_ROLE));
        let mut out = vec![setting_row_described(
            &t("mesh-this-device"),
            &t("mesh-this-device-desc"),
            Box::new(role),
        )];
        if shows_advanced_services(self.config.device_role) {
            let home_desc = t(match self.runtime_status {
                RelayRuntimeStatus::Ready => "mesh-home-ready",
                RelayRuntimeStatus::Online => "mesh-home-online",
                RelayRuntimeStatus::Syncing => "mesh-home-syncing",
                RelayRuntimeStatus::Offline => "mesh-home-offline",
                RelayRuntimeStatus::Unreadable => "mesh-home-unreadable",
            });
            let runtime = self.runtime_status.label();
            out.push(setting_row_described(
                &t("mesh-home-peer"),
                &home_desc,
                semantic_status_pill(&runtime, runtime_status_tone(self.runtime_status)),
            ));
        }
        out.push(self.files_row());
        if shows_advanced_services(self.config.device_role) {
            out.extend(self.service_rows());
        }
        out.push(note(&t(match self.config.device_role {
            DeviceRole::Interactive => "mesh-role-hint-personal",
            DeviceRole::Hybrid => "mesh-role-hint-hybrid",
            DeviceRole::AlwaysOnServer
                if self.common_replication() == ReplicationIntent::FullWorkspace =>
            {
                "mesh-role-hint-server-full"
            }
            DeviceRole::AlwaysOnServer => "mesh-role-hint-server",
        })));
        out
    }

    fn files_row(&self) -> provider_settings_ui::widgets::Item {
        let files = t("mesh-files");
        if !self.config.device_role.can_serve() {
            return value_row(&files, &t("mesh-download-opened"));
        }
        let repl_shown = replication_shown(&self.common_replication());
        let replication_state = ui::LocalState::new(repl_shown.clone());
        let replication = Dropdown::from_state(
            [
                ReplicationIntent::OnDemand,
                ReplicationIntent::WorkspaceOnly,
                ReplicationIntent::FullWorkspace,
            ]
            .into_iter()
            .map(|intent| replication_shown(&intent))
            .collect(),
            &replication_state,
        )
        .with_id("device_mesh.replication")
        .with_placeholder(&repl_shown);
        let replication = self.bind(replication, effect(DeviceMeshEffect::SET_REPLICATION));
        setting_row_described(
            &files,
            &replication_description(&self.common_replication()),
            Box::new(replication),
        )
    }

    fn service_rows(&self) -> [provider_settings_ui::widgets::Item; 4] {
        [
            self.service_row(
                "mesh-svc-relay",
                "mesh-svc-relay-desc",
                "inbound_relay",
                "device_mesh.inbound_relay",
                self.config.services.inbound_relay,
            ),
            self.service_row(
                "mesh-svc-remote",
                "mesh-svc-remote-desc",
                "remote_control_relay",
                "device_mesh.remote_relay",
                self.config.services.remote_control_relay,
            ),
            self.service_row(
                "mesh-svc-apps",
                "mesh-svc-apps-desc",
                "app_serving",
                "device_mesh.app_serving",
                self.config.services.app_serving,
            ),
            self.service_row(
                "mesh-svc-jobs",
                "mesh-svc-jobs-desc",
                "compute",
                "device_mesh.compute",
                self.config.services.compute,
            ),
        ]
    }

    fn home_peer_rows(
        &self,
        peer: &tilo_relay_protocol::HomePeerRegistration,
    ) -> Vec<provider_settings_ui::widgets::Item> {
        let device = peer.device_node_id.clone();
        let name = self.bind_on_row(
            &home_server_row(peer).into_resource(),
            EditableLabel::new(&peer.label)
                .with_id(&peer_key("name", &peer.device_node_id))
                .with_accessible_name(t("mesh-name"))
                .edits_on_click(),
            effect(DeviceMeshEffect::RENAME_HOME_SERVER)
                .map(move |name: String| (device.clone(), name.trim().to_owned())),
        );
        let health = self.home_peer_health.get(&peer.device_node_id).copied();
        let badge = health.map_or_else(|| t("mesh-badge-checking"), peer_health_badge);
        let detail = health.map_or_else(|| t("mesh-checking"), peer_health_detail);
        let tone = match health {
            Some(HomePeerHealth::Online { .. }) => StatusTone::Success,
            Some(HomePeerHealth::Connecting) | None => StatusTone::Warning,
            Some(HomePeerHealth::Removed | HomePeerHealth::Unreachable) => StatusTone::Neutral,
        };
        vec![
            provider_settings_ui::widgets::setting_row(&t("mesh-name"), Box::new(name)),
            setting_row_described(
                &t("mesh-availability"),
                &detail,
                semantic_status_pill(&badge, tone),
            ),
        ]
    }

    fn removal_rows(&self, entry: &DeviceEntry<'_>) -> Vec<provider_settings_ui::widgets::Item> {
        let Some(peer) = entry.peer else {
            if entry.personal.is_none() {
                return Vec::new();
            }
            let open = self.bind_invocation(
                Button::new(
                    &personal_device_key("manage-access", &entry.key),
                    &t("mesh-manage-access"),
                ),
                crate::effects::NavigationEffect::OpenPane {
                    pane: crate::categories::Category::DevicesAccess.slug().to_owned(),
                },
            );
            return vec![provider_settings_ui::widgets::row(Box::new(
                ui::widget::Row::new()
                    .push(Box::new(open))
                    .push(Box::new(ui::widget::Spacer::new())),
            ))];
        };
        let live = home_server_row(peer).into_resource();
        if self.removing.as_deref() != Some(entry.key.as_str()) {
            return vec![provider_settings_ui::widgets::row(Box::new(
                ui::widget::Row::new()
                    .push(Box::new(
                        self.bind_row(
                            Button::new(
                                &peer_key("review-remove", &entry.key),
                                &t("mesh-remove-home"),
                            )
                            .role(ButtonRole::Destructive),
                            &live,
                            DeviceMeshEffect::ReviewRemoval {
                                device: entry.key.clone(),
                            },
                        ),
                    ))
                    .push(Box::new(ui::widget::Spacer::new())),
            ))];
        }
        let confirm = self.bind_row(
            Button::new(
                &peer_key("remove", &peer.device_node_id),
                &t("mesh-remove-home-confirm"),
            )
            .role(ButtonRole::Destructive),
            &live,
            DeviceMeshEffect::RemoveHomeServer {
                device: peer.device_node_id.clone(),
            },
        );
        let cancel = self.bind_invocation(
            Button::new(&peer_key("cancel-remove", &entry.key), &t("mesh-cancel")),
            DeviceMeshEffect::CancelRemoval,
        );
        vec![provider_settings_ui::widgets::item_row(
            &t_args("mesh-remove-ask", &[("label", entry.label.as_str().into())]),
            Vec::new(),
            &[t("mesh-files-stay-desc")],
            vec![confirm, cancel],
        )]
    }
}
