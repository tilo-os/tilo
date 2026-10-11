//! Fluent copy for Diff & Merge.
//!
//! Every user-facing sentence with a count or a variable lives in the
//! embedded catalog, so plurals follow the active locale and a `--shots`
//! render, a unit test and a live session resolve the same words.

use ui::i18n::{ArgValue, EmbeddedCatalog};

/// Diff & Merge's Fluent catalogs, source locale first.
pub const LOCALES: &[(&str, &str)] = &[("en-US", include_str!("../locales/en-US.ftl"))];

static CATALOG: EmbeddedCatalog = catalog();

const fn catalog() -> EmbeddedCatalog {
    let catalog = EmbeddedCatalog::new("en-US", LOCALES, "empty-title").with_system_locale();
    // Unit tests assert the source copy on any developer or CI machine.
    #[cfg(test)]
    let catalog = catalog.with_pinned_locale("en-US");
    catalog
}

/// The app's catalogs, for the framework's history sentences.
pub fn catalogs() -> &'static EmbeddedCatalog {
    &CATALOG
}

/// The message `key` in the active locale.
pub fn t(key: &str) -> String {
    CATALOG.t(key)
}

/// The message `key` with its named variables.
pub fn t_args(key: &str, args: &[(&str, ArgValue)]) -> String {
    CATALOG.t_args(key, args)
}

/// A count as a message argument.
pub fn count(n: usize) -> ArgValue {
    ArgValue::Number(n as f64)
}

/// Text as a message argument.
pub fn text(value: &str) -> ArgValue {
    ArgValue::Text(value.to_owned())
}
