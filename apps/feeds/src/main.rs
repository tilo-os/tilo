//! Feeds — standard qOS application entry point.

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
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};
use ui::widget::Widget;

fn shot_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    Ok(feeds::app_root_for_shot(
        args.scenario.as_deref().unwrap_or_default(),
    ))
}

pub fn main() {
    ui::launch::run_or_exit(
        LaunchSpec::new("dev.qos.feeds", feeds::app_root)
            .shot_root(shot_root)
            .scenarios(&[
                "sidebar-open",
                "detail",
                "empty",
                "reader-settings",
                "refresh",
                "long-titles",
                "context-menu",
                "post-import",
                "add-subscription",
            ])
            .configure_app(|app| feeds::actions::register(app.action_registry_mut())),
    )
}
