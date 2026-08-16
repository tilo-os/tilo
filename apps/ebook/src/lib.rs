//! Books — qOS native EPUB reader (CRDT-native, built on the `ui` toolkit).
//!
//! The crate is layered so each piece is testable in isolation:
//!
//! - [`model`] — pure data: parsed book content + the **shared annotation
//!   model** (bookmarks / highlights / notes) the PDF reader and word
//!   processor are meant to reuse.
//! - [`doc`] — the Automerge document layer. Reading settings, per-book
//!   progress, and every annotation live in the CRDT so undo/redo and live
//!   collaboration work for free.
//! - [`xhtml`] / [`epub`] / [`content`] — a real EPUB container reader plus the
//!   bundled library.
//! - [`paginate`] — reflowable, measurement-bound text layout + pagination.
//! - [`reader_theme`] / [`paint`] — page palettes and vector drawing helpers.
//! - [`app`] — the single retained-mode [`Widget`](ui::widget::Widget) that
//!   composes the library shelf and the paginated reader.
//!
//! [`app_root`] is the one integration point used by both the live event loop
//! and the `--shots` offscreen renderer.

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
/// `--shots` renderer (which captures one settled frame and can't drive live
/// input). See [`BooksApp::seed_shot_scenario`]. An empty/unknown name yields
/// the default frame.
///
/// Recognised scenarios: `narrow` — the contents rail summoned as an overlay
/// flap over the page behind a scrim (the narrow-window layout).
pub fn app_root_for_shot(scenario: &str) -> Box<dyn ui::widget::Widget> {
    let mut app = BooksApp::new();
    app.seed_shot_scenario(scenario);
    Box::new(app)
}
