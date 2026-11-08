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

/// Load the copy a notice or a query resolves on this thread.
///
/// A catalogue's first use lists the installed language packs and stats the
/// files in them. The session's notices and search's rows are first read when
/// a notice arrives or a query runs, long after startup, so they load here,
/// where the loop is not yet running.
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
pub(crate) struct SeededWords;

/// Let the session read `catalogues` beside its own words: shared, so a package's language never
/// becomes the one the session draws in.
fn share(catalogues: &[(String, String)]) {
    ensure();
    for (locale, text) in catalogues {
        ui::i18n::I18nManager::add_shared_resource(locale, text.clone());
    }
}

impl tilolang_tilo::runtime::Words for SeededWords {
    fn add(&self, catalogues: &[(String, String)]) {
        share(catalogues);
    }

    fn add_used(&self, catalogues: &[(String, String)]) {
        share(catalogues);
    }

    fn word(&self, key: &str, args: &[(String, String)], source: &str) -> String {
        ensure();
        let args: Vec<(&str, ui::i18n::ArgValue)> = args
            .iter()
            .map(|(name, value)| (name.as_str(), tilo_ui_host::i18n::value_of(value)))
            .collect();
        let said = ui::i18n::I18nManager::format(key, &args);
        if said == key { source.to_owned() } else { said }
    }

    fn revision(&self) -> u64 {
        ui::i18n::I18nManager::revision()
    }

    fn language(&self) -> String {
        ui::i18n::I18nManager::window_locale()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_copy_lint::catalog;

    #[test]
    fn every_catalogue_entry_resolves_to_a_real_string() {
        let keys = catalog::messages(CATALOGUES[0].1);
        assert!(!keys.is_empty());
        for key in keys.keys() {
            let value = t(key);
            assert_ne!(&value, key, "{key} has no entry in the catalogue");
            assert!(!value.trim().is_empty(), "{key} resolves to nothing");
        }
    }

    /// A key only English defines would show English on a German desktop,
    /// and a German key English lacks is dead copy.
    #[test]
    fn every_notice_is_translated_in_every_locale() {
        catalog::check_translations(CATALOGUES);
    }

    #[test]
    fn a_notice_interpolates_its_facts() {
        let body = t_args(
            "storage-low-body",
            &[("free", "2.7 GB".into()), ("volume", "your files".into())],
        );
        assert!(body.starts_with("2.7 GB left for your files."), "{body}");
        let froze = t_args("crash-notice-froze-body", &[("time", "22:52".into())]);
        assert!(froze.contains("22:52"), "{froze}");
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
}
