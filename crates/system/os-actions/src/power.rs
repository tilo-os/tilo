//! Power profile control through power-profiles-daemon's D-Bus interface.

use tilo_power_mgmt::PowerProfilesMonitor;

/// Power profile, mirrors `shell::quick_settings::PowerProfile` —
/// duplicated so this crate doesn't need to import the popover state
/// type, and so unit tests don't need any UI scaffolding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerProfile {
    Saver,
    Balanced,
    Performance,
}

impl PowerProfile {
    /// The daemon's name for the profile.
    pub fn cli_arg(self) -> &'static str {
        match self {
            PowerProfile::Saver => "power-saver",
            PowerProfile::Balanced => "balanced",
            PowerProfile::Performance => "performance",
        }
    }

    /// Inverse of [`Self::cli_arg`]; used by [`get_power_profile`].
    pub fn from_cli(s: &str) -> Option<Self> {
        match s.trim() {
            "power-saver" => Some(PowerProfile::Saver),
            "balanced" => Some(PowerProfile::Balanced),
            "performance" => Some(PowerProfile::Performance),
            _ => None,
        }
    }
}

/// The power-profiles daemon on the system bus.
async fn daemon() -> Result<PowerProfilesMonitor, String> {
    let bus = zbus::Connection::system()
        .await
        .map_err(|e| format!("cannot reach the system bus: {e}"))?;
    PowerProfilesMonitor::with_connection(&bus)
        .await
        .map_err(|e| format!("power-profiles-daemon: {e}"))
}

/// Set the active power profile.
pub fn set_power_profile(profile: PowerProfile) -> Result<(), String> {
    crate::block_on(async move {
        daemon()
            .await?
            .set_active_profile(profile.cli_arg())
            .await
            .map_err(|e| format!("power-profiles-daemon refused {}: {e}", profile.cli_arg()))
    })
}

/// Read the active power profile.
pub fn get_power_profile() -> Result<PowerProfile, String> {
    let active = crate::block_on(async {
        daemon()
            .await?
            .active_profile()
            .await
            .map_err(|e| format!("power-profiles-daemon: {e}"))
    })?;
    PowerProfile::from_cli(&active)
        .ok_or_else(|| format!("power-profiles-daemon reports an unknown profile: {active:?}"))
}

/// Return only the profiles the running power-profiles daemon advertises.
///
/// The daemon's `Profiles` list is deliberately treated as the capability
/// source; callers must not offer a profile merely because it is part of the
/// protocol.
pub fn available_power_profiles() -> Result<Vec<PowerProfile>, String> {
    let names = crate::block_on(async {
        daemon()
            .await?
            .profiles()
            .await
            .map_err(|e| format!("power-profiles-daemon: {e}"))
    })?;
    advertised_profiles(&names)
}

fn advertised_profiles(names: &[String]) -> Result<Vec<PowerProfile>, String> {
    let mut profiles = Vec::new();
    for profile in names.iter().filter_map(|name| PowerProfile::from_cli(name)) {
        if !profiles.contains(&profile) {
            profiles.push(profile);
        }
    }
    if profiles.is_empty() {
        Err(format!(
            "power-profiles-daemon advertised no supported profile: {names:?}"
        ))
    } else {
        Ok(profiles)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_profiles_round_trip_and_only_advertised_ones_parse() {
        {
            assert_eq!(PowerProfile::Saver.cli_arg(), "power-saver");
            assert_eq!(PowerProfile::Balanced.cli_arg(), "balanced");
            assert_eq!(PowerProfile::Performance.cli_arg(), "performance");
            assert_eq!(
                PowerProfile::from_cli("balanced"),
                Some(PowerProfile::Balanced)
            );
            assert_eq!(
                PowerProfile::from_cli("performance\n"),
                Some(PowerProfile::Performance)
            );
            assert_eq!(PowerProfile::from_cli("garbled"), None);
        }
        {
            let names = |list: &[&str]| list.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>();
            assert_eq!(
                advertised_profiles(&names(&[
                    "performance",
                    "balanced",
                    "power-saver",
                    "balanced"
                ]))
                .unwrap(),
                vec![
                    PowerProfile::Performance,
                    PowerProfile::Balanced,
                    PowerProfile::Saver
                ]
            );
            assert!(advertised_profiles(&names(&["placeholder"])).is_err());
        }
    }
}
