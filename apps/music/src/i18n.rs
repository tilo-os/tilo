//! Music's Fluent copy, resolved by the shared manager.

use ui::i18n::{ArgValue, EmbeddedCatalog};

static CATALOG: EmbeddedCatalog = catalog();

const fn catalog() -> EmbeddedCatalog {
    let catalog = EmbeddedCatalog::new(
        "en-US",
        &[("en-US", include_str!("../locales/en-US.ftl"))],
        "music-open-audio-file",
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

/// The message `key` with its named variables.
pub(crate) fn t_args(key: &str, args: &[(&str, ArgValue)]) -> String {
    CATALOG.t_args(key, args)
}
