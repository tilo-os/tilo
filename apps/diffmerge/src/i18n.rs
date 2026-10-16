//! Fluent copy for Diff & Merge.
//!
//! Every user-facing sentence with a count or a variable lives in the
//! embedded catalog, so plurals follow the active locale and a `--shots`
//! render, a unit test and a live session resolve the same words.

use ui::i18n::{ArgValue, EmbeddedCatalog};

ui::embedded_catalog!(pub fn, ["en-US"]);

/// The app's catalogs, for the framework's history sentences.
pub fn catalogs() -> &'static EmbeddedCatalog {
    &CATALOG
}

/// A count as a message argument.
pub fn count(n: usize) -> ArgValue {
    ArgValue::Number(n as f64)
}

/// Text as a message argument.
pub fn text(value: &str) -> ArgValue {
    ArgValue::Text(value.to_owned())
}
