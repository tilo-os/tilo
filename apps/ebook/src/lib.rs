//! Books — qOS native EPUB reader (CRDT-native, built on the `ui` toolkit).
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

pub mod actions;
pub mod app;
pub mod content;
pub mod doc;
pub mod epub;
pub mod model;
pub mod paginate;
pub mod paint;
pub mod portal;
pub mod reader_theme;
pub mod search_provider;
pub mod xhtml;

pub use app::BooksApp;

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
