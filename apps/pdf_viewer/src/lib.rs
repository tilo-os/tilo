//! PDF — qOS native app (CRDT-native, built on the `ui` toolkit).
//!
//! A collaborative PDF reader: scrollable multi-page view with zoom and a
//! thumbnail rail, a contents/comments/signatures sidebar, incremental
//! search, and an annotation layer (highlight, freehand ink, sticky notes)
//! plus a signing flow — all durable markup lives in the shared Automerge
//! document so undo/redo and live collaboration come for free.
//!
//! ## Rendering backend
//!
//! A real PDF is loaded through [`source`], which rasterises pages with a
//! dynamically-loaded `pdfium` when one is available and otherwise falls back
//! to pure-Rust text extraction. Either way the durable markup lives in the
//! shared Automerge document, so undo/redo and live collaboration come for
//! free. Launched with a file path the app opens it; launched bare it shows
//! an empty state. The bundled demonstration document ([`model::sample_document`])
//! is used only by the screenshot harness and the tests.

pub mod app;
pub mod doc;
pub mod model;
pub mod portal;
pub mod render;
pub mod text_layer;

pub use app::PdfApp;
pub use doc::PdfDoc;
pub use model::{
    AnnoKind, Annotation, Comment, Document, Signature, Swatch, Tool,
};

pub mod source;

use ui::widget::Widget;

/// The no-document state (live launch with no file).
pub fn app_root() -> Box<dyn Widget> {
    Box::new(PdfApp::empty())
}

/// Open a real PDF from `path`.
pub fn app_root_open(path: impl AsRef<std::path::Path>) -> Box<dyn Widget> {
    Box::new(PdfApp::open(path))
}

/// The bundled demonstration document with example markup — screenshots and
/// tests only.
pub fn app_root_sample() -> Box<dyn Widget> {
    Box::new(PdfApp::sample())
}

/// Screenshot-only: the sample document with the inline comment editor open,
/// so a still frame can show the caret + focus ring.
pub fn app_root_sample_editing() -> Box<dyn Widget> {
    Box::new(PdfApp::sample_editing())
}
