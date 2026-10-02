//! Books — Tilo native EPUB reader (CRDT-native, built on the `ui` toolkit).
//!
//! Layered so each piece is testable in isolation: [`model`] (pure data +
//! the shared annotation model reused by the PDF reader/word processor),
//! [`doc`] (the Automerge document layer), [`xhtml`]/[`epub`]/[`content`]
//! (EPUB parsing + library), [`paginate`] (text layout), [`reader_theme`]/
//! [`paint`] (palettes + drawing), and [`app`] (the retained-mode
//! [`Widget`](ui::widget::Widget) composing the shelf and reader).
//!
//! [`app_root`] is the one integration point used by both the live event
//! loop and the `--shots` offscreen renderer.

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
        clippy::float_cmp,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod actions;
pub mod app;
pub mod chrome;
mod comment_overlay;
pub mod component;
pub mod content;
pub mod doc;
pub mod epub;
mod input_controls;
mod local_library;
pub mod model;
pub mod paginate;
pub mod paint;
mod reader_bubble;
pub mod reader_theme;
mod reading_settings;
pub mod resource;
pub mod search_provider;
mod shelf_controls;
#[cfg(test)]
mod test_epub;
mod web_file;
pub mod xhtml;

pub use app::BooksApp;
pub use component::definition;

/// Build and return the app's root widget tree.
pub fn app_root() -> Box<dyn ui::widget::Widget> {
    Box::new(BooksApp::new())
}

/// Build the reader opened directly on the `.epub` at `path` — the entry point
/// the OS uses when the reader is the handler for a book file.
pub fn app_root_for_path(path: &std::path::Path) -> Box<dyn ui::widget::Widget> {
    Box::new(BooksApp::open_path(path))
}

/// Like [`app_root`] but staged with a named scenario for the headless
/// `--shots` renderer. See [`BooksApp::seed_shot_scenario`] for the
/// recognised names; an empty/unknown name yields the default frame.
pub fn app_root_for_shot(scenario: &str) -> Box<dyn ui::widget::Widget> {
    let mut app = BooksApp::new();
    app.seed_shot_scenario(scenario);
    Box::new(app)
}
pub use resource::launch_spec;
pub use resource::{Ebook, EbookShell, EbookState, standalone_launch};
