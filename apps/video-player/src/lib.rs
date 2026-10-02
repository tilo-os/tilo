//! A local video player for Tilo, built on the shared [`media`] streaming layer
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
pub mod app;
pub mod collab;
pub mod framework_history;
#[cfg(test)]
mod framework_history_tests;
mod kept;
mod preferences;
pub mod resource;
pub mod search;
pub mod shared;
pub use app::{PlayerApp, source_for_path};

use std::path::PathBuf;
use ui::widget::Widget;

/// The live app root: a player with collaborative playback attached (a peer on
/// the same workspace can join and follow). Opens `open` immediately when given.
pub fn app_root(open: Option<PathBuf>) -> Box<dyn Widget> {
    app_root_source(open.map(|path| source_for_path(&path)), None)
}

/// Live root for either a file or an exact-target remote-stream handoff.
pub fn app_root_source(open: Option<media::Source>, title: Option<String>) -> Box<dyn Widget> {
    let mut app = match open {
        Some(source) => PlayerApp::new()
            .with_open_source(source, title.unwrap_or_else(|| "Handed-off media".into())),
        None => PlayerApp::new(),
    };
    if let Some((position_ms, resume_playing)) = media_handoff_transport_env() {
        app = app.with_handoff_transport(position_ms, resume_playing);
    }
    Box::new(app.with_collab())
}

fn media_handoff_transport_env() -> Option<(u64, bool)> {
    let position = std::env::var("TILO_MEDIA_HANDOFF_POSITION_MS")
        .ok()?
        .parse()
        .ok()?;
    let resume = std::env::var("TILO_MEDIA_HANDOFF_RESUME_PLAYING")
        .ok()
        .is_some_and(|value| value == "1");
    Some((position, resume))
}

/// The app as every host mounts it: the live player and its actions.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::launch::LaunchSpec::new("so.tilo.video-player", || app_root(None))
        .component_actions(actions::register)
        .as_definition("Video Player")
}

/// The headless `--shots` root: a player staged with a frame + revealed
/// controls, no collab socket. Opens `open` when given, else a demo clip.
/// `scenario` selects a demo overlay (see [`PlayerApp::for_shot`]).
pub fn app_root_for_shot(open: Option<PathBuf>, scenario: Option<&str>) -> Box<dyn Widget> {
    Box::new(PlayerApp::for_shot(open, scenario))
}
pub use resource::{VideoPlayer, VideoPlayerShell, launch_spec as framework_launch_spec};
