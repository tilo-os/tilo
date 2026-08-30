//! Snippets — standard qOS application entry point.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic"
)]

use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};
use ui::widget::Widget;

fn shot_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    Ok(snippets::app_root_for_shot(
        args.scenario.as_deref().unwrap_or_default(),
    ))
}

pub fn main() {
    ui::launch::run_or_exit(
        LaunchSpec::new("dev.qos.snippets", snippets::app_root)
            .shot_root(shot_root)
            .scenarios(&["narrow", "detail", "empty"])
            .configure_app(|app| snippets::actions::register(app.action_registry_mut())),
    )
}
