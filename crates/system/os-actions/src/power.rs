//! Power profile control via `powerprofilesctl`.

use crate::process::{run, run_capture};

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
    /// Stringly-typed cli argument used by `powerprofilesctl set <name>`.
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

/// Set the active power profile via `powerprofilesctl set <profile>`.
pub fn set_power_profile(profile: PowerProfile) -> Result<(), String> {
    run("powerprofilesctl", &["set", profile.cli_arg()])
}

/// Read the active power profile via `powerprofilesctl get`.
pub fn get_power_profile() -> Result<PowerProfile, String> {
    let out = run_capture("powerprofilesctl", &["get"])?;
    PowerProfile::from_cli(out.trim())
        .ok_or_else(|| format!("powerprofilesctl get unexpected: {out:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_profile_round_trip() {
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
}
