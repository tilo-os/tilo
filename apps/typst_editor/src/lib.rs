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
pub mod highlight;
pub mod preview;
pub mod samples;
pub mod worker;

use ui::widget::Widget;

/// Build and return the app's root widget tree.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(app::TypstApp::new())
}
