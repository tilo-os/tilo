//! API tester — standard qOS application entry point.

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
    let mut app = api_tester::ApiTesterApp::new_demo();
    if args.has("--rail-open") {
        app.snap_rail(true);
    }
    Ok(Box::new(app))
}

pub fn main() {
    ui::launch::run_or_exit(
        LaunchSpec::new("dev.qos.api-tester", api_tester::app_root)
            .shot_root(shot_root)
            .allowed_flags(&["--rail-open"])
            .configure_app(|app| api_tester::actions::register(app.action_registry_mut())),
    )
}
