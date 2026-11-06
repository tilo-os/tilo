//! Fluent catalogue for setup's semantic resource and action surface.

use ui::i18n::ArgValue;

pub fn t(key: &str) -> String {
    crate::resource::CATALOG.t(key)
}

pub fn t_args(key: &str, args: &[(&str, ArgValue)]) -> String {
    crate::resource::CATALOG.t_args(key, args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::i18n::I18nManager;

    #[test]
    fn setup_catalog_resolves_and_ships_the_languages_beside_its_source() {
        // A test build pins the catalogue's source language, so the copy a
        // test asserts is the same on every machine.
        assert_eq!(t("imager-setup-action-next"), "Continue setup");
        assert_eq!(I18nManager::current_locale(), "en-US");
        // The German setup screen reads the catalogue the crate ships beside
        // its source, which a session in German picks up from the packs.
        let german = crate::resource::CATALOG
            .every_source()
            .into_iter()
            .find(|(locale, _)| *locale == "de-DE")
            .expect("the German catalogue ships beside the source");
        assert!(
            german
                .1
                .contains("imager-setup-action-next = Einrichtung fortsetzen"),
            "the German catalogue carries setup's own words"
        );
    }
}
