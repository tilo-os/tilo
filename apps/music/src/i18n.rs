//! Music's Fluent copy, resolved by the shared manager.

ui::embedded_catalog!(pub(crate) fn, ["de-DE", "en-US"]);

/// German is one of the languages the app ships, and it reads in German: the
/// copy comes from `locales/de-DE.ftl`, never from the English source
/// catalogue beside it.
#[cfg(test)]
mod german_window {
    #[test]
    fn a_german_window_reads_the_catalogue() {
        let catalog = &super::CATALOG;
        assert!(
            catalog.languages().contains(&"de-DE"),
            "the app ships German among {:?}",
            catalog.languages()
        );
        catalog.set_locale("de-DE");
        assert_eq!(catalog.t("music-title"), "Musik");
        catalog.set_locale("en-US");
        assert_eq!(catalog.t("music-title"), "Music");
    }
}
