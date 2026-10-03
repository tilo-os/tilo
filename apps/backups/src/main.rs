//! Backups — standard Tilo application entry point.

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints remain denied; shared crates retain the full lint set
// because a fault there can affect the session or every app.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
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
    if args.has("--empty") {
        Ok(backups::app_root())
    } else if args.scenario.as_deref() == Some("status") {
        let mut app = backups::BackupsApp::new_demo();
        app.arm_status_bubble_for_shots();
        Ok(Box::new(app))
    } else {
        let mut app = backups::BackupsApp::new_demo();
        if let Some(scenario) = args.scenario.as_deref() {
            app.stage_shot_scenario(scenario);
        }
        Ok(Box::new(app))
    }
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
        ui::framework::run::<backups::BackupsShell>();
        return;
    }
    ui::launch::run_or_exit(
        &LaunchSpec::new("so.tilo.backups", backups::app_root)
            .shot_root(shot_root)
            .scenarios(&[
                "status",
                "progress",
                "failure",
                "restore",
                "sources",
                "destinations",
                "schedules",
                "unavailable",
            ])
            .allowed_flags(&["--empty"])
            .configure_app(|app| backups::actions::register(app.action_registry_mut())),
    );
}
