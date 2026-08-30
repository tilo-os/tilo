//! Reading the [`PowerPolicy`] from the shared `settings.json`.
//!
//! The Settings app owns the *write* side (its `PowerPrefs` serde struct);
//! this module owns the compositor's *read* side. The contract between them
//! is the JSON on disk — a `"power"` object — **not** a shared crate, exactly
//! like the compositor's `load_display_scale`. Keeping the reader here (next
//! to the policy it produces) means the compositor never re-implements the
//! parse, and a cross-consistency test pins the two sides together.
//!
//! ```json
//! "power": {
//!   "auto_suspend": true,
//!   "blank_minutes": 10,
//!   "suspend_minutes": 15,
//!   "lid_close_action": "suspend",
//!   "power_key_action": "suspend",
//!   "low_battery_percent": 15,
//!   "critical_battery_percent": 5,
//!   "critical_action": "suspend"
//! }
//! ```
//!
//! Every field is optional: a missing or malformed value falls back to the
//! [`PowerPolicy`] / Settings default so a corrupt config can never leave the
//! machine unmanaged (or, worse, wedge it into an aggressive suspend).

use std::path::PathBuf;
use std::time::Duration;

use crate::policy::{PowerAction, PowerPolicy};

/// The Settings default idle-suspend timeout, in minutes, used when
/// `auto_suspend` is on but no explicit `suspend_minutes` is written.
pub const DEFAULT_SUSPEND_MINUTES: u32 = 15;

/// Resolve the shared settings path (`$XDG_CONFIG_HOME/qos/settings.json`,
/// falling back to `~/.config/qos/settings.json`) — the same file the
/// Settings app and the rest of the compositor read.
pub fn settings_path() -> Option<PathBuf> {
    settings_store::settings_path()
}

/// Load the policy from the shared settings file, falling back to the
/// Settings defaults on any read/parse failure.
pub fn load_policy() -> PowerPolicy {
    load_policy_for_profile(active_power_profile().as_deref())
}

/// Load Settings policy for a profile already obtained from
/// power-profiles-daemon. This avoids spawning its CLI from the D-Bus monitor.
pub fn load_policy_for_profile(active_profile: Option<&str>) -> PowerPolicy {
    let Some(path) = settings_path() else {
        return default_policy();
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => policy_from_settings_json_for_profile(&text, active_profile),
        Err(_) => default_policy(),
    }
}

/// The policy a freshly-installed machine runs (Settings defaults). This
/// differs from [`PowerPolicy::default`] in one place: idle-suspend is
/// **enabled** (15 min), matching the Settings "suspend when idle" toggle
/// defaulting on.
pub fn default_policy() -> PowerPolicy {
    PowerPolicy {
        idle_suspend: Some(Duration::from_secs(u64::from(DEFAULT_SUSPEND_MINUTES) * 60)),
        ..PowerPolicy::default()
    }
}

/// Parse a [`PowerPolicy`] from a full `settings.json` document. Never fails;
/// unknown/missing fields take defaults.
pub fn policy_from_settings_json(text: &str) -> PowerPolicy {
    policy_from_settings_json_for_profile(text, active_power_profile().as_deref())
}

pub fn policy_from_settings_json_for_profile(
    text: &str,
    active_profile: Option<&str>,
) -> PowerPolicy {
    let root: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return default_policy(),
    };
    let Some(section) = root.get("power") else {
        return default_policy();
    };
    policy_from_section_for_profile(section, active_profile)
}

/// Parse a [`PowerPolicy`] from just the `"power"` object.
pub fn policy_from_section(section: &serde_json::Value) -> PowerPolicy {
    policy_from_section_for_profile(section, active_power_profile().as_deref())
}

/// Resolve a policy for a known power-profiles-daemon profile.
///
/// This separate entry point keeps parsing deterministic in tests and makes
/// the Settings/compositor contract explicit. Unknown profiles retain the
/// legacy `suspend_minutes` value.
pub fn policy_from_section_for_profile(
    section: &serde_json::Value,
    active_profile: Option<&str>,
) -> PowerPolicy {
    let defaults = default_policy();

    let auto_suspend = section
        .get("auto_suspend")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    let legacy_suspend_minutes = section
        .get("suspend_minutes")
        .and_then(serde_json::Value::as_u64)
        .map_or(DEFAULT_SUSPEND_MINUTES, |m| m as u32);
    let profile_key = match active_profile {
        Some("power-saver") => Some("saver_suspend_minutes"),
        Some("balanced") => Some("balanced_suspend_minutes"),
        Some("performance") => Some("performance_suspend_minutes"),
        _ => None,
    };
    let suspend_minutes = profile_key
        .and_then(|key| section.get(key))
        .and_then(serde_json::Value::as_u64)
        .map_or(legacy_suspend_minutes, |m| m as u32);

    // `auto_suspend` off, or a 0-minute timeout, disables idle-suspend.
    let idle_suspend = if auto_suspend && suspend_minutes > 0 {
        Some(Duration::from_secs(u64::from(suspend_minutes) * 60))
    } else {
        None
    };

    let action = |key: &str, fallback: PowerAction| -> PowerAction {
        section
            .get(key)
            .and_then(|v| v.as_str())
            .and_then(PowerAction::from_label)
            .unwrap_or(fallback)
    };
    let percent = |key: &str, fallback: u8| -> u8 {
        section
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .map_or(fallback, |p| p.min(100) as u8)
    };

    PowerPolicy {
        idle_suspend,
        lid_close_action: action("lid_close_action", defaults.lid_close_action),
        power_key_action: action("power_key_action", defaults.power_key_action),
        low_battery_percent: percent("low_battery_percent", defaults.low_battery_percent),
        critical_battery_percent: percent(
            "critical_battery_percent",
            defaults.critical_battery_percent,
        ),
        critical_action: action("critical_action", defaults.critical_action),
    }
}

fn active_power_profile() -> Option<String> {
    let output = std::process::Command::new("powerprofilesctl")
        .arg("get")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let profile = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!profile.is_empty()).then_some(profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_settings_defaults() {
        let p = policy_from_settings_json("");
        assert_eq!(p, default_policy());
        assert_eq!(p.idle_suspend, Some(Duration::from_mins(15)));
    }

    #[test]
    fn no_power_section_is_defaults() {
        let p = policy_from_settings_json(r#"{"version":1,"display":{"scale":1.0}}"#);
        assert_eq!(p, default_policy());
    }

    #[test]
    fn full_section_parses() {
        // Exactly the shape the Settings app writes.
        let json = r#"{
            "version": 1,
            "power": {
                "auto_suspend": true,
                "blank_minutes": 5,
                "suspend_minutes": 20,
                "lid_close_action": "lock",
                "power_key_action": "ignore",
                "low_battery_percent": 20,
                "critical_battery_percent": 8,
                "critical_action": "suspend"
            }
        }"#;
        let p = policy_from_settings_json(json);
        assert_eq!(p.idle_suspend, Some(Duration::from_mins(20)));
        assert_eq!(p.lid_close_action, PowerAction::Lock);
        assert_eq!(p.power_key_action, PowerAction::Ignore);
        assert_eq!(p.low_battery_percent, 20);
        assert_eq!(p.critical_battery_percent, 8);
        assert_eq!(p.critical_action, PowerAction::Suspend);
    }

    #[test]
    fn auto_suspend_off_disables_idle_suspend() {
        let json = r#"{"power":{"auto_suspend":false,"suspend_minutes":15}}"#;
        assert_eq!(policy_from_settings_json(json).idle_suspend, None);
    }

    #[test]
    fn zero_suspend_minutes_disables_idle_suspend() {
        let json = r#"{"power":{"auto_suspend":true,"suspend_minutes":0}}"#;
        assert_eq!(policy_from_settings_json(json).idle_suspend, None);
    }

    #[test]
    fn malformed_action_falls_back_to_default() {
        let json = r#"{"power":{"lid_close_action":"explode"}}"#;
        let p = policy_from_settings_json(json);
        assert_eq!(p.lid_close_action, default_policy().lid_close_action);
    }

    #[test]
    fn percent_is_clamped() {
        let json = r#"{"power":{"low_battery_percent":250}}"#;
        assert_eq!(policy_from_settings_json(json).low_battery_percent, 100);
    }

    #[test]
    fn partial_section_keeps_other_defaults() {
        // Only the lid action is set; everything else is the default.
        let json = r#"{"power":{"lid_close_action":"suspend"}}"#;
        let p = policy_from_settings_json(json);
        let d = default_policy();
        assert_eq!(p.lid_close_action, PowerAction::Suspend);
        assert_eq!(p.power_key_action, d.power_key_action);
        assert_eq!(p.low_battery_percent, d.low_battery_percent);
        assert_eq!(p.idle_suspend, d.idle_suspend);
    }

    #[test]
    fn active_profile_selects_its_idle_timeout() {
        let section: serde_json::Value = serde_json::from_str(
            r#"{
                "auto_suspend": true,
                "suspend_minutes": 15,
                "saver_suspend_minutes": 7,
                "balanced_suspend_minutes": 17,
                "performance_suspend_minutes": 37
            }"#,
        )
        .unwrap();
        assert_eq!(
            policy_from_section_for_profile(&section, Some("power-saver")).idle_suspend,
            Some(Duration::from_secs(7 * 60))
        );
        assert_eq!(
            policy_from_section_for_profile(&section, Some("balanced")).idle_suspend,
            Some(Duration::from_secs(17 * 60))
        );
        assert_eq!(
            policy_from_section_for_profile(&section, Some("performance")).idle_suspend,
            Some(Duration::from_secs(37 * 60))
        );
    }

    #[test]
    fn unknown_profile_uses_legacy_timeout() {
        let section: serde_json::Value =
            serde_json::from_str(r#"{"suspend_minutes":23,"balanced_suspend_minutes":99}"#)
                .unwrap();
        assert_eq!(
            policy_from_section_for_profile(&section, Some("vendor-profile")).idle_suspend,
            Some(Duration::from_secs(23 * 60))
        );
    }
}
