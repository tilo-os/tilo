//! Database client — standard qOS application entry point.

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

#[allow(
    clippy::unnecessary_wraps,
    reason = "signature is fixed by ui::launch::ShotFactory"
)]
fn shot_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    let width = f64::from(args.viewport.unwrap_or(ui::launch::DEFAULT_VIEWPORT).0);
    Ok(if args.has("--empty") {
        db_client::app_root_for_shot_empty(width)
    } else if args.has("--status") {
        db_client::app_root_for_shot_status(width)
    } else {
        db_client::app_root_for_shot(width)
    })
}

pub fn main() {
    ui::launch::run_or_exit(
        LaunchSpec::new("dev.qos.db-client", db_client::app_root)
            .shot_root(shot_root)
            .allowed_flags(&["--empty", "--status"])
            .configure_app(|app| db_client::actions::register(app.action_registry_mut())),
    );
}
