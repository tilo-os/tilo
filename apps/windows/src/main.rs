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
pub fn main() {
    ui::launch::run_or_exit(
        &LaunchSpec::new("dev.qos.windows", windows::app_root).shot_root(shot_root),
    );
}
