//! Terminal's Fluent copy, resolved by the shared manager.

use ui::i18n::EmbeddedCatalog;

static CATALOG: EmbeddedCatalog = catalog();

const fn catalog() -> EmbeddedCatalog {
    let catalog = EmbeddedCatalog::new(
        "en-US",
        &[
            ("en-US", include_str!("../locales/en-US.ftl")),
            ("de-DE", include_str!("../locales/de-DE.ftl")),
        ],
        "terminal-find-scrollback",
    )
    .with_system_locale();
    // Unit tests assert the source copy on any developer or CI machine.
    #[cfg(test)]
    let catalog = catalog.with_pinned_locale("en-US");
    catalog
}

/// The message `key` in the active locale.
pub(crate) fn t(key: &str) -> String {
    CATALOG.t(key)
}
