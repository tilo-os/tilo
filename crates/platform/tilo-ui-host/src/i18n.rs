//! The host's fluent catalogue: chrome only. What an app says comes from the app's own source.

// A window's frames are drawn in every window, so their words are part of the binary, in every
// language they ship, rather than read from a language pack: a window that speaks German says
// "Alle Notizen" beside the app's "Notiz" even where nobody installed a pack. They are shared
// rather than a window's own, so they follow its language and never choose it (see
// `ui::i18n::WIDGET_CATALOG`).
static CATALOG: ui::i18n::EmbeddedCatalog = {
    let catalog = ui::i18n::EmbeddedCatalog::new(
        "en-US",
        &[
            ("en-US", include_str!("../locales/en-US.ftl")),
            ("de-DE", include_str!("../locales/de-DE.ftl")),
        ],
    )
    .with_system_locale()
    .as_shared();
    #[cfg(test)]
    let catalog = catalog.with_pinned_locale("en-US");
    catalog
};

/// The catalogue's word `key` on this thread.
#[must_use]
pub fn t(key: &str) -> String {
    CATALOG.t(key)
}

/// The catalogue's word `key`, interpolating `args`.
#[must_use]
pub fn t_args(key: &str, args: &[(&str, ui::i18n::ArgValue)]) -> String {
    CATALOG.t_args(key, args)
}

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
            "tilolang-suggestion-accept",
            "tilolang-suggestion-dismiss",
            "tilolang-suggestion-says",
            "tilolang-suggested-by",
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

    /// A window's language reaches the host's own words: a window that speaks German — because
    /// the app it shows ships German — says its frames' sort menu and its More button in
    /// German too, where the desktop's own catalogue would leave them in its source language.
    /// The catalogue is shared, so the host's German alone does not make an English app's
    /// window German: the app's own catalogue is what says which language the window speaks.
    #[test]
    fn a_windows_language_covers_the_hosts_own_words() {
        ensure();
        ui::i18n::I18nManager::register_widget_messages();
        ui::i18n::I18nManager::set_locale("de-DE");
        assert_eq!(
            t("tilolang-sort"),
            "Sort",
            "no app speaks German here, so the window stays in its source language"
        );
        ui::i18n::I18nManager::add_resource("de-DE", "app-title = Titel".to_owned());
        assert_eq!(t("tilolang-sort"), "Sortieren");
        assert_eq!(ui::i18n!("window-toolbar-more"), "Weitere Aktionen");
        ui::i18n::I18nManager::set_locale("en-US");
        assert_eq!(t("tilolang-sort"), "Sort");
    }
}
