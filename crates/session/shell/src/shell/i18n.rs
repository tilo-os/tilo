//! The shell's Fluent catalogue.
//!
//! Chrome strings are keys resolved through the toolkit's [`mod@ui::i18n`]
//! manager, never literals. The catalogue is embedded with `include_str!`, so
//! a `--shots` render, a unit test and a live session resolve the same copy.

use ui::i18n::{ArgValue, EmbeddedCatalog};

/// Loaded per thread on first use; a host's manager is joined, never replaced.
/// The probe is a key every catalogue revision defines.
static CATALOG: EmbeddedCatalog = EmbeddedCatalog::new(
    "en-US",
    &[("en-US", include_str!("../../locales/en-US.ftl"))],
    "continuity-dismiss",
);

/// Resolve a shell string key.
pub fn t(key: &str) -> String {
    CATALOG.t(key)
}

/// Resolve a shell string key that interpolates the given nouns.
pub fn t_args(key: &str, args: &[(&str, ArgValue)]) -> String {
    CATALOG.t_args(key, args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_the_shell_uses_resolves_to_a_real_string() {
        for key in [
            "continuity-continue",
            "continuity-continue-subtitle",
            "continuity-dismiss",
            "continuity-dismiss-title",
            "continuity-dismiss-subtitle",
            "continuity-other-device",
            "tab-closed-toast",
            "tab-closed-group-toast",
            "tab-closed-undo",
            "tab-closed-undo-hint",
            "tab-closed-undo-hint-unbound",
            "window-switcher-title",
            "window-switcher-description",
            "window-switcher-app-title",
            "window-switcher-app-description",
            "window-switcher-untitled",
            "window-switcher-caption-paged",
        ] {
            let s = t(key);
            assert_ne!(s, key, "{key} has no entry in the catalogue");
            assert!(!s.trim().is_empty(), "{key} resolves to nothing");
        }
        assert_eq!(
            t_args(
                "continuity-continue",
                &[("app", "Notes".into()), ("device", "Laptop".into())]
            ),
            "Continue Notes from Laptop"
        );
    }

    #[test]
    fn every_catalogue_entry_resolves_to_a_real_string() {
        let source = include_str!("../../locales/en-US.ftl");
        let keys: Vec<&str> = source
            .lines()
            .filter(|line| !line.starts_with('#') && !line.starts_with(' '))
            .filter_map(|line| line.split_once(" = ").map(|(key, _)| key.trim()))
            .collect();
        assert!(!keys.is_empty());
        for key in keys {
            let value = t(key);
            assert_ne!(value, key, "{key} has no entry in the catalogue");
            assert!(!value.trim().is_empty(), "{key} resolves to nothing");
        }
    }
}
