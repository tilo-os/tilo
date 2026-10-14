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
//!   "auto_suspend": false,
//!   "blank_minutes": 10,
//!   "lock_minutes": 5,
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
pub const DEFAULT_SUSPEND_MINUTES: u32 = settings_store::power::DEFAULT_SUSPEND_MINUTES;

/// The Settings default screen-blank timeout, in minutes. `0` is the
/// persisted spelling of "Never".
pub const DEFAULT_BLANK_MINUTES: u32 = settings_store::power::DEFAULT_BLANK_MINUTES;

/// The Settings default idle-lock timeout, in minutes. `0` is the persisted
/// spelling of "Never".
pub const DEFAULT_LOCK_MINUTES: u32 = settings_store::power::DEFAULT_LOCK_MINUTES;

/// The largest screen-blank timeout the Settings UI can express. Rejecting
/// larger persisted values keeps corrupt/manual configuration from making the
/// display effectively never blank while still accepting every UI choice.
pub const MAX_BLANK_MINUTES: u32 = 30;

/// The largest idle-lock timeout exposed by Settings.
pub const MAX_LOCK_MINUTES: u32 = 30;

/// Resolve the shared settings path (`$XDG_CONFIG_HOME/tilo/settings.json`,
/// falling back to `~/.config/tilo/settings.json`) — the same file the
/// Settings app and the rest of the compositor read.
pub fn settings_path() -> Option<PathBuf> {
    settings_store::settings_path()
}

/// Load the policy from the session's settings for the power-profiles-daemon
/// profile the power service last observed ([`crate::PowerBridge::active_profile`]),
/// falling back to the Settings defaults when there are none. Never asks the
/// daemon itself: the compositor reloads this on its frame thread.
///
/// The document is read the way every other reader reads it
/// ([`settings_store::load_section`]): from the source the session installed
/// when there is one, else from the file. A reader woken by a write to the
/// store must see that write, and the session's source publishes a change
/// before it has rewritten the `settings.json` copy, so reading the copy here
/// let a write made through the store be applied from the value it replaced.
///
/// The result is already narrowed to this machine's duty: a host never
/// suspends on its own.
pub fn load_policy_for_profile(active_profile: Option<&str>) -> PowerPolicy {
    let section =
        serde_json::Value::Object(settings_store::load_section(settings_store::power::SECTION));
    policy_from_section_for_profile(&section, active_profile)
        .for_duty(crate::policy::this_machine_duty())
}

/// The policy a freshly-installed machine runs (Settings defaults): idle
/// blanks and locks the screen, but idle-suspend follows
/// [`settings_store::power::DEFAULT_AUTO_SUSPEND`] (off), so nothing but the
/// lid, the power key or a critical battery puts the machine to sleep until
/// the person turns "suspend when idle" on.
pub fn default_policy() -> PowerPolicy {
    PowerPolicy {
        idle_suspend: settings_store::power::DEFAULT_AUTO_SUSPEND
            .then(|| Duration::from_secs(u64::from(DEFAULT_SUSPEND_MINUTES) * 60)),
        ..PowerPolicy::default()
    }
}

/// Parse a [`PowerPolicy`] from a full `settings.json` document with no
/// active power profile. Never fails; unknown/missing fields take defaults.
pub fn policy_from_settings_json(text: &str) -> PowerPolicy {
    policy_from_settings_json_for_profile(text, None)
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
        .unwrap_or(settings_store::power::DEFAULT_AUTO_SUSPEND);
    let legacy_suspend_minutes = section
        .get("suspend_minutes")
        .and_then(serde_json::Value::as_u64)
        .map_or(DEFAULT_SUSPEND_MINUTES, |m| {
            u32::try_from(m).unwrap_or(u32::MAX)
        });
    let profile_key = match active_profile {
        Some("power-saver") => Some("saver_suspend_minutes"),
        Some("balanced") => Some("balanced_suspend_minutes"),
        Some("performance") => Some("performance_suspend_minutes"),
        _ => None,
    };
    let suspend_minutes = profile_key
        .and_then(|key| section.get(key))
        .and_then(serde_json::Value::as_u64)
        .map_or(legacy_suspend_minutes, |m| {
            u32::try_from(m).unwrap_or(u32::MAX)
        });

    // `auto_suspend` off, or a 0-minute timeout, disables idle-suspend.
    let idle_suspend = if auto_suspend && suspend_minutes > 0 {
        Some(Duration::from_secs(u64::from(suspend_minutes) * 60))
    } else {
        None
    };

    // `0` is the Settings value for "Never". Values beyond the UI's bounded
    // range are treated as malformed and repaired to the safe Settings
    // default, rather than becoming an invisible, unbounded timeout.
    let blank_minutes = section
        .get("blank_minutes")
        .and_then(serde_json::Value::as_u64)
        .and_then(|minutes| u32::try_from(minutes).ok())
        .filter(|minutes| *minutes <= MAX_BLANK_MINUTES)
        .unwrap_or(DEFAULT_BLANK_MINUTES);
    let idle_blank =
        (blank_minutes > 0).then(|| Duration::from_secs(u64::from(blank_minutes) * 60));

    let lock_minutes = section
        .get("lock_minutes")
        .and_then(serde_json::Value::as_u64)
        .and_then(|minutes| u32::try_from(minutes).ok())
        .filter(|minutes| *minutes <= MAX_LOCK_MINUTES)
        .unwrap_or(DEFAULT_LOCK_MINUTES);
    let idle_lock = (lock_minutes > 0).then(|| Duration::from_secs(u64::from(lock_minutes) * 60));

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
        idle_blank,
        idle_lock,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_json_parses_power_sections_and_falls_back_to_defaults() {
        // Phase `missing_file_is_settings_defaults`.
        {
            let p = policy_from_settings_json("");
            assert_eq!(p, default_policy());
            assert_eq!(p.idle_blank, Some(Duration::from_secs(600)));
            assert_eq!(p.idle_lock, Some(Duration::from_secs(300)));
            assert_eq!(
                p.idle_suspend, None,
                "a fresh install never suspends because nobody touched it"
            );
            assert_eq!(
                p.lid_close_action,
                PowerAction::Suspend,
                "closing the lid still does"
            );
        }
        // Phase `a_timeout_without_the_switch_stays_off`.
        {
            // A suspend timeout is written by the pane even while the switch
            // is off; it alone must not turn idle-suspend back on.
            let p = policy_from_settings_json(r#"{"power":{"suspend_minutes":15}}"#);
            assert_eq!(p.idle_suspend, None);
            let p = policy_from_settings_json(
                r#"{"power":{"auto_suspend":true,"suspend_minutes":15}}"#,
            );
            assert_eq!(p.idle_suspend, Some(Duration::from_secs(900)));
        }
        // Phase `no_power_section_is_defaults`.
        {
            let p = policy_from_settings_json(r#"{"version":1,"display":{"scale":1.0}}"#);
            assert_eq!(p, default_policy());
        }
        // Phase `full_section_parses`.
        {
            // Exactly the shape the Settings app writes.
            let json = r#"{
                "version": 1,
                "power": {
                    "auto_suspend": true,
                    "blank_minutes": 5,
                    "lock_minutes": 10,
                    "suspend_minutes": 20,
                    "lid_close_action": "lock",
                    "power_key_action": "ignore",
                    "low_battery_percent": 20,
                    "critical_battery_percent": 8,
                    "critical_action": "suspend"
                }
            }"#;
            let p = policy_from_settings_json(json);
            assert_eq!(p.idle_blank, Some(Duration::from_secs(300)));
            assert_eq!(p.idle_lock, Some(Duration::from_secs(600)));
            assert_eq!(p.idle_suspend, Some(Duration::from_secs(1200)));
            assert_eq!(p.lid_close_action, PowerAction::Lock);
            assert_eq!(p.power_key_action, PowerAction::Ignore);
            assert_eq!(p.low_battery_percent, 20);
            assert_eq!(p.critical_battery_percent, 8);
            assert_eq!(p.critical_action, PowerAction::Suspend);
        }
        // Phase `malformed_action_falls_back_to_default`.
        {
            let json = r#"{"power":{"lid_close_action":"explode"}}"#;
            let p = policy_from_settings_json(json);
            assert_eq!(p.lid_close_action, default_policy().lid_close_action);
        }
        // Phase `percent_is_clamped`.
        {
            let json = r#"{"power":{"low_battery_percent":250}}"#;
            assert_eq!(policy_from_settings_json(json).low_battery_percent, 100);
        }
        // Phase `partial_section_keeps_other_defaults`.
        {
            // Only the lid action is set; everything else is the default.
            let json = r#"{"power":{"lid_close_action":"suspend"}}"#;
            let p = policy_from_settings_json(json);
            let d = default_policy();
            assert_eq!(p.lid_close_action, PowerAction::Suspend);
            assert_eq!(p.power_key_action, d.power_key_action);
            assert_eq!(p.low_battery_percent, d.low_battery_percent);
            assert_eq!(p.idle_blank, d.idle_blank);
            assert_eq!(p.idle_lock, d.idle_lock);
            assert_eq!(p.idle_suspend, d.idle_suspend);
        }
    }

    #[test]
    fn idle_timeouts_disable_at_zero_and_repair_out_of_range_values() {
        type Case = (
            &'static str,
            String,
            fn(&PowerPolicy) -> Option<Duration>,
            Option<Duration>,
        );
        let blank = |p: &PowerPolicy| p.idle_blank;
        let lock = |p: &PowerPolicy| p.idle_lock;
        let suspend = |p: &PowerPolicy| p.idle_suspend;
        let lock_default = Some(Duration::from_secs(u64::from(DEFAULT_LOCK_MINUTES) * 60));
        let blank_default = Some(Duration::from_secs(u64::from(DEFAULT_BLANK_MINUTES) * 60));
        let cases: [Case; 8] = [
            (
                "auto_suspend_off_disables_idle_suspend",
                r#"{"power":{"auto_suspend":false,"suspend_minutes":15}}"#.into(),
                suspend,
                None,
            ),
            (
                "zero_suspend_minutes_disables_idle_suspend",
                r#"{"power":{"auto_suspend":true,"suspend_minutes":0}}"#.into(),
                suspend,
                None,
            ),
            (
                "zero_blank_minutes_means_never_blank",
                r#"{"power":{"blank_minutes":0}}"#.into(),
                blank,
                None,
            ),
            (
                "zero_lock_minutes_means_never_auto_lock",
                r#"{"power":{"lock_minutes":0}}"#.into(),
                lock,
                None,
            ),
            (
                "lock_minutes_is_bounded_and_malformed_values_repair_to_default",
                r#"{"power":{"lock_minutes":31}}"#.into(),
                lock,
                lock_default,
            ),
            (
                "lock_minutes_is_bounded_and_malformed_values_repair_to_default",
                r#"{"power":{"lock_minutes":999999999999999999999}}"#.into(),
                lock,
                lock_default,
            ),
            (
                "blank_minutes_is_bounded_and_malformed_values_repair_to_default",
                r#"{"power":{"blank_minutes":31}}"#.into(),
                blank,
                blank_default,
            ),
            (
                "blank_minutes_is_bounded_and_malformed_values_repair_to_default",
                r#"{"power":{"blank_minutes":999999999999999999999}}"#.into(),
                blank,
                blank_default,
            ),
        ];
        for (case, json, field, want) in cases {
            assert_eq!(
                field(&policy_from_settings_json(&json)),
                want,
                "case {case}: {json}"
            );
        }
    }

    #[test]
    fn power_profiles_select_their_idle_timeout_or_the_legacy_one() {
        // Phase `active_profile_selects_its_idle_timeout`.
        {
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
                Some(Duration::from_secs(420))
            );
            assert_eq!(
                policy_from_section_for_profile(&section, Some("balanced")).idle_suspend,
                Some(Duration::from_secs(1020))
            );
            assert_eq!(
                policy_from_section_for_profile(&section, Some("performance")).idle_suspend,
                Some(Duration::from_secs(2220))
            );
        }
        // Phase `unknown_profile_uses_legacy_timeout`.
        {
            let section: serde_json::Value =
                serde_json::from_str(r#"{"suspend_minutes":23,"balanced_suspend_minutes":99}"#)
                    .unwrap();
            assert_eq!(
                policy_from_section_for_profile(&section, Some("vendor-profile")).idle_suspend,
                Some(Duration::from_secs(1380))
            );
        }
    }
}
