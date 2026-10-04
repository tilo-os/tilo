//! Snippets — a text & code snippet manager for Tilo.
//!
//! A searchable, taggable library of reusable snippets. Each snippet has a
//! title, language, tags and a body that may contain `${placeholder}`
//! variables; the detail pane shows a syntax-highlighted preview, and the
//! expand-with-variables flow fills the placeholders before copying to the
//! clipboard. The library is a CRDT object (see [`doc`]) so it syncs across
//! devices and collaborators converge on one shared set of snippets.
//!
//! The app runs through the framework ([`launch_spec`]): the live window,
//! `--shots` and its scenarios share one root.

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
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod actions;
pub mod app;
mod detail_pane;
pub mod doc;
pub mod draw;
pub mod expand;
pub mod highlight;
mod list_pane;
pub mod model;
pub mod resource;
pub mod search_provider;
pub mod seed;

pub use app::SnippetsApp;
pub use model::{Language, Library, Selection, SmartFolder, Snippet, SnippetId};

use ui::widget::Widget;

/// Build and return the app's root widget tree.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(SnippetsApp::new())
}

/// Snippets' Fluent catalogs, source locale first.
pub const LOCALES: &[(&str, &str)] = &[
    ("en-US", include_str!("../locales/en-US.ftl")),
    ("de-DE", include_str!("../locales/de-DE.ftl")),
];

/// The app as every host mounts it: the live root and its actions.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::launch::LaunchSpec::new("so.tilo.snippets", app_root)
        .history(&framework_history::HISTORY)
        .component_actions(actions::register)
        .component_locales(LOCALES)
        .as_definition("Snippets")
}

/// The catalogs, loaded on first use on each thread: a host's manager gets
/// them added, a thread without one starts in the person's locale.
#[cfg(not(test))]
static CATALOG: ui::i18n::EmbeddedCatalog =
    ui::i18n::EmbeddedCatalog::new("en-US", LOCALES, "snippets-library").with_system_locale();

/// Tests read the source copy on every machine.
#[cfg(test)]
static CATALOG: ui::i18n::EmbeddedCatalog =
    ui::i18n::EmbeddedCatalog::new("en-US", LOCALES, "snippets-library")
        .with_pinned_locale("en-US");

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

pub use resource::{Snippets, SnippetsShell, launch_spec};

pub mod framework_history;

#[cfg(test)]
mod framework_history_tests;
