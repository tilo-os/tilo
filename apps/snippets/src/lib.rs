//! Snippets — a text & code snippet manager for Tilo.
//!
//! A searchable, taggable library of reusable snippets. Each snippet has a
//! title, language, tags and a body that may contain `${placeholder}`
//! variables; the detail pane shows a syntax-highlighted preview, and the
//! expand-with-variables flow fills the placeholders before copying to the
//! clipboard. The library is a CRDT object (see [`doc`]) so it syncs across
//! devices and collaborators converge on one shared set of snippets.
//!
//! The single integration point is [`app_root`], used by both the live
//! event loop and the `--shots` offscreen renderer in `main.rs`.

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
pub mod doc;
pub mod draw;
pub mod expand;
pub mod highlight;
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

/// Like [`app_root`] but with a named interaction scenario staged for a
/// single headless screenshot (the live `--shots` renderer paints one frame
/// and can't reach toggled states through input). Recognised scenarios:
///
/// * `"narrow"` — pair with a sub-breakpoint `--viewport` to show the
///   primary sidebar as an open overlay flap over the content + scrim.
/// * `"detail"` — pair with a very-narrow `--viewport` to show the
///   single-pane push stack on the detail with its Back affordance.
///
/// An empty / unknown name yields the same frame as [`app_root`].
pub fn app_root_for_shot(scenario: &str) -> Box<dyn Widget> {
    // The `empty` scenario captures the honest live zero-state (a fresh
    // install has no snippets); every other scenario stages the seeded demo
    // library so the capture shows the manager doing real work.
    let mut app = if scenario == "empty" {
        SnippetsApp::new()
    } else {
        SnippetsApp::new_demo()
    };
    app.seed_shot_scenario(scenario);
    Box::new(app)
}

#[cfg(test)]
mod framework_history_tests;

pub mod framework_history;
pub use resource::{Snippets, SnippetsShell, launch_spec};
