//! A VPN profile's kill switch (docs/device-vpn.md, "Profile kill switch"):
//! while the profile is set to connect and is not connected, nothing leaves
//! the computer but through a tunnel.
//!
//! Settings changes the person's choice and tells the profile they turned
//! off on purpose only through their section's actions
//! ([`DeviceVpnPrefs`]); the session
//! holds the block through the root helper and publishes which profiles
//! block, which the profile page shows as "Blocking traffic".

use settings_store::device_vpn::{self, PROFILE_KILL_SWITCHES, ProfileKillSwitch};
use ui::LocalState;
use ui::widget::Toggle;

use super::super::{Begun, NetworkModel, Page, VpnInfo};
use super::VpnEffect;
use crate::effects::effect;
use crate::i18n::t;
use crate::prefs::DeviceVpnPrefs;
use provider_settings_ui::widgets::{Item, error_caption, row, setting_row_described};

impl NetworkModel {
    /// The kill switch of the profile `uuid`, while it is on.
    pub(super) fn kill_switch(&self, uuid: &str) -> Option<ProfileKillSwitch> {
        let document = self.reach.storage.settings_document();
        PROFILE_KILL_SWITCHES
            .from_section(&document.section(device_vpn::SECTION))
            .remove(uuid)
    }

    /// Whether the session holds the profile `uuid`'s block now.
    pub(super) fn blocking(&self, uuid: &str) -> bool {
        self.vpn.devices.status.as_ref().is_some_and(|status| {
            status
                .profile_block
                .blocking
                .iter()
                .any(|profile| profile == uuid)
        })
    }

    /// Whether `vpn` is set to connect as its switch shows it: connected, or
    /// blocking traffic until it is.
    pub(super) fn set_to_connect(&self, vpn: &VpnInfo) -> bool {
        vpn.active || self.blocking(&vpn.uuid)
    }

    /// Turn `connection`'s kill switch on or off; `allow_local`, when given,
    /// lets the local network through while it blocks.
    pub(super) fn set_kill_switch(
        &mut self,
        connection: &str,
        on: bool,
        allow_local: Option<bool>,
    ) -> Result<Begun, String> {
        let vpn = self.vpn(connection)?;
        let previous = self.kill_switch(&vpn.uuid);
        let next = on.then(|| ProfileKillSwitch {
            allow_local: allow_local
                .unwrap_or_else(|| previous.is_some_and(|switch| switch.allow_local)),
        });
        if next == previous {
            return Ok(Begun::Done);
        }
        self.reach
            .sections
            .invoke(DeviceVpnPrefs::SET_PROFILE_KILL_SWITCH.with((
                vpn.uuid.clone(),
                on,
                allow_local,
            )))?;
        if next.is_none() {
            self.unblock(&vpn.uuid);
        }
        self.changed = true;
        Ok(Begun::Done)
    }

    /// Tell the session the person turned the profile `uuid` off on purpose,
    /// so its block lifts; only a profile with its kill switch on has one.
    pub(super) fn release_profile(&self, uuid: &str) -> Result<(), String> {
        if self.kill_switch(uuid).is_none() {
            return Ok(());
        }
        self.reach
            .sections
            .invoke(DeviceVpnPrefs::RELEASE_PROFILE.with(uuid.to_owned()))
    }

    /// Show the profile `uuid` as no longer blocking until the session
    /// publishes where it stands; whether it was.
    pub(super) fn unblock(&mut self, uuid: &str) -> bool {
        let Some(status) = &mut self.vpn.devices.status else {
            return false;
        };
        let before = status.profile_block.blocking.len();
        status
            .profile_block
            .blocking
            .retain(|profile| profile != uuid);
        let was = status.profile_block.blocking.len() != before;
        self.changed |= was;
        was
    }

    /// Forget the kill switch of the profile `uuid`, deleted.
    pub(super) fn forget_kill_switch(&mut self, uuid: &str) {
        if let Err(error) = self.release_profile(uuid).and_then(|()| {
            if self.kill_switch(uuid).is_none() {
                return Ok(());
            }
            self.reach
                .sections
                .invoke(DeviceVpnPrefs::SET_PROFILE_KILL_SWITCH.with((
                    uuid.to_owned(),
                    false,
                    None,
                )))
        }) {
            tracing::warn!(target: "tilo::settings", %error, "a deleted VPN's kill switch stayed");
        }
        self.unblock(uuid);
    }
}

impl Page<'_> {
    /// The profile page's kill switch, and while it is on whether the local
    /// network stays reachable, and the session's failure to block.
    pub(super) fn kill_switch_rows(
        &self,
        vpn: &VpnInfo,
        live: &ui::tilo_actions::ResourceSpec,
    ) -> Vec<Item> {
        let switch = self.kill_switch(&vpn.uuid);
        let label = t("net-vpn-kill-switch");
        let toggle = self.bind_on_row(
            live,
            Toggle::from_state(&LocalState::new(switch.is_some()))
                .with_id(&format!("settings.network.vpn.kill-switch.{}", vpn.uuid))
                .with_accessible_name(&label),
            effect(VpnEffect::SET_KILL_SWITCH).map({
                let connection = vpn.uuid.clone();
                move |on: bool| (connection.clone(), on, None)
            }),
        );
        let mut out = vec![setting_row_described(
            &label,
            &t("net-vpn-kill-switch-caption"),
            Box::new(toggle),
        )];
        let Some(switch) = switch else {
            return out;
        };
        let local = t("net-vpn-allow-local");
        let toggle = self.bind_on_row(
            live,
            Toggle::from_state(&LocalState::new(switch.allow_local))
                .with_id(&format!("settings.network.vpn.allow-local.{}", vpn.uuid))
                .with_accessible_name(&local),
            effect(VpnEffect::SET_KILL_SWITCH).map({
                let connection = vpn.uuid.clone();
                move |allow: bool| (connection.clone(), true, Some(allow))
            }),
        );
        out.push(setting_row_described(
            &local,
            &t("net-vpn-allow-local-caption"),
            Box::new(toggle),
        ));
        let failed = self
            .vpn
            .devices
            .status
            .as_ref()
            .is_some_and(|status| status.profile_block.failed);
        if failed {
            out.push(row(error_caption(&t("net-vpn-kill-switch-failed"))));
        }
        out
    }
}

impl NetworkModel {
    /// The sample tunnel `uuid` with its kill switch on, for a capture: up,
    /// or dropped with its block holding (and the local network let
    /// through).
    pub(crate) fn sample_kill_switch(&mut self, uuid: &str, blocking: bool) {
        let switch = ProfileKillSwitch {
            allow_local: blocking,
        };
        // A capture's fixture: written as it would be stored.
        let document = self.reach.storage.settings_document();
        let mut switches =
            PROFILE_KILL_SWITCHES.from_section(&document.section(device_vpn::SECTION));
        switches.insert(uuid.to_owned(), switch);
        let mut values = serde_json::Map::new();
        values.insert(
            PROFILE_KILL_SWITCHES.name().to_owned(),
            serde_json::to_value(&switches).unwrap_or_default(),
        );
        if let Err(error) = document.update_section(device_vpn::SECTION, values) {
            tracing::warn!(target: "tilo::settings", %error, "the sample kill switch stayed off");
        }
        if !blocking {
            return;
        }
        for vpn in &mut self.vpns {
            if vpn.uuid == uuid {
                vpn.active = false;
            }
        }
        self.vpn
            .devices
            .status
            .get_or_insert_with(device_vpn::DeviceVpnStatus::default)
            .profile_block
            .blocking = vec![uuid.to_owned()];
    }
}
