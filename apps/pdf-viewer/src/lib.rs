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
//! free. Launched with a file path the app opens it; launched bare it shows
//! an empty state. The bundled demonstration document ([`model::sample_document`])
//! is used only by the screenshot harness and the tests.

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
pub mod adopt;
pub mod app;
pub mod chrome;
pub mod doc;
pub mod model;
pub mod render;
pub mod resource;
pub mod search_provider;
pub mod text_layer;
pub mod tiles;
mod web_file;

pub use app::PdfApp;
pub use doc::PdfDoc;
pub use model::{AnnoKind, Annotation, Comment, Document, Signature, SignatureInk, Swatch, Tool};

pub mod source;

use ui::widget::Widget;

/// The no-document state (live launch with no file).
pub fn app_root() -> Box<dyn Widget> {
    Box::new(PdfApp::empty())
}

/// Screenshot-only owner-worker loading state.
pub fn app_root_loading() -> Box<dyn Widget> {
    Box::new(PdfApp::loading_preview())
}

/// Screenshot-only failed-open state with the persistent accessible error.
pub fn app_root_error() -> Box<dyn Widget> {
    Box::new(PdfApp::error_preview())
}

/// Open a real PDF from `path`.
pub fn app_root_open(path: impl AsRef<std::path::Path>) -> Box<dyn Widget> {
    Box::new(PdfApp::open(path))
}

/// Open a real PDF from `path`, keeping imported pages in `content`.
pub fn app_root_open_with_content(
    path: impl AsRef<std::path::Path>,
    content: tilo_content::ContentStore,
) -> Box<dyn Widget> {
    Box::new(PdfApp::open_with_content(path, content))
}

/// The app as every host mounts it: the reader and its actions.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::launch::LaunchSpec::new("so.tilo.pdf-viewer", app_root)
        .component_actions(actions::register)
        .as_definition("PDF Viewer")
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

/// Screenshot-only: the sample document with a settled text selection, so a
/// still frame shows the shared selection HUD (highlight / Copy / Note).
pub fn app_root_selection_hud() -> Box<dyn Widget> {
    let mut app = PdfApp::sample();
    app.arm_shots_selection();
    Box::new(app)
}

/// Screenshot-only: the sample document with the floating page-info bubble
/// pinned open, so a still frame shows the transient page overlay (not a bottom
/// strip).
pub fn app_root_page_bubble() -> Box<dyn Widget> {
    let mut app = PdfApp::sample();
    app.arm_shots_page_bubble();
    Box::new(app)
}

/// Screenshot-only: sample document with the annotation context menu open.
pub fn app_root_context_menu() -> Box<dyn Widget> {
    let mut app = PdfApp::sample();
    app.arm_shots_context_menu();
    Box::new(app)
}

/// Screenshot-only: the sample document with the find bar open (query + live
/// matches) so a still frame shows the non-overlapping find bar.
pub fn app_root_search() -> Box<dyn Widget> {
    let mut app = PdfApp::sample();
    app.arm_shots_search();
    Box::new(app)
}

/// Screenshot-only: the Signatures panel in Draw mode with a drawn signature and
/// the Sign tool armed, so a still frame shows the signature-composer workflow.
pub fn app_root_sign() -> Box<dyn Widget> {
    let mut app = PdfApp::sample();
    app.arm_shots_sign_compose();
    Box::new(app)
}

/// Screenshot-only: open `path` (a real PDF) with a live page-rail reorder drag
/// staged, so a still frame shows the left sidebar mid-reorder.
pub fn app_root_reorder_demo(path: Option<&std::path::Path>) -> Box<dyn Widget> {
    let mut app = match path {
        Some(p) => PdfApp::open(p),
        None => PdfApp::sample(),
    };
    app.arm_shots_rail_drag();
    Box::new(app)
}

/// Screenshot-only: open `path` (a real PDF) and import a generated image as a
/// new page, so a still frame shows the imported image page in the rail + canvas.
pub fn app_root_import_demo(path: Option<&std::path::Path>) -> Box<dyn Widget> {
    let mut app = match path {
        Some(p) => PdfApp::open(p),
        None => PdfApp::sample(),
    };
    app.demo_import_image();
    Box::new(app)
}
pub use resource::{PdfMarkup, PdfViewer, PdfViewerShell, launch_spec, standalone_launch};
