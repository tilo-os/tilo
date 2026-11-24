//! The session's Fluent catalogue: what its notices say.
//! Every notice the session shows the person is a catalogue key resolved through the toolkit's
//! [`mod@ui::i18n`] manager, never a literal. The catalogues are embedded with `include_str!` , so
//! a release session and a unit test resolve the same copy; on a thread with no manager yet the
//! user's locale comes from the environment, and a message a translation lacks falls back to
//! `en-US` .

#[cfg(test)]
const CATALOGUES: &[(&str, &str)] = &[
    ("en-US", include_str!("../locales/en-US.ftl")),
    ("de-DE", include_str!("../locales/de-DE.ftl")),
];

ui::embedded_catalog!(pub(crate) fn, ["en-US", "de-DE"]);

/// The message `key` in the person's language with `args`, and `source` where the catalogue holds
/// no such message: what a host says for a thing the catalogue has no word for.
#[cfg(target_os = "linux")]
pub(crate) fn word(key: &str, args: &[(&str, String)], source: &str) -> String {
    let args: Vec<(&str, ui::i18n::ArgValue)> = args
        .iter()
        .map(|(name, value)| (*name, value.clone().into()))
        .collect();
    let said = t_args(key, &args);
    if said == key { source.to_owned() } else { said }
}

/// Load the copy a notice or a query resolves on this thread.
///
/// A catalogue's first use lists the installed language packs and stats the
/// files in them. The session's notices and search's rows are first read when
/// a notice arrives or a query runs, long after startup, so they load here,
/// where the loop is not yet running.
#[cfg_attr(
    target_arch = "wasm32",
    allow(
        dead_code,
        reason = "the page loads the catalogue with its own host next"
    )
)]
pub(crate) fn ensure() {
    CATALOG.ensure();
    tilo_desktop_search::i18n::ensure();
}

/// The words of the tilolang packages a development session seeds, in the person's language: their
/// catalogues ride along with the session's — a declaration's label, description and noun read
/// through them (`docs/designs/app-language/i18n.md`) — without saying which language the session
/// itself draws in. A seeded package that ships German leaves the session's own words (the
/// toolkit's, the desktop's) English on a German device; a session that has its own German reads
/// them German still.
#[cfg(target_os = "linux")]
pub(crate) struct SeededWords;

/// Let the session read `catalogues` beside its own words: shared, so a package's language never
/// becomes the one the session draws in.
#[cfg(target_os = "linux")]
fn share(catalogues: &[(String, String)]) {
    ensure();
    tilo_app_window::i18n::add_shared_catalogues(catalogues);
}

#[cfg(target_os = "linux")]
impl tilolang_tilo::runtime::Words for SeededWords {
    fn add(&self, catalogues: &[(String, String)]) {
        share(catalogues);
    }

    fn add_used(&self, catalogues: &[(String, String)]) {
        share(catalogues);
    }

    fn word(&self, key: &str, args: &[(String, String)], source: &str) -> String {
        ensure();
        tilo_app_window::i18n::word(key, args, source)
    }

    fn revision(&self) -> u64 {
        ui::i18n::I18nManager::revision()
    }

    fn language(&self) -> String {
        ui::i18n::I18nManager::window_locale()
    }
}

/// The language the session's surfaces are drawn in, followed from the person's settings.
#[cfg(target_os = "linux")]
pub(crate) struct Language {
    /// The settings version the language was last followed at, so the document is read again only
    /// when it changed.
    settings: std::cell::Cell<settings_store::SettingsFollower>,
}

#[cfg(target_os = "linux")]
impl Language {
    pub(crate) fn new() -> Self {
        Self {
            settings: std::cell::Cell::new(settings_store::SettingsFollower::default()),
        }
    }

    /// Follow the person's language when their settings changed since the last pass. Returns
    /// whether the words on screen are now resolved in another language: the caller lays out what
    /// it keeps between frames.
    pub(crate) fn follow(&self) -> bool {
        let mut settings = self.settings.get();
        if !settings.changed(settings_store::stamp()) {
            return false;
        }
        self.settings.set(settings);
        let before = ui::I18nManager::revision();
        ui::i18n::follow_configured_language();
        ui::I18nManager::revision() != before
    }
}

#[cfg(target_os = "linux")]
impl crate::Session {
    /// Speak the person's language in the session's own surfaces: the manager's selection follows
    /// their settings (see [`Language`]). Every tilolang host running the person's apps is told
    /// too, so the windows it draws relabel where they stand.
    pub(crate) fn follow_language(&self) {
        if self.language.follow() {
            if let Some(apps) = self.shared.launch.clone() {
                apps.locale_changed();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_copy_lint::catalog;

    tilo_test_env::scenario! {
        fn every_notice_has_real_words_in_every_locale;

        phase every_catalogue_entry_resolves_to_a_real_string;
        {
            let keys = catalog::messages(CATALOGUES[0].1);
            assert!(!keys.is_empty());
            for key in keys.keys() {
                let value = t(key);
                assert_ne!(&value, key, "{key} has no entry in the catalogue");
                assert!(!value.trim().is_empty(), "{key} resolves to nothing");
            }
        }

        phase every_notice_is_translated_in_every_locale;
        // A key only English defines would show English on a German desktop,
        // and a German key English lacks is dead copy.
        {
            catalog::check_translations(CATALOGUES);
        }

        phase a_notice_interpolates_its_facts;
        {
            let body = t_args(
                "storage-low-body",
                &[("free", "2.7 GB".into()), ("volume", "your files".into())],
            );
            assert!(body.starts_with("2.7 GB left for your files."), "{body}");
            let froze = t_args("crash-notice-froze-body", &[("time", "22:52".into())]);
            assert!(froze.contains("22:52"), "{froze}");
        }
    }

    #[test]
    fn german_desktops_resolve_german_copy() {
        let english = t("storage-full-title");
        ui::i18n::I18nManager::set_locale("de-DE");
        let german = t("storage-full-title");
        ui::i18n::I18nManager::set_locale("en-US");
        assert_eq!(english, "Storage is full");
        assert_eq!(german, "Der Speicher ist voll");
    }

    /// What the layout publishes reads in the person's language, and a key its catalogue
    /// lacks keeps the source words a host names it with.
    #[test]
    fn the_layout_words_resolve_in_the_persons_language() {
        // A catalogue installs itself on its first use, and selecting a language before that
        // would be replaced by the desktop's own.
        assert_eq!(
            word("session-resource-layout-history", &[], "Layout history"),
            "Layout history"
        );
        ui::i18n::I18nManager::set_locale("de-DE");
        assert_eq!(
            word("session-resource-layout-history", &[], "Layout history"),
            "Layoutverlauf"
        );
        assert_eq!(
            word(
                "session-strip-width",
                &[("width", "40".to_owned())],
                "Strip, 40% wide"
            ),
            "Leiste, 40 % breit"
        );
        assert_eq!(word("no-such-word", &[], "Source"), "Source");
        ui::i18n::I18nManager::set_locale("en-US");
        assert_eq!(word("session-resource-layout", &[], "Layout"), "Layout");
    }

    /// With no settings store (`stamp()` is `None`), the language follower refreshes once and
    /// skips later passes instead of re-applying the configured language on every pass.
    #[cfg(target_os = "linux")]
    #[test]
    fn without_a_settings_store_language_refreshes_once() {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("HOME", "").remove("XDG_DATA_HOME");
        let _source = settings_store::scoped_source(None);
        assert_eq!(settings_store::stamp(), None);

        ensure();
        let before = ui::i18n::I18nManager::current_locale();
        let alternate = if ui::i18n::preferred_language()
            .as_deref()
            .is_some_and(|locale| locale.starts_with("de"))
        {
            "en-US"
        } else {
            "de-DE"
        };
        ui::i18n::I18nManager::set_locale(alternate);
        assert_eq!(ui::i18n::I18nManager::current_locale(), alternate);

        let follow = Language::new();
        assert!(
            follow.follow(),
            "the first pass follows the language even without a settings store"
        );
        assert_ne!(ui::i18n::I18nManager::current_locale(), alternate);

        ui::i18n::I18nManager::set_locale(alternate);
        assert_eq!(ui::i18n::I18nManager::current_locale(), alternate);
        assert!(
            !follow.follow(),
            "the second pass skips refreshing while there is still no stamp"
        );
        assert_eq!(ui::i18n::I18nManager::current_locale(), alternate);

        ui::i18n::I18nManager::set_locale(&before);
    }
}
