//! The Sync between devices page's home relay section: per workspace this
//! device hosts, a switch for whether the person's home relay hosts it too.
//! What each switch shows is the workspace's own list of hosts, as the host
//! answers it through its `tilo.hosting` actions; flipping one asks the host to
//! add the relay as a host of that workspace or take it off.

use ui::framework::{EffectReply, LiveRow};
use ui::tilo_actions::ResourcePresentation;
use ui::tilo_actions::remote::InvokeArgument;
use ui::tilo_actions::remote::home_relay_hosting::{self, HomeRelayHosting, HostedWorkspace};
use ui::tilo_resource::Effect as _;

use super::{DeviceMeshEffect, DeviceMeshModel, Page};
use crate::effects::{SettingsEffect, effect};
use crate::i18n::{t, t_args};
use provider_settings_ui::widgets::{SettingsSection, error_caption, note, row, rows, section};

#[derive(Default)]
pub(super) struct HomeRelay {
    pub(super) hosting: Option<HomeRelayHosting>,
    listing: Option<ui::InvocationCall>,
    choosing: Option<(ui::InvocationCall, Option<EffectReply>)>,
    /// Why the last ask failed.
    error: Option<String>,
}

impl HomeRelay {
    /// Whether the host still owes an answer.
    pub(super) const fn waiting(&self) -> bool {
        self.listing.is_some() || self.choosing.is_some()
    }

    pub(super) fn relay(&self) -> Option<(&HomeRelayHosting, &str)> {
        let hosting = self.hosting.as_ref()?;
        Some((hosting, hosting.relay.as_deref()?))
    }
}

impl DeviceMeshModel {
    pub(super) fn refresh_home_relay(&mut self) {
        if self.home_relay.listing.is_none() {
            self.home_relay.listing = Some(self.remote.invoke(
                home_relay_hosting::target(home_relay_hosting::WORKSPACES),
                InvokeArgument::None,
                None,
            ));
        }
    }

    pub(super) fn choose_home_relay(
        &mut self,
        workspace: &str,
        on: bool,
        reply: Option<EffectReply>,
    ) -> Result<(), String> {
        if self.home_relay.choosing.is_some() {
            return Err("the home relay is still being changed".into());
        }
        let hosted = self
            .home_relay
            .hosting
            .as_mut()
            .and_then(|hosting| {
                hosting
                    .workspaces
                    .iter_mut()
                    .find(|hosted| hosted.workspace == workspace)
            })
            .ok_or("that workspace is not hosted on this device")?;
        hosted.on_relay = on;
        let mut arguments = serde_json::Map::new();
        // The workspace under the key the host's step already reads.
        arguments.insert("room".into(), workspace.into());
        arguments.insert("on".into(), on.into());
        let call = self.remote.invoke(
            home_relay_hosting::target(home_relay_hosting::SET),
            InvokeArgument::Json(arguments),
            None,
        );
        self.home_relay.choosing = Some((call, reply));
        self.home_relay.error = None;
        Ok(())
    }

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
            let reply = self.home_relay.choosing.take().and_then(|(_, reply)| reply);
            if let Err(error) = &answer {
                self.home_relay.error = Some(error.clone());
            }
            if let Some(reply) = reply {
                reply.finish(answer);
            }
            self.refresh_home_relay();
            self.changed = true;
        }
    }
}

impl DeviceMeshModel {
    pub(super) fn home_relay_rows(&self) -> Vec<LiveRow> {
        let Some((hosting, relay)) = self.home_relay.relay() else {
            return Vec::new();
        };
        hosting
            .workspaces
            .iter()
            .map(|workspace| home_relay_row(workspace, relay))
            .collect()
    }
}

impl Page<'_> {
    pub(super) fn home_relay_section(&self) -> Option<SettingsSection> {
        let (hosting, relay) = self.home_relay.relay()?;
        let mut items: Vec<_> = hosting
            .workspaces
            .iter()
            .map(|workspace| {
                let workspace_id = workspace.workspace.clone();
                self.switch_on_row(
                    workspace.name.clone(),
                    t_args("mesh-home-relay-also", &[("relay", relay.into())]),
                    &format!("device_mesh.home_relay.{}", workspace.workspace),
                    workspace.on_relay,
                    &home_relay_row(workspace, relay).into_resource(),
                    effect(DeviceMeshEffect::SET_HOME_RELAY_HOSTING)
                        .map(move |on: bool| (workspace_id.clone(), on)),
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

pub(super) fn home_relay_row_id(workspace: &str) -> String {
    format!("settings.home-relay-workspace:{workspace}")
}

fn home_relay_row(hosted: &HostedWorkspace, relay: &str) -> LiveRow {
    let switch = SettingsEffect::from(DeviceMeshEffect::SetHomeRelayHosting {
        workspace: hosted.workspace.clone(),
        on: true,
    });
    let row = LiveRow::new(
        home_relay_row_id(&hosted.workspace),
        "settings.home-relay-workspace",
        hosted.name.clone(),
    )
    .operation(switch.bound_asking(&["on"]));
    if hosted.on_relay {
        row.presentation(ResourcePresentation {
            detail: Some(t_args("mesh-home-relay-also", &[("relay", relay.into())])),
            ..ResourcePresentation::default()
        })
    } else {
        row
    }
}
