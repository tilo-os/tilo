//! Copy for the missing-decoder offer, resolved by the shared Fluent manager.
use ui::i18n::{ArgValue, EmbeddedCatalog};
static CATALOG: EmbeddedCatalog = catalog();
const fn catalog() -> EmbeddedCatalog {
    let catalog = EmbeddedCatalog::new(
        "en-US",
        &[
            ("en-US", include_str!("../locales/en-US.ftl")),
            ("de-DE", include_str!("../locales/de-DE.ftl")),
        ],
        "video-formats-title",
    )
    .with_system_locale();
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
