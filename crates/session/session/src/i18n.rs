//! The session's Fluent catalogue: what its notices say.
//!
//! Every notice the session shows the person is a catalogue key resolved
//! through the toolkit's [`mod@ui::i18n`] manager, never a literal. The
//! catalogues are embedded with `include_str!`, so a release session and a
//! unit test resolve the same copy; on a thread with no manager yet the
//! user's locale comes from the environment, and a message a translation
//! lacks falls back to `en-US`.

#[cfg(test)]
const CATALOGUES: &[(&str, &str)] = &[
    ("en-US", include_str!("../locales/en-US.ftl")),
    ("de-DE", include_str!("../locales/de-DE.ftl")),
];

ui::embedded_catalog!(pub(crate) fn, ["en-US", "de-DE"]);

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

    /// A key the code asks for but a catalogue lacks renders as the raw key
    /// (or English on a German desktop), so every one must be defined.
    #[test]
    fn every_key_used_in_code_is_in_every_catalogue() {
        // The crate's other modules ask the toolkit's catalogues of other
        // crates for their copy; only the notices are this catalogue's.
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let alone = tempfile::tempdir().unwrap();
        std::fs::copy(
            manifest.join("src/notices.rs"),
            alone.path().join("notices.rs"),
        )
        .unwrap();
        catalog::UsedKeys::scan(&[alone.path()])
            .min_keys(20)
            .check_defined(CATALOGUES);
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
