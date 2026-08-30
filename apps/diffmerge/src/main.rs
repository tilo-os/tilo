//! Diff/merge — standard qOS application entry point.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic"
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
    Ok(if args.scenario.as_deref() == Some("status") {
        diffmerge::app_root_status()
    } else {
        diffmerge::app_root_demo()
    })
}

pub fn main() {
    ui::launch::run_or_exit(
        LaunchSpec::new("dev.qos.diffmerge", diffmerge::app_root)
            .shot_root(shot_root)
            .scenarios(&["status"])
            .configure_app(|app| diffmerge::actions::register(app.action_registry_mut())),
    )
}
