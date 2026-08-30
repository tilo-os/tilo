//! Firewall — standard qOS application entry point.

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

use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};
use ui::widget::Widget;

fn shot_root(_: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    Ok(
        if std::env::var("QOS_FIREWALL_SAMPLE").as_deref() == Ok("1") {
            firewall::app_root_demo()
        } else {
            firewall::app_root()
        },
    )
}

pub fn main() {
    ui::launch::run_or_exit(
        LaunchSpec::new("dev.qos.firewall", firewall::app_root)
            .shot_root(shot_root)
            .configure_app(|app| firewall::actions::register(app.action_registry_mut())),
    )
}
