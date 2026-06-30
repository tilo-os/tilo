//! Snippets — a text & code snippet manager for qOS.
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

pub mod app;
pub mod doc;
pub mod draw;
pub mod expand;
pub mod highlight;
pub mod model;
pub mod seed;

pub use app::SnippetsApp;
pub use model::{Language, Library, Selection, Snippet, SnippetId, SmartFolder};

use ui::widget::Widget;

/// Build and return the app's root widget tree.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(SnippetsApp::new())
}
