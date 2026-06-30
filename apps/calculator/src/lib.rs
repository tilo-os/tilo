//! Calculator — qOS native app (CRDT-native, built on the `ui` toolkit).
//!
//! An everyday scientific calculator that also does proper 2D and 3D
//! graphing. State (expression, history, plotted functions, view
//! transforms) lives in an Automerge document so undo/redo and live
//! collaboration come for free; the panels are custom Vello-drawn widgets.
//!
//! `app_root()` is the single integration point used by both the live
//! event loop and the `--shots` offscreen renderer.

pub mod app;
pub mod doc;
pub mod draw;
pub mod engine;
pub mod graph2d;
pub mod graph3d;
pub mod keypad;
pub mod mathfield;
pub mod model;

pub use app::CalculatorApp;

use ui::widget::Widget;

/// Build and return the app's root widget tree.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(CalculatorApp::new())
}
