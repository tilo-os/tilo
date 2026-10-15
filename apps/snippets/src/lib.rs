//! Snippets — a text & code snippet manager for Tilo.
//!
//! A searchable, taggable library of reusable snippets. Each snippet has a
//! title, language, tags and a body that may contain `${placeholder}`
//! variables; the detail pane shows a syntax-highlighted preview, and the
//! expand-with-variables flow fills the placeholders before copying to the
//! clipboard. The library is a typed model (see [`model`]) stored in a CRDT
//! document, so it syncs across devices and collaborators converge on one
//! shared set of snippets.
//!
//! The app is [`SnippetsShell`], a framework app over the [`Snippets`]
//! resource: every host mounts it through [`definition`].

// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop, and proving every layout sum cannot
// overflow is a tax a third-party app author would pay too. The lints that
// produce a *crash* stay denied — indexing_slicing, string_slice,
// unwrap_used, expect_used, panic — because a panicking app is a bad app
// whoever wrote it. `shell`, `ui` and everything under `crates/` keep the
// full set: a fault there takes the session, or every app with it.
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

pub use app::SnippetsApp;
pub use model::{Language, Selection, SmartFolder, Snippet, SnippetId, Snippets};
pub use resource::{SnippetsShell, SnippetsView};

/// The app as every host mounts it.
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
