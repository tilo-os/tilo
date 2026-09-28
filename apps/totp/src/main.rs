//! Authenticator — standard Tilo application entry point.
//!
//! Live windows run on the resource framework. `--shots` stays on the
//! standalone seeded roots.

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

const APP_ID: &str = "so.tilo.totp";
const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 800);
const SCREENS: &[&str] = &[
    "list",
    "add",
    "add-advanced",
    "add-picker",
    "empty",
    "provision",
    "persistence-error",
    "missing-secret",
];

fn shot_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    match args.scenario.as_deref().or_else(|| args.value("--screen")) {
        None | Some("list") => Ok(totp::app_root_demo()),
        Some("add") => Ok(totp::app_root_add_screen()),
        Some("add-advanced") => Ok(totp::app_root_add_advanced_screen()),
        Some("add-picker") => Ok(totp::app_root_add_picker_screen()),
        Some("empty") => Ok(totp::app_root_empty()),
        Some("provision") => Ok(totp::app_root_provision_screen()),
        Some("persistence-error") => Ok(totp::app_root_persistence_error()),
        Some("missing-secret") => Ok(totp::app_root_missing_secret()),
        Some(screen) => Err(LaunchError::Scenario(format!(
            "unknown Authenticator screen {screen:?}; expected one of {}",
            SCREENS
                .iter()
                .map(|name| format!("{name:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

fn configure(app: &mut ui::App) {
    app.set_window_title(Some("Authenticator".into()));
    app.set_capture_protected(true);
    totp::actions::register(app.action_registry_mut());
}

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
    LaunchSpec::new(APP_ID, totp::app_root)
        .shot_root(shot_root)
        .shot_viewport(|_| DEFAULT_SHOTS_VIEWPORT)
        .scenarios(SCREENS)
        .allowed_options(&["--screen"])
        .configure_app(configure)
}

#[cfg(feature = "thin")]
extern crate runtime_dylib;

pub fn main() {
    if wants_standalone() {
        ui::launch::run_or_exit(&shot_spec());
        return;
    }
    ui::launch::run_or_exit(&totp::launch_spec());
}
