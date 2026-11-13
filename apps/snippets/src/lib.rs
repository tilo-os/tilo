//! Snippets — a library of text and code snippets, with folders, tags and fill-in
//! variables.

// Apps run isolated, so app-local arithmetic checks are relaxed; a fault in a shared crate reaches the session, so its lints stay.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "app-local arithmetic; see the note above"
)]
pub mod app;
mod detail_pane;
pub mod draw;
pub mod expand;
pub mod highlight;
mod list_pane;
pub mod model;
pub mod resource;
pub mod seed;

pub use model::{Language, Selection, SmartFolder, Snippet, SnippetId, Snippets};
pub use resource::{SnippetsShell, SnippetsView};

pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(SnippetsShell)
}

ui::embedded_catalog!(pub(crate) static CATALOG, ["en-US", "de-DE"]);

/// A message of the app's catalogs in the person's language.
pub(crate) fn tr(key: &str) -> String {
    CATALOG.t(key)
}

/// A message with named text arguments.
pub(crate) fn tr_with(key: &str, args: &[(&str, &str)]) -> String {
    let args: Vec<(&str, ui::i18n::ArgValue)> = args
        .iter()
        .map(|(name, value)| (*name, ui::i18n::ArgValue::Text((*value).to_string())))
        .collect();
    CATALOG.t_args(key, &args)
}

/// A message with a plural count.
pub(crate) fn tr_count(key: &str, count: usize) -> String {
    CATALOG.t_args(key, &[("count", ui::i18n::ArgValue::Number(count as f64))])
}
