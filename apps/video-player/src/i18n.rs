use ui::i18n::{ArgValue, EmbeddedCatalog};

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

/// German copy comes from `locales/de-DE.ftl`, beside the source catalogue.
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

    /// The actions' names come from the catalogue, in the window's language, not words
    /// written in the code.
    #[test]
    fn the_actions_are_named_from_the_catalogue() {
        use super::CATALOG;
        use ui::tilo_resource::effect::Effect as _;

        let label = |id: &str| {
            crate::actions::VideoEffect::rows()
                .into_iter()
                .find(|row| row.id == id)
                .expect("the player declares it")
                .label
        };
        for row in crate::actions::VideoEffect::rows() {
            assert!(
                row.label.catalogue.is_some(),
                "{} is named from the catalogue, not by a literal",
                row.id
            );
        }
        CATALOG.set_locale("en-US");
        assert_eq!(label("open-video").read(), "Open video");
        CATALOG.set_locale("de-DE");
        assert_eq!(label("open-video").read(), "Video öffnen");
        assert_eq!(label("toggle-fullscreen").read(), "Vollbild umschalten");
        CATALOG.set_locale("en-US");
    }
}
