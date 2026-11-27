//! The `appearance` section's plain leaves and bounds, which the toolkit draws by and the
//! settings store's typed keys write within. The schedule is [`crate::day_schedule`]'s.

pub use crate::day_schedule::{MODE_KEY, SECTION};

/// The field of the interface language, named as its language pack is (`de`; see
/// `tilo_paths::language_of`). Unset follows the device's locale.
pub const LANGUAGE: &str = "language";

/// The interface language: the one the person chose ([`LANGUAGE`]), else the
/// locale the device runs in (`LC_ALL`, `LC_MESSAGES`, `LANGUAGE`, `LANG`, read
/// as POSIX).
///
/// Every surface resolves its language — and the language of the dates it
/// formats — from this one answer, so a person who chose English on a German
/// machine reads English throughout: the system locale is only the default,
/// never a second source.
#[must_use]
pub fn preferred_language() -> Option<String> {
    let chosen = crate::source::source().and_then(|source| {
        crate::source::load_section_in(&*source, SECTION)
            .get(LANGUAGE)?
            .as_str()
            .map(str::to_owned)
    });
    let system = tilo_process::environment::environment().locale.clone();
    preferred_language_from(chosen.as_deref(), system.as_deref())
}

/// [`preferred_language`] over an explicit choice and system locale. A value
/// that names no language (`C`, `POSIX`, malformed) falls back to the next
/// source; `None` means neither named one.
#[must_use]
pub fn preferred_language_from(chosen: Option<&str>, system: Option<&str>) -> Option<String> {
    [chosen, system].into_iter().flatten().find_map(|value| {
        tilo_process::environment::parse_posix_locale(value).map(|locale| locale.to_string())
    })
}

/// The section of this device's own appearance, beside the person's.
pub const DEVICE_SECTION: &str = "appearance_device";

/// The bounds of the person's text scale.
pub const TEXT_SCALE_MIN: f32 = 0.5;
pub const TEXT_SCALE_MAX: f32 = 4.0;

/// The bounds of how much of a theme's translucency the person keeps.
pub const TRANSLUCENCY_MIN: f32 = 0.0;
pub const TRANSLUCENCY_MAX: f32 = 1.0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_chosen_language_wins_and_the_system_locale_is_only_the_default() {
        assert_eq!(
            preferred_language_from(Some("en-US"), Some("de_DE.UTF-8")).as_deref(),
            Some("en-US")
        );
        assert_eq!(
            preferred_language_from(Some("de_DE.UTF-8"), None).as_deref(),
            Some("de-DE")
        );
        assert_eq!(
            preferred_language_from(None, Some("de_DE.UTF-8")).as_deref(),
            Some("de-DE"),
            "with no choice the device's locale is the default"
        );
        assert_eq!(
            preferred_language_from(Some("C"), Some("fr_FR.UTF-8")).as_deref(),
            Some("fr-FR"),
            "an explicit C names no language, so the default decides"
        );
        assert_eq!(preferred_language_from(Some("C"), Some("C.UTF-8")), None);
        assert_eq!(preferred_language_from(Some("not a locale!"), None), None);
        assert_eq!(preferred_language_from(None, None), None);
    }
}
