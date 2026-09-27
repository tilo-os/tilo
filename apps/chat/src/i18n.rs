//! Chat's Fluent catalogue.
//!
//! User-facing strings are keys, never literals, resolved through the toolkit's
//! [`mod@ui::i18n`] manager. The catalogue is embedded with `include_str!`, so there
//! is no build step and no runtime file I/O — a `--shots` render and a unit test
//! resolve the same strings a live session does.

use ui::i18n::{ArgValue, EmbeddedCatalog};

/// Loaded per thread on first use; a host's manager is joined, never replaced.
/// The probe is a key every catalogue revision defines.
static CATALOG: EmbeddedCatalog = EmbeddedCatalog::new(
    "en-US",
    &[("en-US", SOURCE_FTL), ("de-DE", GERMAN_FTL)],
    "chat-mention-agent-tag",
);

const SOURCE_FTL: &str = include_str!("../locales/en-US.ftl");
const GERMAN_FTL: &str = include_str!("../locales/de-DE.ftl");

/// Load the catalogue on this thread if it is not loaded yet.
pub fn ensure() {
    CATALOG.ensure();
}

/// Resolve a Chat string key.
pub fn t(key: &str) -> String {
    CATALOG.t(key)
}

/// Resolve a Chat string key that interpolates variables.
///
/// The values are the app's own words — an operation's label, a participant's
/// name — so the *sentence* around them stays in the catalogue and only the
/// nouns are substituted. Building the line with `format!` instead would put
/// English word order in the source and leave nothing to translate.
pub fn t_args(key: &str, args: &[(&str, ArgValue)]) -> String {
    CATALOG.t_args(key, args)
}

/// The emoji catalogue in the desktop's language: names, group labels and
/// search follow it, with English kept for anything it does not translate.
pub fn emoji_catalogue() -> &'static emoji::Catalogue {
    emoji::Catalogue::for_locale(&ui::i18n::I18nManager::current_locale())
}

#[cfg(test)]
#[path = "i18n_copy_lint_tests.rs"]
mod copy_lint_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Every message in a catalogue, keyed by id, with the variables it uses.
    fn messages(ftl: &str) -> BTreeMap<String, Vec<String>> {
        let mut found: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut current: Option<String> = None;
        for line in ftl.lines() {
            let key = line.split_once(" =").map(|(key, _)| key).filter(|key| {
                !key.is_empty()
                    && key
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            });
            if let Some(key) = key {
                current = Some(key.to_string());
                found.entry(key.to_string()).or_default();
            } else if line.starts_with('#') || line.trim().is_empty() {
                current = None;
            }
            let Some(key) = &current else { continue };
            let vars = found.entry(key.clone()).or_default();
            for (start, _) in line.match_indices('$') {
                let name: String = line
                    .get(start + 1..)
                    .unwrap_or_default()
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                    .collect();
                if !vars.contains(&name) {
                    vars.push(name);
                }
            }
        }
        for vars in found.values_mut() {
            vars.sort();
        }
        found
    }

    #[test]
    fn german_matches_the_source_catalogue_key_for_key() {
        let source = messages(SOURCE_FTL);
        let german = messages(GERMAN_FTL);
        assert!(source.len() > 100, "the source catalogue lost its messages");
        assert_eq!(
            source.keys().collect::<Vec<_>>(),
            german.keys().collect::<Vec<_>>(),
            "de-DE.ftl and en-US.ftl must define the same messages"
        );
        for (key, vars) in &source {
            assert_eq!(
                Some(vars),
                german.get(key),
                "{key} changed its variables in German"
            );
        }
    }

    #[test]
    fn every_message_resolves_in_both_locales() {
        // A key that resolves to itself has no catalogue entry: the failure
        // mode that ships an untranslatable id to a screen.
        for (locale, ftl) in [("en-US", SOURCE_FTL), ("de-DE", GERMAN_FTL)] {
            CATALOG.set_locale(locale);
            for (key, vars) in messages(ftl) {
                if !vars.is_empty() {
                    continue;
                }
                let s = t(&key);
                assert_ne!(s, key, "{key} has no {locale} entry");
                assert!(
                    !s.trim().is_empty(),
                    "{key} resolves to nothing in {locale}"
                );
            }
        }
        assert_eq!(t("chat-call-mute"), "Stummschalten");
        CATALOG.set_locale("en-US");
        assert_eq!(t("chat-call-mute"), "Mute");
    }
}
