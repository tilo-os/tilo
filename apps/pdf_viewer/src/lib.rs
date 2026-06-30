//! PDF — qOS native app (CRDT-native, built on the `ui` toolkit).
//!
//! A collaborative PDF reader: scrollable multi-page view with zoom and a
//! thumbnail rail, a contents/comments/signatures sidebar, incremental
//! search, and an annotation layer (highlight, freehand ink, sticky notes)
//! plus a signing flow — all durable markup lives in the shared Automerge
//! document so undo/redo and live collaboration come for free.
//!
//! `app_root()` is the single integration point used by both the live
//! event loop and the `--shots` offscreen renderer.
//!
//! ## Rendering backend
//!
//! Real PDF rasterisation (pdfium / mupdf) requires a native library that
//! is not guaranteed present in the build sandbox, so this slice ships a
//! pure-Rust page model ([`model::Document`]): each page is a list of
//! typographic blocks laid out with the toolkit's `cosmic-text` engine
//! into honest, selectable, searchable text on a US-Letter card. Geometry
//! is expressed in PDF points so a real raster backend can be slotted in
//! behind the same [`model::Page`] coordinate space later — see
//! `model.rs` for the followup note.

pub mod app;
pub mod doc;
pub mod model;
pub mod render;

pub use app::PdfApp;
pub use doc::PdfDoc;
pub use model::{
    AnnoKind, Annotation, Comment, Document, Signature, Swatch, Tool,
};

use ui::widget::Widget;

/// Build and return the app's root widget tree.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(PdfApp::new())
}
