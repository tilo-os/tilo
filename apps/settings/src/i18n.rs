//! Settings' Fluent catalogue.
//!
//! User-facing strings resolve by key through the toolkit's [`mod@ui::i18n`]
//! manager. The catalogues are embedded with `include_str!`, so a `--shots`
//! render, a unit test and a live session resolve the same copy. The locale
//! follows the usual POSIX variables and falls back per message to `en-US`.

ui::embedded_catalog!(static CATALOG, ["en-US", "de-DE"]);

/// Native Settings copy takes precedence; shared provider copy fills missing keys.
pub fn t(key: &str) -> String {
    let local = CATALOG.t(key);
    if local == key {
        provider_settings_ui::i18n::t(key)
    } else {
        local
    }
}

pub fn t_args(key: &str, args: &[(&str, ui::i18n::ArgValue)]) -> String {
    let local = CATALOG.t_args(key, args);
    if local == key {
        provider_settings_ui::i18n::t_args(key, args)
    } else {
        local
    }
}

/// The English names of a `(label, value)` table, as an effect's or verb's
/// choices.
pub(crate) fn english<T>(
    table: &'static [(&'static str, T)],
) -> impl Iterator<Item = &'static str> {
    table.iter().map(|(label, _)| *label)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    const SOURCE_FTL: &str = include_str!("../locales/en-US.ftl");
    const DE_FTL: &str = include_str!("../locales/de-DE.ftl");

    /// `key -> pattern` for every message in a catalogue.
    fn messages(source: &str) -> BTreeMap<&str, &str> {
        source
            .lines()
            .filter(|line| !line.starts_with('#') && !line.starts_with(' '))
            .filter_map(|line| line.split_once(" = "))
            .map(|(key, pattern)| (key.trim(), pattern))
            .collect()
    }

    fn variables(pattern: &str) -> Vec<&str> {
        let mut vars: Vec<&str> = pattern
            .split("{ $")
            .skip(1)
            .filter_map(|rest| rest.split_once(' ').map(|(name, _)| name))
            .collect();
        vars.sort_unstable();
        vars
    }

    #[test]
    fn native_agent_copy_precedes_shared_fallback() {
        assert_eq!(t("agent-vertex-title"), CATALOG.t("agent-vertex-title"));
        assert_ne!(t("agent-vertex-title"), "agent-vertex-title");
        assert_eq!(
            t("agent-sign-in-custody"),
            provider_settings_ui::i18n::t("agent-sign-in-custody")
        );
        let args = [("error", "diagnostic".into())];
        assert_eq!(
            t_args("agent-sign-in-forget-err", &args),
            CATALOG.t_args("agent-sign-in-forget-err", &args)
        );
    }

    #[test]
    fn every_catalogue_entry_resolves_to_a_real_string() {
        let keys = messages(SOURCE_FTL);
        assert!(!keys.is_empty());
        for key in keys.keys() {
            let value = t(key);
            assert_ne!(value, *key, "{key} has no entry in the catalogue");
            assert!(!value.trim().is_empty(), "{key} resolves to nothing");
        }
    }

    #[test]
    fn the_german_catalogue_matches_the_source_key_for_key() {
        let source = messages(SOURCE_FTL);
        let german = messages(DE_FTL);
        assert_eq!(
            source.keys().collect::<Vec<_>>(),
            german.keys().collect::<Vec<_>>(),
            "de-DE must translate exactly the en-US keys"
        );
        for (key, pattern) in &source {
            assert_eq!(
                variables(pattern),
                variables(german[key]),
                "placeholders changed in the German {key}"
            );
            assert!(!german[key].trim().is_empty(), "{key} is empty in de-DE");
        }
    }
}
