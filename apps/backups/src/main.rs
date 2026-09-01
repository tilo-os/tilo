//! Backups — standard qOS application entry point.

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

// `ShotFactory` (in `ui::launch`) is `fn(&LaunchArgs) -> Result<Box<dyn
// Widget>, LaunchError>` — a fixed callback type shared by every app's
// `--shots` entry point, so this can't drop the `Result` even though this
// particular app never fails to build its shot root.
#[allow(
    clippy::unnecessary_wraps,
    reason = "must match ui::launch's ShotFactory = fn(&LaunchArgs) -> Result<..>"
)]
fn shot_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    if args.has("--empty") {
        Ok(backups::app_root())
    } else if args.scenario.as_deref() == Some("status") {
        let mut app = backups::BackupsApp::new_demo();
        app.arm_status_bubble_for_shots();
        Ok(Box::new(app))
    } else {
        Ok(backups::app_root_demo())
    }
}

#[cfg(feature = "thin")]
extern crate runtime_dylib;

pub fn main() {
    ui::launch::run_or_exit(
        &LaunchSpec::new("dev.qos.backups", backups::app_root)
            .shot_root(shot_root)
            .scenarios(&["status"])
            .allowed_flags(&["--empty"])
            .configure_app(|app| backups::actions::register(app.action_registry_mut())),
    );
}
