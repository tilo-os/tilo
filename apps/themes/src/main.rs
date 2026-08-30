//! Themes — standard qOS application entry point.

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
    let width = args.viewport.unwrap_or(ui::launch::DEFAULT_VIEWPORT).0;
    Ok(themes::app_root_for_shot(width))
}

pub fn main() {
    ui::launch::run_or_exit(
        LaunchSpec::new("dev.qos.themes", themes::app_root)
            .shot_root(shot_root)
            .configure_app(|app| themes::actions::register(app.action_registry_mut())),
    )
}
