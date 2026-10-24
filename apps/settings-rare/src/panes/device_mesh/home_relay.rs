//! Devices & Sync's home relay section: per workspace this device hosts, a
//! switch for whether the person's home relay hosts it too.
//!
//! What each switch shows is the room's own list of hosts, as the host
//! answers it through its `tilo.hosting` actions; flipping one asks the host
//! to add the relay as a host of that room or take it off. Nothing is kept
//! here but the host's last answer.

use ui::tilo_actions::remote::InvokeArgument;
use ui::tilo_actions::remote::home_relay_hosting::{self, HomeRelayHosting};

use super::{DeviceMeshEffect, DeviceMeshModel};
use crate::effects::effect;
use crate::i18n::{t, t_args};
use crate::widgets::{SettingsSection, error_caption, note, row, rows, section};

/// What the section knows of the host's home-relay hosting.
#[derive(Default)]
pub(super) struct HomeRelay {
    /// The host's last answer; `None` before it answered.
    pub(super) hosting: Option<HomeRelayHosting>,
    /// The host's answer to the list, while it is asked.
    listing: Option<ui::InvocationCall>,
    /// A choice the host carries out, and its room.
    choosing: Option<(ui::InvocationCall, String)>,
    /// Why the last ask failed.
    error: Option<String>,
}

impl HomeRelay {
    /// Whether the host still owes an answer.
    pub(super) const fn waiting(&self) -> bool {
        self.listing.is_some() || self.choosing.is_some()
    }
}

impl DeviceMeshModel {
    /// Ask the host which workspaces its home relay hosts.
    pub(super) fn refresh_home_relay(&mut self) {
        if self.home_relay.listing.is_none() {
            self.home_relay.listing = Some(self.remote.invoke(
                home_relay_hosting::target(home_relay_hosting::WORKSPACES),
                InvokeArgument::None,
                None,
            ));
        }
    }

    /// Ask the host to have the home relay host `room` as well, or no
    /// longer. The switch shows the choice at once; the host's answer
    /// confirms it or puts it back.
    pub(super) fn choose_home_relay(&mut self, room: &str, on: bool) -> Result<(), String> {
        if self.home_relay.choosing.is_some() {
            return Err("the home relay is still being changed".into());
        }
        let workspace = self
            .home_relay
            .hosting
            .as_mut()
            .and_then(|hosting| {
                hosting
                    .workspaces
                    .iter_mut()
                    .find(|workspace| workspace.room == room)
            })
            .ok_or("that workspace is not hosted on this device")?;
        workspace.on_relay = on;
        let mut arguments = serde_json::Map::new();
        arguments.insert("room".into(), room.into());
        arguments.insert("on".into(), on.into());
        let call = self.remote.invoke(
            home_relay_hosting::target(home_relay_hosting::SET),
            InvokeArgument::Json(arguments),
            None,
        );
        self.home_relay.choosing = Some((call, room.to_owned()));
        self.home_relay.error = None;
        Ok(())
    }

    /// Take in the host's answers to the list and to a choice, once each
    /// settled.
    pub(super) fn poll_home_relay(&mut self) {
        if let Some(answer) = self
            .home_relay
            .listing
            .as_ref()
            .and_then(crate::device_fleet::settled)
        {
            self.home_relay.listing = None;
            match answer.and_then(|output| {
                serde_json::from_value::<HomeRelayHosting>(
                    output.value.unwrap_or(serde_json::Value::Null),
                )
                .map_err(|_| "the host's answer about the home relay is not readable".into())
            }) {
                Ok(hosting) => {
                    self.home_relay.hosting = Some(hosting);
                    self.home_relay.error = None;
                }
                Err(error) => self.home_relay.error = Some(error),
            }
            self.changed = true;
        }
        if let Some(answer) = self
            .home_relay
            .choosing
            .as_ref()
            .and_then(|(call, _)| crate::device_fleet::settled(call))
        {
            self.home_relay.choosing = None;
            if let Err(error) = answer {
                self.home_relay.error = Some(error);
            }
            // What the room's hosts are now, either way.
            self.refresh_home_relay();
            self.changed = true;
        }
    }

    /// The section, once the host answered that the person has a home
    /// relay; nothing to show without one.
    pub(super) fn home_relay_section(&self) -> Option<SettingsSection> {
        let hosting = self.home_relay.hosting.as_ref()?;
        let relay = hosting.relay.as_deref()?;
        let mut items: Vec<_> = hosting
            .workspaces
            .iter()
            .map(|workspace| {
                let room = workspace.room.clone();
                self.cx
                    .row(workspace.name.clone())
                    .described(t_args("mesh-home-relay-also", &[("relay", relay.into())]))
                    .id(format!("device_mesh.home_relay.{}", workspace.room))
                    .toggle_effect(
                        &ui::LocalState::new(workspace.on_relay),
                        effect(DeviceMeshEffect::SET_HOME_RELAY_HOSTING)
                            .map(move |on: bool| (room.clone(), on)),
                    )
            })
            .collect();
        if items.is_empty() {
            items.push(note(&t("mesh-home-relay-none")));
        }
        if let Some(error) = &self.home_relay.error {
            items.push(row(error_caption(error)));
        }
        items.push(note(&t("mesh-home-relay-caption")));
        Some(section(&t("mesh-home-relay-title"), rows(items)))
    }
}
