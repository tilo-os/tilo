//! A local video player for qOS, built on the shared [`media`] streaming layer
//! and the reusable [`ui::widget::VideoPlayer`] widget.
//!
//! The binary is a thin entry point ([`crate`]'s `main`); all logic lives here.
//! [`PlayerApp`] is the root widget; [`app_root`] / [`app_root_for_shot`] build
//! it for the live event loop and the headless screenshot path respectively.

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
