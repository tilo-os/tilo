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

/// Return only the profiles the running power-profiles daemon advertises.
///
/// `powerprofilesctl list` is deliberately treated as the capability source;
/// callers must not offer a profile merely because it is part of the protocol.
pub fn available_power_profiles() -> Result<Vec<PowerProfile>, String> {
    let out = run_capture("powerprofilesctl", &["list"])?;
    parse_available_profiles(&out)
}

fn parse_available_profiles(out: &str) -> Result<Vec<PowerProfile>, String> {
    let mut profiles = Vec::new();
    for line in out.lines() {
        let candidate = line
            .trim()
            .strip_prefix('*')
            .unwrap_or_else(|| line.trim())
            .trim()
            .strip_suffix(':')
            .unwrap_or("");
        if let Some(profile) = PowerProfile::from_cli(candidate)
            && !profiles.contains(&profile)
        {
            profiles.push(profile);
        }
    }
    if profiles.is_empty() {
        Err(format!(
            "powerprofilesctl list did not advertise a supported profile: {out:?}"
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
            let output = r"  performance:
        Driver: platform_profile
    * balanced:
        Driver: platform_profile
      power-saver:
        Driver: platform_profile
    ";
            assert_eq!(
                parse_available_profiles(output).unwrap(),
                vec![
                    PowerProfile::Performance,
                    PowerProfile::Balanced,
                    PowerProfile::Saver
                ]
            );
        }
        {
            assert!(parse_available_profiles("Driver: placeholder\n").is_err());
        }
    }
}
