//! API tester — standard Tilo application entry point.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
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

// `ShotFactory` (in `ui::launch`) is `fn(&LaunchArgs) -> Result<Box<dyn
// Widget>, LaunchError>` — a fixed callback type shared by every app's
// `--shots` entry point, so this can't drop the `Result` even though this
// particular app never fails to build its shot root.
#[allow(
    clippy::unnecessary_wraps,
    reason = "must match ui::launch's ShotFactory = fn(&LaunchArgs) -> Result<..>"
)]
fn shot_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    let mut app = api_tester::ApiTesterApp::new_demo();
    if let Some(scenario) = args.scenario.as_deref() {
        let _ = app.stage(scenario);
    }
    Ok(Box::new(app))
}

#[cfg(feature = "thin")]
extern crate runtime_dylib;

fn wants_standalone(args: impl Iterator<Item = String>) -> bool {
    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => {
                let _ = args.next();
            }
            "--shots" | "--responsive-matrix" | "--native-audit" | "--headless" => return true,
            flag if flag.starts_with('-') => {}
            _ => {}
        }
    }
    false
}

pub fn main() {
    if !wants_standalone(std::env::args().skip(1)) {
        ui::framework::run::<api_tester::ApiTesterShell>();
        return;
    }
    ui::launch::run_or_exit(
        &LaunchSpec::new("so.tilo.api-tester", api_tester::app_root)
            .shot_root(shot_root)
            .scenarios(api_tester::ApiTesterApp::SCENARIOS)
            .configure_app(|app| api_tester::actions::register(app.action_registry_mut())),
    );
}
