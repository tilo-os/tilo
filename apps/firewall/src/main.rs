//! Firewall — standard Tilo application entry point.

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints remain denied; shared crates retain the full lint set
// because a fault there can affect the session or every app.
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
    reason = "must match the ShotFactory fn-pointer type, which returns Result"
)]
fn shot_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    // Captures are always hermetic and use only the bundled example ruleset.
    // Live startup remains the only path that interrogates the host firewall.
    let mut app = firewall::FirewallApp::new_demo();
    if let Some(scenario) = args.scenario.as_deref() {
        app.pose_fixture(scenario);
    }
    Ok(Box::new(app))
}

#[cfg(feature = "thin")]
extern crate runtime_dylib;

pub fn main() {
    ui::launch::run_or_exit(
        &LaunchSpec::new("so.tilo.firewall", firewall::app_root)
            .shot_root(shot_root)
            .scenarios(&[
                "empty",
                "error",
                "confirmation",
                "active-change",
                "service-error",
                "services",
            ])
            .configure_app(|app| firewall::actions::register(app.action_registry_mut())),
    );
}
