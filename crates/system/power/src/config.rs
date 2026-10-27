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

/// Resolve the shared settings path (`$XDG_CONFIG_HOME/tilo/session/settings.json`,
/// falling back to `~/.config/tilo/session/settings.json`) — the same file the
/// Settings app and the rest of the compositor read.
pub fn settings_path() -> Option<PathBuf> {
    settings_store::settings_path()
}

/// Load the policy from the session's settings for the power-profiles-daemon
/// profile the power service last observed ([`crate::PowerBridge::active_profile`])
/// and the power source it last saw ([`crate::PowerBridge::on_battery`]),
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
pub fn load_policy_for_profile(active_profile: Option<&str>, on_battery: bool) -> PowerPolicy {
    let section =
        serde_json::Value::Object(settings_store::load_section(settings_store::power::SECTION));
    policy_from_section_for_profile(&section, active_profile, on_battery)
        .for_duty(crate::policy::this_machine_duty())
}

/// The policy a freshly-installed machine runs (Settings defaults): idle
/// blanks and locks the screen, but idle-suspend follows
/// [`settings_store::power::DEFAULT_AUTO_SUSPEND`] (off), so nothing but the
/// lid, the power key or a critical battery puts the machine to sleep until
/// the person turns "suspend when idle" on.
pub fn default_policy() -> PowerPolicy {
    default_policy_from_installation(std::path::Path::new("/etc/tilo/install-purpose"))
}

fn default_policy_from_installation(path: &std::path::Path) -> PowerPolicy {
    default_policy_for_purpose(std::fs::read_to_string(path).ok().as_deref().map(str::trim))
}

/// Installation purpose changes only automatic idle sleep. Explicit power
/// settings can override it; lid and critical-battery protection stay intact.
pub fn default_policy_for_purpose(purpose: Option<&str>) -> PowerPolicy {
    PowerPolicy {
        idle_suspend: (purpose != Some("server") && settings_store::power::DEFAULT_AUTO_SUSPEND)
            .then(|| Duration::from_secs(u64::from(DEFAULT_SUSPEND_MINUTES) * 60)),
        ..PowerPolicy::default()
    }
}

/// Parse a [`PowerPolicy`] from a full `settings.json` document with no
/// active power profile. Never fails; unknown/missing fields take defaults.
pub fn policy_from_settings_json(text: &str) -> PowerPolicy {
    policy_from_settings_json_for_profile(text, None, false)
}

pub fn policy_from_settings_json_for_profile(
    text: &str,
    active_profile: Option<&str>,
    on_battery: bool,
) -> PowerPolicy {
    let root: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return default_policy(),
    };
    let Some(section) = root.get("power") else {
        return default_policy();
    };
    policy_from_section_for_profile(section, active_profile, on_battery)
}

/// Resolve a policy for a power-profiles-daemon profile and a power source.
///
/// This separate entry point keeps parsing deterministic in tests and makes
/// the Settings/compositor contract explicit. The idle-suspend timer is the
/// power source's ([`settings_store::power::SuspendTimers::applying`]); a
/// profile's own applies only where the stored profile timers truly differ.
pub fn policy_from_section_for_profile(
    section: &serde_json::Value,
    active_profile: Option<&str>,
    on_battery: bool,
) -> PowerPolicy {
    let defaults = default_policy();

    let auto_suspend = section
        .get("auto_suspend")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or_else(|| defaults.idle_suspend.is_some());
    let suspend_minutes = section
        .as_object()
        .map_or(DEFAULT_SUSPEND_MINUTES, |section| {
            settings_store::power::SuspendTimers::from_section(section)
                .applying(active_profile, on_battery)
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
        // Only an explicit `false` lets sleep leave the session unlocked: a
        // missing or malformed value keeps the password.
        lock_on_sleep: section
            .get(settings_store::power::LOCK_ON_SLEEP.name())
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(defaults.lock_on_sleep),
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
    fn installed_purpose_changes_only_idle_sleep_defaults() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), "server\n").unwrap();
        let server = default_policy_from_installation(file.path());
        let computer = default_policy_for_purpose(Some("computer"));
        assert_eq!(server.idle_suspend, None);
        assert_eq!(
            computer.idle_suspend,
            settings_store::power::DEFAULT_AUTO_SUSPEND.then(|| Duration::from_secs(900))
        );
        assert_eq!(
            PowerPolicy {
                idle_suspend: None,
                ..computer
            },
            server
        );
        for legacy in [None, Some(""), Some("untrusted-purpose")] {
            assert_eq!(default_policy_for_purpose(legacy), computer);
        }
        std::fs::write(file.path(), "computer\n").unwrap();
        assert_eq!(default_policy_from_installation(file.path()), computer);
    }

    #[test]
    fn settings_json_parses_power_sections_and_falls_back_to_defaults() {
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
        {
            let p = policy_from_settings_json(r#"{"version":1,"display":{"scale":1.0}}"#);
            assert_eq!(p, default_policy());
        }
        {
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
        {
            let json = r#"{"power":{"lid_close_action":"explode"}}"#;
            let p = policy_from_settings_json(json);
            assert_eq!(p.lid_close_action, default_policy().lid_close_action);
        }
        {
            let json = r#"{"power":{"low_battery_percent":250}}"#;
            assert_eq!(policy_from_settings_json(json).low_battery_percent, 100);
        }
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
    fn the_password_after_sleep_setting_is_kept_unless_it_is_explained_away() {
        type Case = (&'static str, &'static str, bool);
        let cases: [Case; 4] = [
            ("missing keeps the password", "{}", true),
            ("no power section keeps it", r#"{"display":{}}"#, true),
            (
                "an explicit false lets sleep leave the session open",
                r#"{"power":{"lock_on_sleep":false}}"#,
                false,
            ),
            (
                "a malformed value keeps the password",
                r#"{"power":{"lock_on_sleep":"sometimes"}}"#,
                true,
            ),
        ];
        for (name, json, want) in cases {
            assert_eq!(
                policy_from_settings_json(json).lock_on_sleep,
                want,
                "{name}"
            );
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
                policy_from_section_for_profile(&section, Some("power-saver"), false).idle_suspend,
                Some(Duration::from_secs(420))
            );
            assert_eq!(
                policy_from_section_for_profile(&section, Some("balanced"), false).idle_suspend,
                Some(Duration::from_secs(1020))
            );
            assert_eq!(
                policy_from_section_for_profile(&section, Some("performance"), false).idle_suspend,
                Some(Duration::from_secs(2220))
            );
        }
        {
            let section: serde_json::Value = serde_json::from_str(
                r#"{"auto_suspend":true,"suspend_minutes":23,"balanced_suspend_minutes":99}"#,
            )
            .unwrap();
            assert_eq!(
                policy_from_section_for_profile(&section, Some("vendor-profile"), false)
                    .idle_suspend,
                Some(Duration::from_secs(1380))
            );
        }
        {
            let section: serde_json::Value = serde_json::from_str(
                r#"{"auto_suspend":true,"suspend_minutes":30,"battery_suspend_minutes":10}"#,
            )
            .unwrap();
            for profile in [None, Some("power-saver"), Some("performance")] {
                assert_eq!(
                    policy_from_section_for_profile(&section, profile, true).idle_suspend,
                    Some(Duration::from_secs(600)),
                    "on battery with {profile:?}"
                );
                assert_eq!(
                    policy_from_section_for_profile(&section, profile, false).idle_suspend,
                    Some(Duration::from_secs(1800)),
                    "plugged in with {profile:?}"
                );
            }
        }
    }

    /// What can put this machine to sleep when "suspend when idle" is off
    /// and the screen is set never to lock: idleness never, whatever the
    /// profile, power source or stored timers; only the lid, the power key
    /// and a critical battery, each as configured.
    #[test]
    fn with_idle_suspend_off_only_the_lid_the_power_key_and_a_critical_battery_suspend() {
        let section: serde_json::Value = serde_json::from_str(
            r#"{
                "auto_suspend": false,
                "lock_minutes": 0,
                "suspend_minutes": 15,
                "battery_suspend_minutes": 5,
                "saver_suspend_minutes": 3,
                "balanced_suspend_minutes": 10,
                "performance_suspend_minutes": 30
            }"#,
        )
        .unwrap();
        for profile in [
            None,
            Some("power-saver"),
            Some("balanced"),
            Some("performance"),
        ] {
            for on_battery in [false, true] {
                let policy = policy_from_section_for_profile(&section, profile, on_battery);
                assert_eq!(
                    policy.idle_suspend, None,
                    "{profile:?} on battery {on_battery}"
                );
                assert_eq!(
                    policy.idle_lock, None,
                    "{profile:?} on battery {on_battery}"
                );
                assert_eq!(policy.on_lid(true), Some(PowerAction::Suspend));
                assert_eq!(policy.on_power_key(), Some(PowerAction::Suspend));
                assert_eq!(policy.critical_action, PowerAction::Suspend);
            }
        }
        let ignored: serde_json::Value = serde_json::from_str(
            r#"{"auto_suspend":false,"lid_close_action":"ignore","power_key_action":"lock","critical_action":"ignore"}"#,
        )
        .unwrap();
        let policy = policy_from_section_for_profile(&ignored, None, true);
        assert_eq!(policy.on_lid(true), None);
        assert_eq!(policy.on_power_key(), Some(PowerAction::Lock));
        assert_eq!(policy.critical_action, PowerAction::Ignore);
    }
}
