//! Copy for the missing-decoder offer, resolved by the shared Fluent manager.
use ui::i18n::{ArgValue, EmbeddedCatalog};

/// The player's Fluent catalogs, source locale first.
pub(crate) const LOCALES: &[(&str, &str)] = &[
    ("en-US", include_str!("../locales/en-US.ftl")),
    ("de-DE", include_str!("../locales/de-DE.ftl")),
];

pub(crate) static CATALOG: EmbeddedCatalog = catalog();
const fn catalog() -> EmbeddedCatalog {
    let catalog = EmbeddedCatalog::new("en-US", LOCALES).with_system_locale();
    #[cfg(test)]
    let catalog = catalog.with_pinned_locale("en-US");
    catalog
}
pub(crate) fn t(key: &str) -> String {
    CATALOG.t(key)
}
pub(crate) fn t_args(key: &str, args: &[(&str, ArgValue)]) -> String {
    CATALOG.t_args(key, args)
}

/// German is one of the languages the player ships, and it reads in German: a
/// video's library drawer speaks German on a device set to German, from
/// `locales/de-DE.ftl` beside the source catalogue.
#[cfg(test)]
mod german_catalogue_tests {
    #[test]
    fn a_german_window_reads_the_player_catalogue() {
        use super::CATALOG;
        assert!(
            CATALOG.languages().contains(&"de-DE"),
            "the app ships German among {:?}",
            CATALOG.languages()
        );
        CATALOG.set_locale("de-DE");
        assert_eq!(CATALOG.t("video-queue"), "Warteschlange");
        assert_eq!(CATALOG.t("video-watch-later"), "Später ansehen");
        CATALOG.set_locale("en-US");
        assert_eq!(CATALOG.t("video-queue"), "Queue");
    }
}
