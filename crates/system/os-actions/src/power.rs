//! Power profile control through power-profiles-daemon's D-Bus interface.

use tilo_power_mgmt::PowerProfilesMonitor;

/// Mirrors the shell's `PowerProfile`, duplicated so this crate needs no popover state type and tests
/// need no UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerProfile {
    Saver,
    Balanced,
    Performance,
}

impl PowerProfile {
    pub fn cli_arg(self) -> &'static str {
        match self {
            PowerProfile::Saver => "power-saver",
            PowerProfile::Balanced => "balanced",
            PowerProfile::Performance => "performance",
        }
    }

    pub fn from_cli(s: &str) -> Option<Self> {
        match s.trim() {
            "power-saver" => Some(PowerProfile::Saver),
            "balanced" => Some(PowerProfile::Balanced),
            "performance" => Some(PowerProfile::Performance),
            _ => None,
        }
    }
}

async fn daemon() -> Result<PowerProfilesMonitor, String> {
    let bus = zbus::Connection::system()
        .await
        .map_err(|e| format!("cannot reach the system bus: {e}"))?;
    PowerProfilesMonitor::with_connection(&bus)
        .await
        .map_err(|e| format!("power-profiles-daemon: {e}"))
}

pub fn set_power_profile(profile: PowerProfile) -> Result<(), String> {
    crate::block_on(async move {
        daemon()
            .await?
            .set_active_profile(profile.cli_arg())
            .await
            .map_err(|e| format!("power-profiles-daemon refused {}: {e}", profile.cli_arg()))
    })
}

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

/// Only the profiles the daemon advertises: its `Profiles` list is the capability source, so callers do
/// not offer a profile it leaves out.
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
