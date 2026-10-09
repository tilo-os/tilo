//! PDF — Tilo native app (CRDT-native, built on the `ui` toolkit).
//!
//! A collaborative PDF reader: scrollable multi-page view with zoom and a
//! thumbnail rail, a contents/comments/signatures sidebar, incremental
//! search, and an annotation layer (highlight, freehand ink, sticky notes)
//! plus a signing flow — all durable markup lives in the shared Automerge
//! document so undo/redo and live collaboration come for free.
//!
//! ## Rendering backend
//!
//! A real PDF is loaded through [`source`], which rasterises pages and
//! extracts their text layer with Tilo's sandboxed PDF engine. The durable
//! markup lives in the shared Automerge document, so undo/redo and live collaboration come for
//! free. Each document's markup is a record of its own: launched with a
//! file the app opens that file's record; launched bare it reopens the
//! document its record marks up, or shows an empty state. The bundled
//! demonstration document ([`model::sample_document`]) is used only by the
//! screenshot scenarios ([`scenarios`]) and the tests.

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
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod actions;
pub mod adopt;
pub mod app;
pub mod markup;
pub mod model;
pub mod render;
pub mod resource;
pub mod search;
pub mod signature;
pub mod text_layer;
pub mod tiles;

pub use app::PdfApp;
pub use model::{AnnoKind, Annotation, Comment, Document, Signature, SignatureInk, Swatch, Tool};

pub mod scenarios;
pub mod source;

/// The app as every host mounts it: the reader over its markup resource.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::framework::definition::<PdfViewerShell>()
}

pub use markup::PdfMarkup;
pub use resource::PdfViewerShell;
