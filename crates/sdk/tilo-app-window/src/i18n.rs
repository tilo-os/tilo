//! The window's fluent catalogue: chrome only. What an app says comes from the app's own source.

// A window's frames are drawn in every window, so their words ship in the binary in every language
// they ship, not in a language pack: a window that speaks German says "Alle Notizen" beside the
// app's "Notiz". They are shared, so they follow its language and never choose it
// (`tilo_ui_base::i18n::WIDGET_CATALOG`).
pub static CATALOG: tilo_ui_base::i18n::EmbeddedCatalog = {
    let catalog = tilo_ui_base::i18n::EmbeddedCatalog::new(
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

#[must_use]
pub fn t(key: &str) -> String {
    CATALOG.t(key)
}

#[must_use]
pub fn t_args(key: &str, args: &[(&str, tilo_ui_base::i18n::ArgValue)]) -> String {
    CATALOG.t_args(key, args)
}

pub fn ensure() {
    CATALOG.ensure();
    // The toolkit's own words are shared with every window and choose no language of their own
    // (`tilo_ui_base::i18n::WIDGET_CATALOG`): they are read here from the start, so what an app says through
    // one of them reads as words, never as a message's name.
    tilo_ui_base::i18n::I18nManager::register_widget_messages();
}

/// A package's catalogues join the person's words, so what it declares reads in their language.
/// These are the words of the package the window shows: they say which languages the window speaks
/// ([`tilo_ui_base::i18n::speak_window_language`] chooses among them, once). A package the window uses takes
/// [`add_shared_catalogues`].
pub fn add_catalogues(catalogues: &[(String, String)]) {
    add(catalogues, tilo_ui_base::i18n::I18nManager::add_resource);
}

/// The catalogues of a package the window uses (a view of another app it calls): read in the
/// language the window speaks, and a word that language lacks reads in the package's source
/// language, never in another of its translations.
pub fn add_shared_catalogues(catalogues: &[(String, String)]) {
    add(
        catalogues,
        tilo_ui_base::i18n::I18nManager::add_shared_resource,
    );
}

fn add(catalogues: &[(String, String)], as_role: fn(&str, String)) {
    // Before the window has read a word of its own there is no manager to take them, and they would
    // be dropped: a package's words are read before the first widget draws.
    ensure();
    for (locale, text) in catalogues {
        as_role(locale, text.clone());
    }
}

/// The word `key` with `args` in the person's language, or `source` when no catalogue of theirs
/// holds it. A count reaches a plural rule as a number.
#[must_use]
pub fn word(key: &str, args: &[(String, String)], source: &str) -> String {
    ensure();
    let args: Vec<(&str, tilo_ui_base::i18n::ArgValue)> = args
        .iter()
        .map(|(name, value)| (name.as_str(), value_of(value)))
        .collect();
    let said = t_args(key, &args);
    if said == key { source.to_owned() } else { said }
}

/// An argument as Fluent takes it: a number where it is one, so a plural rule selects by it.
#[must_use]
pub fn value_of(value: &str) -> tilo_ui_base::i18n::ArgValue {
    match value.parse::<f64>() {
        Ok(number) => number.into(),
        Err(_) => value.into(),
    }
}

/// Moves when the person's language, or a catalogue of theirs, does
/// ([`tilo_ui_base::i18n::I18nManager::revision`]); a published package is published again when it moves.
#[must_use]
pub fn revision() -> u64 {
    tilo_ui_base::i18n::I18nManager::revision()
}

/// The platform's phrase `id` with `args` in the window's language, or `id` when no catalogue
/// holds it: the shared `Platform::phrase` for hosts backed by the toolkit's catalogues.
#[must_use]
pub fn phrase(id: &str, args: &[(String, String)]) -> String {
    word(id, args, id)
}

/// The language the person prefers (`de`, `en`), from the single [`tilo_ui_base::i18n::person_locale`]
/// source, even when the window speaks an English-only package's source language: what typed
/// dates and times are parsed in.
#[must_use]
pub fn language() -> String {
    tilo_ui_base::i18n::person_locale()
        .map(|locale| tilo_ui_base::i18n::language_of(&locale))
        .filter(|language| !language.is_empty())
        .unwrap_or_else(|| "en".to_owned())
}

/// Take in a package's catalogues for a single-package window: the window's own package chooses
/// the one language the window speaks (`speak_window_language`), while packages it uses ride
/// along in that language (`add_shared_catalogues`).
pub fn add_window_words(window: bool, catalogues: &[(String, String)]) {
    if window {
        tilo_ui_base::i18n::speak_window_language(catalogues);
    } else {
        add_shared_catalogues(catalogues);
    }
}

/// Take in a package's catalogues inside the session process: the session hosts several packages
/// on one thread and owns the desktop's active locale, so loading a package adds its catalogues
/// without narrowing the thread's active locale to that package alone.
pub fn add_session_words(window: bool, catalogues: &[(String, String)]) {
    if window {
        add_catalogues(catalogues);
    } else {
        add_shared_catalogues(catalogues);
    }
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
            "tilolang-view-failed",
            "tilolang-seen-failed",
            "tilolang-back",
            "tilolang-detail",
            "tilolang-ask-confirm",
            "tilolang-ask-decline",
            "tilolang-suggestion-says",
            "tilolang-suggested-by",
            "tilolang-ask-always",
            "tilolang-ask-denied",
            "tilolang-ask-needs-approval",
            "tilolang-invocation-pending",
            "tilolang-invocation-done",
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

    /// A notice an app sends names a message of the toolkit's catalogue: the window says it as
    /// words, in the language it reads.
    #[test]
    fn a_notice_an_app_sends_reads_in_the_windows_words() {
        ensure();
        let limit = [(
            "limit",
            tilo_ui_base::i18n::ArgValue::Text("8 MB".to_owned()),
        )];
        assert_eq!(
            t_args("rich-text-too-long", &limit),
            "This paste is too long to keep (limit 8 MB). Paste it in smaller parts."
        );
        let none: [(&str, tilo_ui_base::i18n::ArgValue); 0] = [];
        assert_eq!(
            t_args("rich-text-changed-while-typing", &none),
            "This paragraph changed while you were typing. Your edit wasn't kept."
        );
    }

    #[test]
    fn a_windows_language_covers_the_hosts_own_words() {
        ensure();
        tilo_ui_base::i18n::I18nManager::register_widget_messages();
        tilo_ui_base::i18n::I18nManager::set_locale("de-DE");
        assert_eq!(
            t("tilolang-sort"),
            "Sort",
            "no app speaks German here, so the window stays in its source language"
        );
        tilo_ui_base::i18n::I18nManager::add_resource("de-DE", "app-title = Titel".to_owned());
        assert_eq!(t("tilolang-sort"), "Sortieren");
        assert_eq!(
            tilo_ui_base::i18n::widget_text("window-toolbar-more"),
            "Weitere Aktionen"
        );
        tilo_ui_base::i18n::I18nManager::set_locale("en-US");
        assert_eq!(t("tilolang-sort"), "Sort");
    }

    /// A hosted window receiving `ToUi::Speak("en-US")` for an English-only app narrows only the
    /// window's display locale; a German person's preferred language stays German for typed dates.
    #[test]
    fn speak_from_a_hosted_english_app_leaves_the_persons_language_german() {
        use tilo_render_protocol::patch::{ToUi, Transport as _};

        struct Restore(Option<String>, String);
        impl Drop for Restore {
            fn drop(&mut self) {
                tilo_ui_base::i18n::set_person_locale(self.0.as_deref());
                tilo_ui_base::i18n::I18nManager::set_locale(&self.1);
            }
        }

        ensure();
        let prev_locale = tilo_ui_base::i18n::I18nManager::current_locale();
        let prev_person = tilo_ui_base::i18n::set_person_locale(Some("de-DE"));
        let _restore = Restore(prev_person, prev_locale);
        assert_eq!(language(), "de");

        let (mut app_end, window_end) = tilo_render_protocol::patch::local::pair();
        let mut window = crate::AppWindow::new(Box::new(window_end), Vec::new());
        app_end
            .send(
                tilo_render_protocol::patch::encode_batch(&[ToUi::Speak("en-US".to_owned())])
                    .unwrap(),
            )
            .unwrap();
        window.receive();

        assert_eq!(tilo_ui_base::i18n::I18nManager::current_locale(), "en-US");
        assert_eq!(t("tilolang-sort"), "Sort");
        assert_eq!(
            language(),
            "de",
            "ToUi::Speak with the window's narrowed English locale does not overwrite the person's German"
        );
    }
}
