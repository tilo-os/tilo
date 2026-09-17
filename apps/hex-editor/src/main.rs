//! Hex editor — standard Tilo application entry point.

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

use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};
use ui::widget::Widget;

#[allow(
    clippy::unnecessary_wraps,
    reason = "must match ui::launch::ShotFactory's fn(&LaunchArgs) -> Result<..> signature"
)]
fn shot_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    let mut view = if args.scenario.as_deref() == Some("empty") {
        hex_editor::HexView::empty()
    } else {
        hex_editor::app::sample_view()
    };
    match args.scenario.as_deref() {
        Some("search") => view.arm_search_for_shots(),
        Some("error") => view.arm_error_for_shots(),
        Some("empty") => {}
        _ => view.arm_status_bubble_for_shots(),
    }
    Ok(Box::new(view))
}

#[cfg(feature = "thin")]
extern crate runtime_dylib;

pub fn main() {
    ui::launch::run_or_exit(
        &LaunchSpec::new("dev.tilo.hex-editor", hex_editor::app_root)
            .shot_root(shot_root)
            .scenarios(&["empty", "search", "error"])
            .configure_app(|app| hex_editor::actions::register(app.action_registry_mut())),
    );
}
