//! A local video player for qOS, built on the shared [`media`] streaming layer
//! and the reusable [`ui::widget::VideoPlayer`] widget.
//!
//! The binary is a thin entry point ([`crate`]'s `main`); all logic lives here.
//! [`PlayerApp`] is the root widget; [`app_root`] / [`app_root_for_shot`] build
//! it for the live event loop and the headless screenshot path respectively.

pub mod app;
pub mod collab;
pub mod portal;

pub use app::{PlayerApp, source_for_path};

use std::path::PathBuf;
use ui::widget::Widget;

/// The live app root: a player with collaborative playback attached (a peer on
/// the same workspace can join and follow). Opens `open` immediately when given.
pub fn app_root(open: Option<PathBuf>) -> Box<dyn Widget> {
    let app = match open {
        Some(path) => PlayerApp::new().with_open_path(path),
        None => PlayerApp::new(),
    };
    Box::new(app.with_collab())
}

/// The headless `--shots` root: a player staged with a frame + revealed
/// controls, no collab socket. Opens `open` when given, else a demo clip.
/// `scenario` selects a demo overlay (see [`PlayerApp::for_shot`]).
pub fn app_root_for_shot(open: Option<PathBuf>, scenario: Option<&str>) -> Box<dyn Widget> {
    Box::new(PlayerApp::for_shot(open, scenario))
}
