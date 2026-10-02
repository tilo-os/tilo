//! Database client — standard Tilo application entry point.

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

use ui::launch::{LaunchArgs, LaunchError, LaunchSpec, ShotScenario, build_shot_scenario};
use ui::widget::Widget;

const SHOT_SCENARIOS: &[ShotScenario] = &[
    ShotScenario {
        name: "default",
        build: shot_default,
    },
    ShotScenario {
        name: "empty",
        build: shot_empty,
    },
    ShotScenario {
        name: "status",
        build: shot_status,
    },
    ShotScenario {
        name: "connect",
        build: shot_connect,
    },
    ShotScenario {
        name: "confirm",
        build: shot_confirm,
    },
];

fn shot_width(args: &LaunchArgs) -> f64 {
    f64::from(args.viewport.unwrap_or(ui::launch::DEFAULT_VIEWPORT).0)
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "signature is fixed by ui::launch::ShotFactory"
)]
fn shot_default(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    Ok(db_client::app_root_for_shot(shot_width(args)))
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "signature is fixed by ui::launch::ShotFactory"
)]
fn shot_empty(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    Ok(db_client::app_root_for_shot_empty(shot_width(args)))
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "signature is fixed by ui::launch::ShotFactory"
)]
fn shot_status(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    Ok(db_client::app_root_for_shot_status(shot_width(args)))
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "signature is fixed by ui::launch::ShotFactory"
)]
fn shot_connect(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    Ok(db_client::app_root_for_shot_connect(shot_width(args)))
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "signature is fixed by ui::launch::ShotFactory"
)]
fn shot_confirm(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    Ok(db_client::app_root_for_shot_confirm(shot_width(args)))
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "signature is fixed by ui::launch::ShotFactory"
)]
fn shot_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    // Legacy flag aliases map onto the scenario table so existing scripts
    // keep working while `--scenario` is the preferred door.
    let mut args = args.clone();
    if args.scenario.is_none() {
        if args.has("--empty") {
            args.scenario = Some("empty".into());
        } else if args.has("--status") {
            args.scenario = Some("status".into());
        } else if args.has("--connect") {
            args.scenario = Some("connect".into());
        } else if args.has("--confirm") {
            args.scenario = Some("confirm".into());
        }
    }
    build_shot_scenario(&args, SHOT_SCENARIOS, shot_default)
}

#[cfg(feature = "thin")]
extern crate runtime_dylib;

fn wants_standalone() -> bool {
    std::env::args().any(|arg| {
        [
            "--shots",
            "--responsive-matrix",
            "--native-audit",
            "--headless",
        ]
        .iter()
        .any(|mode| arg == *mode || arg.starts_with(&format!("{mode}=")))
    })
}

fn shot_spec() -> LaunchSpec {
    LaunchSpec::new("so.tilo.db-client", db_client::app_root)
        .shot_root(shot_root)
        .scenarios(&["default", "empty", "status", "connect", "confirm"])
        .allowed_flags(&["--empty", "--status", "--connect", "--confirm"])
        .configure_app(|app| {
            db_client::actions::register(app.action_registry_mut());
            app.set_window_title(Some("Database Client".into()));
        })
}

pub fn main() {
    if wants_standalone() {
        ui::launch::run_or_exit(&shot_spec());
        return;
    }
    ui::launch::run_or_exit(&db_client::launch_spec());
}
