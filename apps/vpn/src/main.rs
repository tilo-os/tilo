//! VPN — standard Tilo application entry point.

// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop, and proving every layout sum cannot
// overflow is a tax a third-party app author would pay too. The lints that
// produce a *crash* stay denied — indexing_slicing, string_slice,
// unwrap_used, expect_used, panic — because a panicking app is a bad app
// whoever wrote it. `shell`, `ui` and everything under `crates/` keep the
// full set: a fault there takes the session, or every app with it.
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

#[allow(
    clippy::unnecessary_wraps,
    reason = "must match ui::launch::ShotFactory's fn(&LaunchArgs) -> Result<..> signature"
)]
fn shot_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    Ok(match args.scenario.as_deref() {
        Some("confirm") => vpn::app_root_demo_confirm(),
        Some("add" | "import") => vpn::app_root_demo_add(),
        Some(
            state @ ("detail" | "config" | "logs" | "loading" | "error" | "disconnected"
            | "connecting" | "unprovisioned" | "setup"),
        ) => vpn::app_root_demo_state(state),
        Some("empty") => vpn::app_root_demo_state("empty"),
        _ if std::env::var("TILO_VPN_SAMPLE").as_deref() == Ok("1") => vpn::app_root_demo(),
        _ => vpn::app_root(),
    })
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
    LaunchSpec::new("so.tilo.vpn", vpn::app_root)
        .shot_root(shot_root)
        .scenarios(&[
            "confirm",
            "add",
            "import",
            "detail",
            "config",
            "logs",
            "loading",
            "empty",
            "error",
            "disconnected",
            "connecting",
            "unprovisioned",
            "setup",
        ])
        .configure_app(|app| vpn::actions::register(app.action_registry_mut()))
}

pub fn main() {
    if wants_standalone() {
        ui::launch::run_or_exit(&shot_spec());
        return;
    }
    ui::launch::run_or_exit(&vpn::launch_spec());
}
