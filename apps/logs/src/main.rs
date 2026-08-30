//! Logs — standard qOS application entry point.

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
        reason = "a test that unwraps is stating an invariant"
    )
)]

use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};
use ui::widget::Widget;

fn shot_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    let entries = if std::env::var("QOS_LOGS_EMPTY").is_ok() {
        Vec::new()
    } else {
        logs::journal::sample_entries()
    };
    let mut app = logs::LogsApp::new(entries, false);
    if args.has("--detail") {
        app.shots_show_detail();
    }
    Ok(Box::new(app))
}

pub fn main() {
    ui::launch::run_or_exit(
        LaunchSpec::new("dev.qos.logs", logs::app_root)
            .shot_root(shot_root)
            .allowed_flags(&["--detail"]),
    )
}
