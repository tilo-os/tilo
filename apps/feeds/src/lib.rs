//! Feeds — a CRDT-native RSS/Atom/Gemini reader for Tilo.
//!
//! Architecture (mirrors the notes app):
//!   * [`model`]  — pure data + the in-memory [`model::Store`].
//!   * [`doc`]    — Automerge mapping; converging state lives here.
//!   * [`sample`] — bundled offline fixture used only by `--shots` and tests.
//!   * [`fetch`]  — HTTP (`tilo_net` + feed-rs) and gemini (TLS) on the app runtime.
//!   * [`html`] / [`gemtext`] — body → renderable [`model::ContentBlock`]s.
//!   * [`opml`]   — subscription import/export.
//!   * [`app`]    — the root [`app::FeedsApp`]: the window shell around the
//!     sidebar of places, the article list and the reader.
//!
//! `app_root()` is the single integration point: it returns the root widget
//! used by both the live `ui::App` event loop and the `--shots` offscreen
//! renderer.

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints remain denied; shared crates retain the full lint set
// because a fault there can affect the session or every app.
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
mod chrome;
pub mod doc;
pub mod fetch;
pub mod gemtext;
pub mod html;
mod list_pane;
pub mod modal;
pub mod model;
pub mod opml;
mod reader_pane;
pub mod reader_theme;
pub mod resource;
pub mod sample;
pub mod search_provider;
mod sidebar;

pub use app::FeedsApp;
pub use model::{Article, Feed, FeedKind, Filter, Folder, Store};

use ui::widget::Widget;

/// Build and return the app's root widget tree.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(FeedsApp::new())
}

/// The app as every host mounts it: the live root and its actions.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::launch::LaunchSpec::new("so.tilo.feeds", app_root)
        .component_actions(actions::register)
        .as_definition("Feeds")
}

/// Like [`app_root`] but staged for a named `--shots` scenario: interaction
/// states a capture cannot reach through live input.
///
/// * `"empty"` — the live app on a fresh document: the first-run state.
/// * `"sidebar-open"` — the sidebar drawer open over a narrow window.
/// * `"detail"` — a narrow window showing the open article.
/// * `"reader-settings"` — the reading settings over a serif, sepia page.
/// * `"refresh"` — a refresh partway through.
/// * `"long-titles"` — feeds with long titles, eliding in the sidebar.
/// * `"context-menu"` — a feed's menu on its sidebar row.
/// * `"post-import"` — the sidebar right after an OPML import.
/// * `"add-subscription"` — the add-subscription dialog with an address typed.
/// * `"tags"` — the open article's tag editor.
pub fn app_root_for_shot(scenario: &str) -> Box<dyn Widget> {
    use crate::model::{ReadFont, ReadTheme};
    if scenario == "empty" {
        return Box::new(FeedsApp::new());
    }
    let mut app = FeedsApp::new_demo();
    match scenario {
        "sidebar-open" | "detail" | "context-menu" => app.stage(scenario),
        "reader-settings" => {
            app.set_read_font(ReadFont::Serif);
            app.set_read_theme(ReadTheme::Sepia);
            app.stage(scenario);
        }
        "refresh" => app.force_refresh_in_progress(),
        "long-titles" => app.stage_long_sidebar_titles(),
        "post-import" => app.stage_post_import(),
        "add-subscription" => app.stage_add_subscription("https://blog.rust-lang.org/feed.xml"),
        "tags" => app.stage_tag_editor(),
        _ => {}
    }
    Box::new(app)
}

/// The window a scenario is captured at: the narrow states in a narrow
/// window. An explicit `--viewport` wins.
pub fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
    match scenario {
        Some("sidebar-open") => (800, 600),
        Some("detail") => (520, 820),
        _ => (1280, 800),
    }
}
pub use resource::{Feeds, FeedsShell};
