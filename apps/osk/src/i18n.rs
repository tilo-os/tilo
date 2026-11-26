//! The keyboard's Fluent catalogue, embedded so a `--shots` render, a unit
//! test and a live session resolve the same strings.

ui::embedded_catalog!(pub fn, ["de-DE", "en-US"]);

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
        assert_eq!(catalog.t("osk-name"), "Bildschirmtastatur");
        catalog.set_locale("en-US");
        assert_eq!(catalog.t("osk-name"), "On-screen keyboard");
    }
}
