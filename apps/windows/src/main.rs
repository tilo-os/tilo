#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local rendering geometry"
)]
#[cfg(feature = "thin")]
extern crate runtime_dylib;
use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};
use ui::widget::Widget;
#[allow(
    clippy::unnecessary_wraps,
    reason = "ShotFactory requires this exact result type"
)]
fn shot_root(_args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    Ok(windows::app_root_offline())
}
#[allow(
    clippy::unnecessary_wraps,
    reason = "signature is fixed by ui::launch's argument-aware root factory"
)]
fn live_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    Ok(args
        .positionals()
        .first()
        .map_or_else(windows::app_root, |path| {
            windows::app_root_open(std::path::Path::new(path))
        }))
}
pub fn main() {
    ui::launch::run_or_exit(
        &LaunchSpec::new_with_args("dev.qos.windows", live_root)
            .max_positionals(1)
            .shot_root(shot_root),
    );
}
