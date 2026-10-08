//! The Media step's copy follows the shared Fluent catalogue.
use ui::i18n::EmbeddedCatalog;
static CATALOG: EmbeddedCatalog = catalog();
const fn catalog() -> EmbeddedCatalog {
    let catalog = EmbeddedCatalog::new(
        "en-US",
        &[
            ("en-US", include_str!("../locales/en-US.ftl")),
            ("de-DE", include_str!("../locales/de-DE.ftl")),
        ],
        "installer-media-title",
    )
    .with_system_locale();
    #[cfg(test)]
    let catalog = catalog.with_pinned_locale("en-US");
    catalog
}
pub(crate) fn t(key: &str) -> String {
    CATALOG.t(key)
}
