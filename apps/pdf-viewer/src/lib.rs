//! The PDF reader, built on the `ui` toolkit.
//!
//! A collaborative reader: pages with zoom and a thumbnail rail, a contents,
//! comments and signatures sidebar, search, and an annotation and signing layer whose
//! markup lives in the shared document, so undo and collaboration come for free.
//!
//! ## Rendering backend
//!
//! A real PDF is loaded through [`source`], which rasterises pages and
//! extracts their text layer with Tilo's sandboxed PDF engine. The durable
//! markup lives in the shared Automerge document, so undo/redo and live collaboration come for
//! free. Each document's markup is a record of its own: launched with a
//! file the app opens that file's record; launched bare it reopens the
//! document its record marks up, or shows an empty state. The screenshot
//! scenarios (`scenarios`, behind the `samples` feature) open a real sample
//! PDF the same way.

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
pub mod dates;
pub mod markup;
pub mod model;
pub use tilo_page_raster::images as page_images;
pub mod render;
pub mod resource;
pub mod search;
pub mod signature;
pub mod text_layer;
pub mod tiles;

pub use app::PdfApp;
pub use model::{
    AnnoKind, Annotation, Comment, Document, HighlightColor, Signature, SignatureInk, Tool,
};

#[cfg(feature = "samples")]
pub mod scenarios;
pub mod source;

pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(PdfViewerShell)
}

pub use markup::PdfMarkup;
pub use resource::PdfViewerShell;

ui::embedded_catalog!(pub(crate) fn, ["de-DE", "en-US"]);

/// German is one of the languages the app ships, and it reads in German: the
/// copy comes from `locales/de-DE.ftl`, never from the English source
/// catalogue beside it.
#[cfg(test)]
mod german_window {
    #[test]
    fn a_german_window_reads_the_catalogue() {
        let catalog = &super::CATALOG;
        assert!(
            catalog.languages().contains(&"de-DE"),
            "the app ships German among {:?}",
            catalog.languages()
        );
        catalog.set_locale("de-DE");
        assert_eq!(catalog.t("pdf-title"), "PDF-Viewer");
        assert_eq!(catalog.t("pdf-tab-comments"), "Kommentare");
        catalog.set_locale("en-US");
        assert_eq!(catalog.t("pdf-title"), "PDF Viewer");
    }
}
