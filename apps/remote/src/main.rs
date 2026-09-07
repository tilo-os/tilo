//! Remote connections — standard qOS application entry point.

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
    let scenario = args.scenario.as_deref().unwrap_or_default();
    Ok(
        if std::env::var("QOS_REMOTE_SAMPLE").as_deref() == Ok("1") {
            remote::app_root_for_shot_demo(scenario)
        } else {
            remote::app_root_for_shot(scenario)
        },
    )
}

#[cfg(feature = "thin")]
extern crate runtime_dylib;

pub fn main() {
    ui::launch::run_or_exit(
        &LaunchSpec::new("dev.qos.remote", remote::app_root)
            .shot_root(shot_root)
            .scenarios(&[
                "narrow",
                "narrow-open",
                "form",
                "form-qos",
                "session-ssh",
                "session-vnc",
                "session-rdp",
                "session-qos",
                "session-connecting",
                "session-failed",
                "confirm-connect",
                "confirm-delete",
                "confirm-ssh-trust",
            ])
            .configure_app(|app| remote::actions::register(app.action_registry_mut())),
    );
}
