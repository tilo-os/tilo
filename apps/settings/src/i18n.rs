//! Settings' Fluent catalogue.
//!
//! User-facing strings resolve by key through the toolkit's [`mod@ui::i18n`]
//! manager. The catalogue is embedded with `include_str!`, so a `--shots`
//! render, a unit test and a live session resolve the same copy.

use ui::i18n::{ArgValue, EmbeddedCatalog};

/// Loaded per thread on first use; a host's manager is joined, never replaced.
static CATALOG: EmbeddedCatalog = EmbeddedCatalog::new(
    "en-US",
    &[("en-US", include_str!("../locales/en-US.ftl"))],
    "display-arrangement-title",
);

/// Resolve a Settings string key.
pub fn t(key: &str) -> String {
    CATALOG.t(key)
}

/// Resolve a Settings string key with named arguments.
pub fn t_args(key: &str, args: &[(&str, ArgValue)]) -> String {
    CATALOG.t_args(key, args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_catalogue_entry_resolves_to_a_real_string() {
        let source = include_str!("../locales/en-US.ftl");
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
