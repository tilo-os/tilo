//! Typed keys of the `power` section and the values the Settings pane offers
//! for them.
//!
//! The Settings pane, the shell's system-settings actions and the
//! compositor's power policy (`tilo_power_mgmt::config` reads the same
//! leaves) all name a setting through these, so a value one of them accepts
//! is a value every other one understands. `0` minutes is the persisted
//! spelling of "Never".

use crate::Key;

pub const SECTION: &str = "power";

/// The Settings default idle-suspend timeout, in minutes.
pub const DEFAULT_SUSPEND_MINUTES: u32 = 15;
/// The Settings default screen-blank timeout, in minutes.
pub const DEFAULT_BLANK_MINUTES: u32 = 10;
/// The Settings default idle-lock timeout, in minutes.
pub const DEFAULT_LOCK_MINUTES: u32 = 5;

/// Whether a machine with no saved choice suspends on its own when idle.
///
/// Off: a laptop that goes to sleep while its owner reads, listens or waits
/// for a build looks like it suspended at random, and idle is a guess about
/// attention the machine cannot verify. Closing the lid, the power key and a
/// critically low battery still suspend; idle-suspend is an opt-in.
pub const DEFAULT_AUTO_SUSPEND: bool = false;

/// Whether the session asks for the person's password when it wakes from
/// sleep.
///
/// On: sleep leaves the session locked, and waking shows the lock screen.
/// Off: a machine that went to sleep unlocked comes back to the work its
/// owner left on screen. An idle, explicit or in-flight lock is kept either
/// way — only the lock this device adds on the way into sleep follows it.
pub const LOCK_ON_SLEEP: Key<bool> = Key::new(SECTION, "lock_on_sleep", || DEFAULT_LOCK_ON_SLEEP);

/// Whether a fresh machine asks for the password after sleep.
///
/// On. A closed lid is the most common way a machine leaves its owner's
/// hands, and waking it should not be the one path back in.
pub const DEFAULT_LOCK_ON_SLEEP: bool = true;

/// Master switch for idle-suspend; off disables it whatever the timeouts say.
pub const AUTO_SUSPEND: Key<bool> = Key::new(SECTION, "auto_suspend", || DEFAULT_AUTO_SUSPEND);
/// Idle minutes before the screen blanks; `0` never.
pub const BLANK_MINUTES: Key<u32> = Key::new(SECTION, "blank_minutes", || DEFAULT_BLANK_MINUTES);
/// Idle minutes before the session locks; `0` never. A manual lock and the
/// lock that precedes a suspend are not affected.
pub const LOCK_MINUTES: Key<u32> = Key::new(SECTION, "lock_minutes", || DEFAULT_LOCK_MINUTES);
/// Idle minutes before the session suspends while plugged in, and on
/// battery too until that has a value of its own; `0` never.
pub const SUSPEND_MINUTES: Key<u32> =
    Key::new(SECTION, "suspend_minutes", || DEFAULT_SUSPEND_MINUTES);
/// Idle minutes before the session suspends on battery; unset, the
/// plugged-in timer applies on battery too.
pub const BATTERY_SUSPEND_MINUTES: Key<Option<u32>> =
    Key::new(SECTION, "battery_suspend_minutes", || None);
/// Idle-suspend minutes on the power-saver profile.
pub const SAVER_SUSPEND_MINUTES: Key<u32> = Key::new(SECTION, "saver_suspend_minutes", || 10);
/// Idle-suspend minutes on the balanced profile.
pub const BALANCED_SUSPEND_MINUTES: Key<u32> =
    Key::new(SECTION, "balanced_suspend_minutes", || {
        DEFAULT_SUSPEND_MINUTES
    });
/// Idle-suspend minutes on the performance profile.
pub const PERFORMANCE_SUSPEND_MINUTES: Key<u32> =
    Key::new(SECTION, "performance_suspend_minutes", || 30);

/// The screen-blank timeouts the Settings pane offers, in minutes.
pub const BLANK_CHOICES: &[u32] = &[0, 1, 5, 10, 15, 30];
/// The idle-lock timeouts the Settings pane offers, in minutes.
pub const LOCK_CHOICES: &[u32] = &[0, 1, 5, 10, 15, 30];
/// The idle-suspend timeouts the Settings pane offers, in minutes.
pub const SUSPEND_CHOICES: &[u32] = &[0, 5, 10, 15, 30, 45, 60];

/// The power-profiles-daemon profiles that can carry their own suspend
/// timeout.
pub const PROFILES: &[&str] = &["saver", "balanced", "performance"];

/// The idle-suspend timers as stored, and which one applies.
///
/// There is one timer per power source: plugged in, and on battery. A
/// profile's own timer is older: it applies only where the stored profile
/// timers truly differ from one another, and then it is the only one (the
/// selected profile's timer, shown with the profile). Timers that are all
/// the same are one timer, whichever leaves hold it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SuspendTimers {
    pub plugged_in: u32,
    pub battery: Option<u32>,
    /// The saver, balanced and performance timers, when set ([`PROFILES`]).
    pub profiles: [Option<u32>; 3],
}

impl SuspendTimers {
    #[must_use]
    pub fn from_section(section: &serde_json::Map<String, serde_json::Value>) -> Self {
        let stored = |name: &str| {
            section
                .get(name)
                .and_then(serde_json::Value::as_u64)
                .map(|minutes| u32::try_from(minutes).unwrap_or(u32::MAX))
        };
        Self {
            plugged_in: SUSPEND_MINUTES.from_section(section),
            battery: stored(BATTERY_SUSPEND_MINUTES.name()),
            profiles: [
                stored(SAVER_SUSPEND_MINUTES.name()),
                stored(BALANCED_SUSPEND_MINUTES.name()),
                stored(PERFORMANCE_SUSPEND_MINUTES.name()),
            ],
        }
    }

    /// Each profile's timer: its own, or the shared one.
    fn per_profile(&self) -> [u32; 3] {
        self.profiles
            .map(|minutes| minutes.unwrap_or(self.plugged_in))
    }

    /// Whether the profiles' timers truly differ from one another.
    #[must_use]
    pub fn profiles_differ(&self) -> bool {
        let [saver, balanced, performance] = self.per_profile();
        saver != balanced || balanced != performance
    }

    /// The timer while plugged in, when the profiles do not differ: the
    /// one value they all hold.
    #[must_use]
    pub fn plugged_in(&self) -> u32 {
        if self.profiles_differ() {
            self.plugged_in
        } else {
            self.per_profile()[0]
        }
    }

    /// The timer on battery, when the profiles do not differ.
    #[must_use]
    pub fn on_battery(&self) -> u32 {
        self.battery.unwrap_or_else(|| self.plugged_in())
    }

    /// The index in [`PROFILES`] of `profile`, by Settings' name or
    /// power-profiles-daemon's.
    #[must_use]
    pub fn profile_index(profile: &str) -> Option<usize> {
        match profile {
            "saver" | "power-saver" => Some(0),
            "balanced" => Some(1),
            "performance" => Some(2),
            _ => None,
        }
    }

    /// The timer of `profile` (see [`Self::profile_index`]).
    #[must_use]
    pub fn of_profile(&self, profile: &str) -> Option<u32> {
        Self::profile_index(profile).and_then(|index| self.per_profile().get(index).copied())
    }

    /// The minutes that apply with `profile` active (power-profiles-daemon's
    /// name, when known) on battery or plugged in; `0` never.
    #[must_use]
    pub fn applying(&self, profile: Option<&str>, on_battery: bool) -> u32 {
        if self.profiles_differ()
            && let Some(minutes) = profile.and_then(|profile| self.of_profile(profile))
        {
            return minutes;
        }
        if on_battery {
            self.on_battery()
        } else {
            self.plugged_in()
        }
    }
}

/// `minutes` when it is one of `choices`.
///
/// # Errors
///
/// A sentence naming the values that are offered.
pub fn check_minutes(what: &str, choices: &[u32], minutes: u32) -> Result<u32, String> {
    if choices.contains(&minutes) {
        return Ok(minutes);
    }
    let offered: Vec<String> = choices.iter().map(u32::to_string).collect();
    Err(format!(
        "{minutes} is not an offered {what} (minutes; 0 is never): {}",
        offered.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_offered_minutes_are_accepted_and_never_is_a_stored_value() {
        assert_eq!(check_minutes("lock timeout", LOCK_CHOICES, 0), Ok(0));
        assert!(check_minutes("lock timeout", LOCK_CHOICES, 7).is_err());
        let never = json!({"lock_minutes": 0});
        assert_eq!(LOCK_MINUTES.from_section(never.as_object().unwrap()), 0);
        assert_eq!(
            LOCK_MINUTES.from_section(&serde_json::Map::new()),
            DEFAULT_LOCK_MINUTES
        );
    }

    fn timers(section: &serde_json::Value) -> SuspendTimers {
        SuspendTimers::from_section(section.as_object().unwrap())
    }

    #[test]
    fn one_suspend_timer_per_power_source_unless_the_profiles_truly_differ() {
        // Nothing chosen: one timer, on battery and plugged in alike.
        let fresh = timers(&json!({}));
        assert!(!fresh.profiles_differ());
        assert_eq!(
            fresh.applying(Some("balanced"), true),
            DEFAULT_SUSPEND_MINUTES
        );
        assert_eq!(
            fresh.applying(Some("performance"), false),
            DEFAULT_SUSPEND_MINUTES
        );

        // A timer per source; the profile changes neither.
        let split = timers(&json!({"suspend_minutes": 30, "battery_suspend_minutes": 10}));
        assert!(!split.profiles_differ());
        for profile in [
            None,
            Some("power-saver"),
            Some("balanced"),
            Some("performance"),
        ] {
            assert_eq!(split.applying(profile, true), 10);
            assert_eq!(split.applying(profile, false), 30);
        }

        let same = timers(&json!({
            "suspend_minutes": 15,
            "saver_suspend_minutes": 45,
            "balanced_suspend_minutes": 45,
            "performance_suspend_minutes": 45,
        }));
        assert!(!same.profiles_differ());
        assert_eq!(same.plugged_in(), 45);
        assert_eq!(same.applying(Some("balanced"), true), 45);

        // Truly different profile timers: the selected profile's applies,
        // whatever the source.
        let differ = timers(&json!({
            "suspend_minutes": 15,
            "battery_suspend_minutes": 5,
            "saver_suspend_minutes": 10,
        }));
        assert!(differ.profiles_differ());
        assert_eq!(differ.applying(Some("power-saver"), false), 10);
        assert_eq!(differ.applying(Some("balanced"), true), 15);
        assert_eq!(
            differ.applying(None, true),
            5,
            "no profile known: the source's"
        );
    }
}
