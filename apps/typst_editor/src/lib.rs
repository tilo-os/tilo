//! Typst — qOS native app (CRDT-native, built on the `ui` toolkit).
//!
//! A split-view Typst editor: a monospace, syntax-highlighted source pane on
//! the left, a live rendered preview of the compiled document on the right,
//! a clickable diagnostics ("Problems") panel, page thumbnails, zoom, and a
//! background compile worker. The document source lives in an Automerge `Text`
//! object so editing is collaborative and undoable for free.
//!
//! `app_root()` is the single integration point used by both the live event
//! loop and the `--shots` offscreen renderer.

pub mod app;
pub mod compile;
pub mod diagnostics;
pub mod editor;
pub mod preview;
pub mod samples;
pub mod worker;

use ui::widget::Widget;

/// Build the live app root: it reads the user's real document library from
/// disk and opens (or seeds) an untitled starter document.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(app::TypstApp::new())
}

/// Build a hermetic root seeded with the bundled demo documents. Used by the
/// offscreen `--shots` renderer and tests so captures are deterministic and do
/// not depend on whatever `.typ` files happen to be on disk.
pub fn demo_root() -> Box<dyn Widget> {
    Box::new(app::TypstApp::with_demo_documents())
}

/// Like [`demo_root`] but pre-configured into a named state so `--shots` can
/// capture views that would otherwise need interaction (compact preview, the
/// overflow menu, the Problems panel). Screenshot-only.
pub fn demo_root_scenario(scenario: &str) -> Box<dyn Widget> {
    let mut app = app::TypstApp::with_demo_documents();
    app.apply_demo_scenario(scenario);
    Box::new(app)
}
