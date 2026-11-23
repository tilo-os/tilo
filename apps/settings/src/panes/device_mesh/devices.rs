//! The person's devices, each once, in one list: a row per device whose
//! settings open under it, renaming a home server in place, and removing a
//! device after asking.

use super::{
    DeviceMeshEffect, HomePeerHealth, Page, ROOM, ServerRuntimeStatus, WORK_FROM, home_server_row,
    last_seen_label, peer_health_badge, peer_health_detail, peer_key, personal_device_key,
    room_shown, runtime_status_tone, takes_work, work_key,
};
use crate::effects::effect;
use crate::i18n::{t, t_args};
use tilo_system_resources::device_settings::WorkFrom;
use ui::icons::Icon;
use ui::tokens::{Rhythm, TextRole};
use ui::widget::{
    Button, ButtonRole, Column, DisclosureRow, Dropdown, EditableLabel, Row, Spacer, Text, Widget,
};

use provider_settings_ui::widgets::{
    SettingsSection, StatusTone, block, error_caption, note, row, rows, section_with,
    semantic_status_pill, setting_row_described,
};

const THIS_DEVICE_KEY: &str = "this-device";

struct DeviceEntry<'a> {
    key: String,
    label: String,
    personal: Option<&'a super::PersonalDevice>,
    peer: Option<&'a tilo_server_protocol::servers::PersonServer>,
    this: bool,
}

impl Page<'_> {
    /// This device first, then the person's other devices as the session reads them, then the
    /// home servers paired here that it does not list.
    fn device_entries(&self) -> Vec<DeviceEntry<'_>> {
        let devices = self.personal_devices.iter().flatten();
        let peer_of = |device: &super::PersonalDevice| {
            self.servers
                .iter()
                .find(|peer| peer.device.eq_ignore_ascii_case(&device.id))
        };
        let mut entries = Vec::new();
        match devices.clone().find(|device| device.local) {
            Some(device) => entries.push(DeviceEntry {
                key: device.id.clone(),
                label: device.label.clone(),
                personal: Some(device),
                peer: None,
                this: true,
            }),
            // Before the session's reading lands, this device still has its
            // settings.
            None => entries.push(DeviceEntry {
                key: THIS_DEVICE_KEY.to_owned(),
                label: t("mesh-status-this-device"),
                personal: None,
                peer: None,
                this: true,
            }),
        }
        for device in devices.filter(|device| !device.local) {
            entries.push(DeviceEntry {
                key: device.id.clone(),
                label: device.label.clone(),
                personal: Some(device),
                peer: peer_of(device),
                this: false,
            });
        }
        for peer in &self.servers {
            if !entries
                .iter()
                .any(|entry| entry.peer.is_some_and(|known| known.device == peer.device))
            {
                entries.push(DeviceEntry {
                    key: peer.device.clone(),
                    label: peer.label.clone(),
                    personal: None,
                    peer: Some(peer),
                    this: false,
                });
            }
        }
        entries
    }

    pub(super) fn devices_section(&self) -> SettingsSection {
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
                    Icon::Monitor
                } else if entry.peer.is_some() {
                    Icon::HardDrive
                } else {
                    Icon::MonitorSmartphone
                })
                .secondary(self.device_state(&entry)),
            )));
        }
        if self.personal_devices_unavailable {
            items.push(row(error_caption(&t(
                "mesh-status-device-list-unavailable",
            ))));
        } else if self.personal_devices.is_none() {
            items.push(note(&t("mesh-status-devices-reading")));
        }

        // The code a new computer joins with, while one is printed: the code itself, with the
        // one control that copies it, and the next step under it.
        if let Some(code) = &self.ticket {
            let copy = self.bind_invocation(
                Button::new("device_mesh.copy_code", &t("mesh-code-copy"))
                    .role(ButtonRole::Secondary)
                    .with_icon(Icon::Copy),
                DeviceMeshEffect::CopyCode { code: code.clone() },
            );
            items.push(block(Box::new(
                Column::new()
                    .with_rhythm(Rhythm::Related)
                    .push(Box::new(Text::new(code).with_role(TextRole::Code)))
                    .push(Box::new(
                        Row::new()
                            .push(Box::new(copy))
                            .push(Box::new(Spacer::new())),
                    )),
            )));
            items.push(note(&t("mesh-add-device-hint")));
        }
        if let Some(status) = self.status.ticket_shown() {
            items.push(note(&status));
        }
        if let Some(trouble) = self.status.settings_trouble_shown() {
            items.push(row(error_caption(&trouble)));
        }

        let add = self.bind_invocation(
            Button::new("device_mesh.add_device", &t("mesh-add-device"))
                .with_enabled(self.ticket_call.is_none()),
            DeviceMeshEffect::AddDevice,
        );
        section_with(&t("mesh-trusted-title"), Box::new(add), rows(items))
    }

    fn device_state(&self, entry: &DeviceEntry<'_>) -> String {
        if entry.this {
            return t("mesh-status-this-device");
        }
        if entry.personal.is_some_and(|device| device.nearby) {
            return t("mesh-status-nearby");
        }
        if let Some(peer) = entry.peer {
            return self.peer_health(peer);
        }
        t(if entry.personal.is_some_and(|device| device.online) {
            "mesh-badge-online"
        } else {
            "mesh-status-trusted"
        })
    }

    /// A home server's line under its name: how it is doing, and when it
    /// was last seen if it is not answering.
    fn peer_health(&self, peer: &tilo_server_protocol::servers::PersonServer) -> String {
        let device = &peer.device;
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
        if entry.this {
            items.extend(self.this_device_rows());
        }
        if let Some(peer) = entry.peer {
            items.extend(self.home_peer_rows(peer));
        }
        if !entry.this && (entry.personal.is_some() || entry.peer.is_some()) {
            items.extend(self.removal_rows(entry));
        }
        provider_settings_ui::widgets::page(vec![provider_settings_ui::widgets::untitled(rows(
            items,
        ))])
    }

    fn this_device_rows(&self) -> Vec<provider_settings_ui::widgets::Item> {
        // Work the page cannot set (some workspaces) shows as the placeholder.
        let chosen = work_key(self.settings.work.from);
        let selected = WORK_FROM.contains(&chosen).then(|| chosen.to_owned());
        let state = ui::LocalState::new(selected.unwrap_or_default());
        let mut work = Dropdown::from_state(Vec::new(), &state)
            .with_id("device_mesh.work")
            .with_placeholder(&t(chosen));
        work.set_value_options(WORK_FROM.map(|key| (key.to_owned(), t(key))).to_vec());
        let work = self.bind(work, effect(DeviceMeshEffect::SET_WORK));
        let mut out = vec![setting_row_described(
            &t("mesh-this-device"),
            &t("mesh-this-device-desc"),
            Box::new(work),
        )];
        let taking = takes_work(self.settings.work);
        if taking {
            let home_desc = t(match self.runtime_status {
                ServerRuntimeStatus::Online => "mesh-home-online",
                ServerRuntimeStatus::Offline => "mesh-home-offline",
                ServerRuntimeStatus::Unreadable => "mesh-home-unreadable",
            });
            let runtime = self.runtime_status.label();
            out.push(setting_row_described(
                &t("mesh-home-peer"),
                &home_desc,
                semantic_status_pill(&runtime, runtime_status_tone(self.runtime_status)),
            ));
        }
        if taking {
            out.extend(self.when_rows());
            out.extend(self.service_rows());
        }
        out.push(note(&t(match self.settings.work.from {
            WorkFrom::Nobody => "mesh-work-hint-nothing",
            WorkFrom::OwnDevices => "mesh-work-hint-own",
            WorkFrom::Anyone => "mesh-work-hint-anyone",
        })));
        out
    }

    /// The states this device takes work in: only while charging, and how much of the processors
    /// and memory it leaves to the people at it.
    fn when_rows(&self) -> [provider_settings_ui::widgets::Item; 3] {
        let when = self.settings.work.when;
        let room = |part: &'static str, label: &str, desc: &str, percent: u8| {
            let shown = room_shown(percent);
            let state = ui::LocalState::new(shown.clone());
            let choice = Dropdown::from_state(ROOM.into_iter().map(room_shown).collect(), &state)
                .with_id(&format!("device_mesh.work_room.{part}"))
                .with_placeholder(&shown);
            let choice = self.bind(
                choice,
                effect(DeviceMeshEffect::SET_WORK_ROOM)
                    .map(move |percent: String| (part.to_owned(), percent)),
            );
            setting_row_described(&t(label), &t(desc), Box::new(choice))
        };
        [
            self.switch(
                t("mesh-work-charging"),
                t("mesh-work-charging-desc"),
                "device_mesh.work_charging".to_owned(),
                when.charging_only,
                effect(DeviceMeshEffect::SET_WORK_WHILE_CHARGING),
            ),
            room(
                "processors",
                "mesh-work-room-cpu",
                "mesh-work-room-cpu-desc",
                when.free_cpu_percent,
            ),
            room(
                "memory",
                "mesh-work-room-memory",
                "mesh-work-room-memory-desc",
                when.free_memory_percent,
            ),
        ]
    }

    fn service_rows(&self) -> [provider_settings_ui::widgets::Item; 4] {
        [
            self.service_row(
                "mesh-svc-server",
                "mesh-svc-server-desc",
                "inbound_server",
                "device_mesh.inbound_server",
                self.settings.services.inbound_server,
            ),
            self.service_row(
                "mesh-svc-remote",
                "mesh-svc-remote-desc",
                "remote_control_server",
                "device_mesh.remote_server",
                self.settings.services.remote_control_server,
            ),
            self.service_row(
                "mesh-svc-apps",
                "mesh-svc-apps-desc",
                "app_serving",
                "device_mesh.app_serving",
                self.settings.services.app_serving,
            ),
            self.service_row(
                "mesh-svc-jobs",
                "mesh-svc-jobs-desc",
                "compute",
                "device_mesh.compute",
                self.settings.services.compute,
            ),
        ]
    }

    fn home_peer_rows(
        &self,
        peer: &tilo_server_protocol::servers::PersonServer,
    ) -> Vec<provider_settings_ui::widgets::Item> {
        let device = peer.device.clone();
        let name = self.bind_on_row(
            &home_server_row(peer),
            EditableLabel::new(&peer.label)
                .with_id(&peer_key("name", &peer.device))
                .with_accessible_name(t("mesh-name"))
                .edits_on_click(),
            effect(DeviceMeshEffect::RENAME_HOME_SERVER)
                .map(move |name: String| (device.clone(), name.trim().to_owned())),
        );
        let health = self.home_peer_health.get(&peer.device).copied();
        let badge = health.map_or_else(|| t("mesh-badge-checking"), peer_health_badge);
        let detail = health.map_or_else(|| t("mesh-checking"), peer_health_detail);
        let tone = match health {
            Some(HomePeerHealth::Online) => StatusTone::Success,
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
        let live = home_server_row(peer);
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
                &peer_key("remove", &peer.device),
                &t("mesh-remove-home-confirm"),
            )
            .role(ButtonRole::Destructive),
            &live,
            DeviceMeshEffect::RemoveHomeServer {
                device: peer.device.clone(),
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
