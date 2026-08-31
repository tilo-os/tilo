//! Containers — standard qOS application entry point.

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

// `shot_root` must keep this exact `Result` return type to satisfy
// `ui::launch::ShotFactory` (`fn(&LaunchArgs) -> Result<Box<dyn Widget>,
// LaunchError>`), even though this app's shot scenarios never fail.
#[allow(
    clippy::unnecessary_wraps,
    reason = "signature is fixed by ui::launch::ShotFactory"
)]
fn shot_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    Ok(match args.scenario.as_deref() {
        Some(scenario) => containers::app_root_shot(scenario),
        None => containers::app_root_offline(),
    })
}

pub fn main() {
    ui::launch::run_or_exit(
        &LaunchSpec::new("dev.qos.containers", containers::app_root)
            .shot_root(shot_root)
            .scenarios(&["confirm", "prompt"]),
    );
}
