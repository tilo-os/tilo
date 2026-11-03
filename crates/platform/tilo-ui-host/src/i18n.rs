//! The host's fluent catalogue: chrome only. What an app says comes from the app's own source.

ui::embedded_catalog!(pub fn, ["en-US", "de-DE"]);

/// Load the catalogue on this thread if it is not loaded yet.
pub fn ensure() {
    CATALOG.ensure();
}

/// A package's catalogues join the person's words, so what it declares reads in their language
/// (docs/designs/app-language/i18n.md): each `(locale, Fluent text)` the module carries is a
/// catalogue the toolkit falls back through, in the language selected here.
pub fn add_catalogues(catalogues: &[(String, String)]) {
    // Before the host has read a word of its own there is no manager to take them, and they
    // would be dropped: a package's words are read before the first widget draws.
    ensure();
    for (locale, text) in catalogues {
        ui::i18n::I18nManager::add_resource(locale, text.clone());
    }
}

/// The word `key` with `args` in the person's language — a declaration's label, its `noun` by
/// `count`, or the platform's phrase — and `source` when no catalogue of theirs holds it.
/// A count reaches a plural rule as a number.
#[must_use]
pub fn word(key: &str, args: &[(String, String)], source: &str) -> String {
    ensure();
    let args: Vec<(&str, ui::i18n::ArgValue)> = args
        .iter()
        .map(|(name, value)| (name.as_str(), value_of(value)))
        .collect();
    let said = t_args(key, &args);
    if said == key { source.to_owned() } else { said }
}

/// An argument as Fluent takes it: a number where it is one, so a plural rule selects by it.
#[must_use]
pub fn value_of(value: &str) -> ui::i18n::ArgValue {
    match value.parse::<f64>() {
        Ok(number) => number.into(),
        Err(_) => value.into(),
    }
}

/// What the words resolve at now: it moves when the person's language, or a catalogue of theirs,
/// does ([`ui::i18n::I18nManager::revision`]). A package published in one language is published
/// again when it moves.
#[must_use]
pub fn revision() -> u64 {
    ui::i18n::I18nManager::revision()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_the_host_uses_resolves_to_a_real_string() {
        ensure();
        for key in [
            "tilolang-title",
            "tilolang-home-heading",
            "tilolang-home-empty",
            "tilolang-open-failed",
            "tilolang-seen-failed",
            "tilolang-back",
            "tilolang-detail",
            "tilolang-ask-title",
            "tilolang-ask-confirm",
            "tilolang-ask-decline",
            "tilolang-ask-always",
            "tilolang-ask-denied",
            "tilolang-ask-needs-approval",
            "tilolang-invocation-pending",
            "tilolang-invocation-done",
            "tilolang-invocation-suggested",
            "tilolang-invocation-declined",
            "tilolang-invocation-failed",
            "tilolang-invocation-denied",
            "tilolang-embed-error",
            "tilolang-typing-unplaced",
            "tilolang-form-cancel",
        ] {
            let text = t(key);
            assert_ne!(text, key, "{key} has no entry in the catalogue");
            assert!(!text.trim().is_empty(), "{key} resolves to nothing");
        }
    }

    /// A window's language reaches the host's own words: a package that ships German makes
    /// its frames' sort menu and its More button speak German too, where the desktop's own
    /// catalogue would leave them in its source language.
    #[test]
    fn a_windows_language_covers_the_hosts_own_words() {
        ensure();
        ui::i18n::I18nManager::register_widget_messages();
        ui::i18n::I18nManager::set_locale("de-DE");
        assert_eq!(t("tilolang-sort"), "Sortieren");
        assert_eq!(ui::i18n!("window-toolbar-more"), "Weitere Aktionen");
        ui::i18n::I18nManager::set_locale("en-US");
        assert_eq!(t("tilolang-sort"), "Sort");
    }
}
