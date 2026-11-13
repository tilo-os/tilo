//! The crate's copy, resolved in the language the window reads.

ui_core::embedded_catalog!(pub(crate) static CATALOG, ["en-US", "de-DE"]);

/// Resolve a key that interpolates variables.
pub(crate) fn t_args(key: &str, args: &[(&str, ui_core::i18n::ArgValue)]) -> String {
    CATALOG.t_args(key, args)
}
