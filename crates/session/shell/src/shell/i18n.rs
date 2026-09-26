//! The shell's Fluent catalogue.
//!
//! Chrome strings are keys resolved through the toolkit's [`mod@ui::i18n`]
//! manager, never literals. The catalogues are embedded with `include_str!`,
//! so a `--shots` render, a unit test and a live session resolve the same
//! copy. The user's locale comes from the environment (`LC_ALL`,
//! `LC_MESSAGES`, `LANGUAGE`, `LANG`), exactly as the toolkit's own widget
//! strings choose it; a message missing from a translation falls back to
//! `en-US`.

use ui::i18n::{ArgValue, EmbeddedCatalog};

const SOURCE_FTL: &str = include_str!("../../locales/en-US.ftl");
const DE_FTL: &str = include_str!("../../locales/de-DE.ftl");

/// Loaded per thread on first use; a host's manager is joined, never replaced.
/// The probe is a key every catalogue revision defines.
static CATALOG: EmbeddedCatalog = catalog();

const fn catalog() -> EmbeddedCatalog {
    let catalog = EmbeddedCatalog::new(
        "en-US",
        &[("en-US", SOURCE_FTL), ("de-DE", DE_FTL)],
        "continuity-dismiss",
    )
    .with_system_locale();
    // Unit tests assert source copy throughout the shell; keep them
    // deterministic on any developer or CI host locale.
    #[cfg(test)]
    let catalog = catalog.with_pinned_locale("en-US");
    catalog
}

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
    use fluent::{FluentBundle, FluentResource};
    use std::collections::{BTreeMap, BTreeSet};

    /// Every message in a catalogue, keyed by id, with its full pattern
    /// (continuation lines included).
    fn catalog_messages(source: &str) -> BTreeMap<String, String> {
        let mut messages = BTreeMap::new();
        let mut current: Option<(String, String)> = None;
        for line in source.lines() {
            if !line.starts_with(char::is_whitespace)
                && !line.starts_with('#')
                && let Some((key, value)) = line.split_once(" = ")
            {
                if let Some((key, value)) = current.take() {
                    messages.insert(key, value);
                }
                current = Some((key.trim().to_string(), value.to_string()));
            } else if let Some((_, value)) = current.as_mut()
                && (line.starts_with(char::is_whitespace) || line.trim() == "}")
            {
                value.push('\n');
                value.push_str(line);
            } else if let Some((key, value)) = current.take() {
                messages.insert(key, value);
            }
        }
        if let Some((key, value)) = current {
            messages.insert(key, value);
        }
        messages
    }

    fn variables(pattern: &str) -> BTreeSet<String> {
        pattern
            .split('$')
            .skip(1)
            .map(|rest| {
                rest.chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
                    .collect()
            })
            .collect()
    }

    fn strict_bundle(locale: &str, source: &str) -> FluentBundle<FluentResource> {
        let resource = FluentResource::try_new(source.to_string())
            .unwrap_or_else(|(_, errors)| panic!("{locale}.ftl has syntax errors: {errors:?}"));
        let mut bundle = FluentBundle::new(vec![locale.parse().expect("locale id")]);
        bundle.set_use_isolating(false);
        bundle
            .add_resource(resource)
            .unwrap_or_else(|errors| panic!("{locale}.ftl defines a key twice: {errors:?}"));
        bundle
    }

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
        let keys = catalog_messages(SOURCE_FTL);
        assert!(!keys.is_empty());
        for key in keys.keys() {
            let value = t(key);
            assert_ne!(&value, key, "{key} has no entry in the catalogue");
            assert!(!value.trim().is_empty(), "{key} resolves to nothing");
        }
    }

    /// A key only English defines would show English on a German desktop,
    /// and a German key English lacks is dead copy. Both catalogues must
    /// define the same keys with the same variables, and parse cleanly.
    #[test]
    fn every_shell_string_is_translated_in_every_locale() {
        let source = catalog_messages(SOURCE_FTL);
        for (locale, catalogue) in [("de-DE", DE_FTL)] {
            let translated = catalog_messages(catalogue);
            let missing: Vec<_> = source
                .keys()
                .filter(|key| !translated.contains_key(*key))
                .collect();
            assert!(missing.is_empty(), "{locale}.ftl lacks {missing:?}");
            let extra: Vec<_> = translated
                .keys()
                .filter(|key| !source.contains_key(*key))
                .collect();
            assert!(
                extra.is_empty(),
                "{locale}.ftl has keys en-US lacks: {extra:?}"
            );
            for (key, pattern) in &source {
                assert_eq!(
                    variables(pattern),
                    variables(&translated[key]),
                    "{locale}.ftl changes the variables of {key}"
                );
            }
            let bundle = strict_bundle(locale, catalogue);
            for key in source.keys() {
                assert!(
                    bundle
                        .get_message(key)
                        .and_then(|message| message.value())
                        .is_some(),
                    "{locale}.ftl has no value for {key}"
                );
            }
        }
        strict_bundle("en-US", SOURCE_FTL);
    }

    #[test]
    fn german_desktops_resolve_german_copy() {
        let english = t("continuity-dismiss");
        ui::i18n::I18nManager::set_locale("de-DE");
        let german = t("continuity-dismiss");
        ui::i18n::I18nManager::set_locale("en-US");
        assert_eq!(english, "Dismiss");
        assert_eq!(german, "Ausblenden");
    }
}
