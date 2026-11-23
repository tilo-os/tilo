//! The sign-in screen's Fluent catalogue.
//!
//! Every string the greeter shows is a key here, resolved in the machine's
//! locale through the toolkit's [`mod@ui::i18n`] manager. The catalogues are
//! embedded, so a `--shots` render and a unit test resolve exactly what the
//! login screen does.

ui::embedded_catalog!(pub fn, ["en-US", "de-DE"]);

/// Load the catalogue on this thread if it is not loaded yet.
pub fn ensure() {
    CATALOG.ensure();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// The keys a catalogue ships and its text for each, as the loader reads
    /// them: one `key = text` line per entry.
    fn catalogue(source: &'static str) -> BTreeMap<&'static str, &'static str> {
        source
            .lines()
            .filter(|line| !line.starts_with('#'))
            .filter_map(|line| line.split_once(" = "))
            .collect()
    }

    #[test]
    fn every_key_resolves_in_every_language() {
        let english = catalogue(include_str!("../locales/en-US.ftl"));
        let german = catalogue(include_str!("../locales/de-DE.ftl"));
        assert_eq!(
            english.keys().collect::<Vec<_>>(),
            german.keys().collect::<Vec<_>>(),
            "every language carries the same keys"
        );
        for (key, text) in &english {
            assert!(!text.trim().is_empty(), "{key} has no English text");
            assert!(!german[key].trim().is_empty(), "{key} has no German text");
            assert_ne!(t(key).as_str(), *key, "{key} does not resolve");
        }
    }
}
