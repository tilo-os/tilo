//! This device's VPN choices (`device_vpn`): whether the person's other
//! devices may route through it, which of their devices it routes through,
//! and the kill switches of their VPN profiles. The session follows them and
//! publishes what it does; every change is one of the section's actions, so
//! Settings, quick settings, the palette and an agent make it the same way,
//! as a step the person undoes.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use settings_store::device_vpn::{ProfileKillSwitch, ProfileRelease, StopRequest, Through};
use ui::tilo_resource::{self, Refusal};

/// This device's VPN choices.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model,
)]
#[model(defaults)]
#[serde(default)]
pub struct DeviceVpnPrefs {
    /// Let the person's other devices route their traffic through this one.
    pub exit: bool,
    /// The device this one routes through, and how; changed by
    /// `route-through` and `set-kill-switch`.
    #[model(readonly)]
    pub through: Option<Through>,
    /// The last request to stop a device routing through this one; made by
    /// `stop`.
    #[model(readonly)]
    pub stop: Option<StopRequest>,
    /// The VPN profiles whose traffic is blocked while they are set to
    /// connect and are not connected, by profile id; changed by
    /// `set-profile-kill-switch`.
    #[model(readonly)]
    pub profile_kill_switches: BTreeMap<String, ProfileKillSwitch>,
    /// The last profile turned off on purpose, whose block lifts; made by
    /// `release-profile`.
    #[model(readonly)]
    pub profile_release: Option<ProfileRelease>,
}

impl ui::storage::SettingsSchema for DeviceVpnPrefs {
    const SCOPE: ui::storage::Scope =
        ui::storage::Scope::Shared(settings_store::device_vpn::SECTION);
}

#[tilo_resource::resource(kind = "settings.device-vpn", label = "Your devices as a VPN")]
impl DeviceVpnPrefs {
    /// Route this device's traffic through another of the person's devices,
    /// keeping how it routes, or stop routing through it.
    #[action(id = "route-through", keywords = "vpn, tunnel, exit node")]
    pub fn route_through(
        &mut self,
        #[param("The other device's id")] device: String,
        on: bool,
    ) -> Result<(), Refusal> {
        self.through = if on {
            Some(Through {
                device,
                ..self.through.clone().unwrap_or_default()
            })
        } else {
            self.through
                .take()
                .filter(|through| through.device != device)
        };
        Ok(())
    }

    /// Block this device's traffic while the tunnel to the device it routes
    /// through is down, or let it go out directly.
    #[action(id = "set-kill-switch")]
    pub fn set_kill_switch(&mut self, on: bool) -> Result<(), Refusal> {
        let through = self
            .through
            .as_mut()
            .ok_or_else(|| Refusal::new("this device routes through none of yours"))?;
        through.kill_switch = on;
        Ok(())
    }

    /// Stop another of the person's devices routing through this one.
    #[action(id = "stop")]
    pub fn stop_device(
        &mut self,
        #[param("The other device's id")] device: String,
    ) -> Result<(), Refusal> {
        self.stop = Some(StopRequest::after(self.stop.as_ref(), device));
        Ok(())
    }

    /// Block a VPN profile's traffic while it is set to connect and is not
    /// connected, or not; and whether the local network stays reachable
    /// meanwhile (kept as it was when omitted).
    #[action(id = "set-profile-kill-switch")]
    pub fn set_profile_kill_switch(
        &mut self,
        #[param("The VPN profile's id")] profile: String,
        on: bool,
        #[param("Keep printers, file shares and the router reachable")] allow_local: Option<bool>,
    ) -> Result<(), Refusal> {
        if on {
            let kept = self
                .profile_kill_switches
                .get(&profile)
                .is_some_and(|switch| switch.allow_local);
            self.profile_kill_switches.insert(
                profile,
                ProfileKillSwitch {
                    allow_local: allow_local.unwrap_or(kept),
                },
            );
        } else {
            self.profile_kill_switches.remove(&profile);
        }
        Ok(())
    }

    /// Lift the block of a VPN profile the person turned off on purpose.
    #[action(id = "release-profile")]
    pub fn release_profile(
        &mut self,
        #[param("The VPN profile's id")] profile: String,
    ) -> Result<(), Refusal> {
        self.profile_release = Some(ProfileRelease::after(
            self.profile_release.as_ref(),
            profile,
        ));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::tilo_resource::Replica;

    /// Routing through another device keeps how it routes; turning it off
    /// leaves a route through a different device alone; the kill switch
    /// needs a route; every stop and release is a request of its own.
    #[test]
    fn the_choices_change_only_through_their_actions() {
        let mut vpn = Replica::<DeviceVpnPrefs>::new();
        vpn.apply(DeviceVpnPrefs::ROUTE_THROUGH.with(("desk".to_owned(), true)))
            .unwrap();
        vpn.apply(DeviceVpnPrefs::SET_KILL_SWITCH.with(true))
            .unwrap();
        vpn.apply(DeviceVpnPrefs::ROUTE_THROUGH.with(("laptop".to_owned(), true)))
            .unwrap();
        let through = vpn.state().unwrap().through.unwrap();
        assert_eq!(through.device, "laptop");
        assert!(through.kill_switch, "how it routes is kept");
        vpn.apply(DeviceVpnPrefs::ROUTE_THROUGH.with(("desk".to_owned(), false)))
            .unwrap();
        assert!(vpn.state().unwrap().through.is_some());
        vpn.apply(DeviceVpnPrefs::ROUTE_THROUGH.with(("laptop".to_owned(), false)))
            .unwrap();
        assert!(vpn.state().unwrap().through.is_none());
        assert!(
            vpn.apply(DeviceVpnPrefs::SET_KILL_SWITCH.with(true))
                .is_err()
        );

        vpn.apply(DeviceVpnPrefs::STOP_DEVICE.with("phone".to_owned()))
            .unwrap();
        vpn.apply(DeviceVpnPrefs::STOP_DEVICE.with("phone".to_owned()))
            .unwrap();
        assert_eq!(vpn.state().unwrap().stop.unwrap().id, 2);

        vpn.apply(DeviceVpnPrefs::SET_PROFILE_KILL_SWITCH.with((
            "work".to_owned(),
            true,
            Some(true),
        )))
        .unwrap();
        vpn.apply(DeviceVpnPrefs::SET_PROFILE_KILL_SWITCH.with(("work".to_owned(), true, None)))
            .unwrap();
        assert!(vpn.state().unwrap().profile_kill_switches["work"].allow_local);
        vpn.apply(DeviceVpnPrefs::RELEASE_PROFILE.with("work".to_owned()))
            .unwrap();
        assert_eq!(
            vpn.state().unwrap().profile_release.unwrap().profile,
            "work"
        );
    }
}
