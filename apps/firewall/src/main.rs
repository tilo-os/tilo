//! Firewall — standard Tilo application entry point.

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

use ui::launch::{LaunchArgs, LaunchError, LaunchSpec, ShotScenario, build_shot_scenario};
use ui::widget::Widget;

const SHOT_SCENARIOS: &[ShotScenario] = &[
    ShotScenario {
        name: "default",
        build: shot_default,
    },
    ShotScenario {
        name: "empty",
        build: shot_named,
    },
    ShotScenario {
        name: "error",
        build: shot_named,
    },
    ShotScenario {
        name: "confirmation",
        build: shot_named,
    },
    ShotScenario {
        name: "active-change",
        build: shot_named,
    },
    ShotScenario {
        name: "service-error",
        build: shot_named,
    },
    ShotScenario {
        name: "services",
        build: shot_named,
    },
    ShotScenario {
        name: "selected",
        build: shot_named,
    },
];

#[allow(
    clippy::unnecessary_wraps,
    reason = "must match the ShotFactory fn-pointer type, which returns Result"
)]
fn shot_default(_: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    // Captures are always hermetic and use only the bundled example ruleset.
    // Live startup remains the only path that interrogates the host firewall.
    Ok(Box::new(firewall::FirewallApp::new_demo()))
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "must match the ShotFactory fn-pointer type, which returns Result"
)]
fn shot_named(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    let mut app = firewall::FirewallApp::new_demo();
    if let Some(scenario) = args.scenario.as_deref() {
        app.pose_fixture(scenario);
    }
    Ok(Box::new(app))
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "must match the ShotFactory fn-pointer type, which returns Result"
)]
fn shot_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    build_shot_scenario(args, SHOT_SCENARIOS, shot_default)
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
    LaunchSpec::new("so.tilo.firewall", firewall::app_root)
        .shot_root(shot_root)
        .scenarios(&[
            "default",
            "empty",
            "error",
            "confirmation",
            "active-change",
            "service-error",
            "services",
            "selected",
        ])
        .configure_app(|app| firewall::actions::register(app.action_registry_mut()))
}

pub fn main() {
    if wants_standalone() {
        ui::launch::run_or_exit(&shot_spec());
        return;
    }
    ui::launch::run_or_exit(&firewall::launch_spec());
}
