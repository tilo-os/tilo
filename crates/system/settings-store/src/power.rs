//! Typed keys of the `power` section and the values the Settings pane offers
//! for them.
//!
//! The Settings pane, the shell's system-settings actions and the
//! compositor's power policy (`tilo_power_mgmt::config` reads the same
//! leaves) all name a setting through these, so a value one of them accepts
//! is a value every other one understands. `0` minutes is the persisted
//! spelling of "Never".

use crate::{Key, SectionWrite};

/// The section holding every power preference.
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

/// Master switch for idle-suspend; off disables it whatever the timeouts say.
pub const AUTO_SUSPEND: Key<bool> = Key::new(SECTION, "auto_suspend", || DEFAULT_AUTO_SUSPEND);
/// Idle minutes before the screen blanks; `0` never.
pub const BLANK_MINUTES: Key<u32> = Key::new(SECTION, "blank_minutes", || DEFAULT_BLANK_MINUTES);
/// Idle minutes before the session locks; `0` never. A manual lock and the
/// lock that precedes a suspend are not affected.
pub const LOCK_MINUTES: Key<u32> = Key::new(SECTION, "lock_minutes", || DEFAULT_LOCK_MINUTES);
/// Idle minutes before the session suspends, for a profile with no value of
/// its own; `0` never.
pub const SUSPEND_MINUTES: Key<u32> =
    Key::new(SECTION, "suspend_minutes", || DEFAULT_SUSPEND_MINUTES);
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

/// The power-profiles-daemon profiles that carry their own suspend timeout.
pub const PROFILES: &[&str] = &["saver", "balanced", "performance"];

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

/// The write that sets idle-suspend to `minutes` for `profile`, or for every
/// profile (and the legacy value older readers fall back to) when it is
/// `None`. A profile's own value outranks the shared one, so a write that
/// left the profile values alone would not change what the compositor does.
///
/// # Errors
///
/// `profile` is not one of [`PROFILES`], or `minutes` is not offered.
pub fn suspend_write(profile: Option<&str>, minutes: u32) -> Result<SectionWrite, String> {
    let minutes = check_minutes("suspend timeout", SUSPEND_CHOICES, minutes)?;
    let write = SectionWrite::new();
    Ok(match profile {
        None => write
            .set(&SUSPEND_MINUTES, &minutes)
            .set(&SAVER_SUSPEND_MINUTES, &minutes)
            .set(&BALANCED_SUSPEND_MINUTES, &minutes)
            .set(&PERFORMANCE_SUSPEND_MINUTES, &minutes),
        Some("saver") => write.set(&SAVER_SUSPEND_MINUTES, &minutes),
        Some("balanced") => write.set(&BALANCED_SUSPEND_MINUTES, &minutes),
        Some("performance") => write.set(&PERFORMANCE_SUSPEND_MINUTES, &minutes),
        Some(other) => {
            return Err(format!(
                "{other:?} is not a power profile: {}",
                PROFILES.join(", ")
            ));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_offered_minutes_are_accepted_and_never_is_a_stored_value() {
        assert_eq!(check_minutes("lock timeout", LOCK_CHOICES, 0), Ok(0));
        assert!(check_minutes("lock timeout", LOCK_CHOICES, 7).is_err());
        assert!(suspend_write(Some("turbo"), 30).is_err());
        assert!(suspend_write(Some("saver"), 7).is_err());
        assert!(suspend_write(None, 30).is_ok());
        let never = json!({"lock_minutes": 0});
        assert_eq!(LOCK_MINUTES.from_section(never.as_object().unwrap()), 0);
        assert_eq!(
            LOCK_MINUTES.from_section(&serde_json::Map::new()),
            DEFAULT_LOCK_MINUTES
        );
    }
}
