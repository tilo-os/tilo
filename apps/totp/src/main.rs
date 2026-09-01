//! Authenticator — standard qOS application entry point.
//!
//! `--shots PATH` uses a deterministic sample vault. `--screen NAME` selects
//! `add`, `add-advanced`, `add-picker`, or `empty`; the default is the account
//! list. `--viewport WxH` controls the screenshot size.

// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop. Crash-producing lints remain denied.
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

const APP_ID: &str = "dev.qos.totp";
const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 800);
const SCREENS: &[&str] = &["add", "add-advanced", "add-picker", "empty"];

fn shot_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    match args.value("--screen") {
        None => Ok(totp::app_root_demo()),
        Some("add") => Ok(totp::app_root_add_screen()),
        Some("add-advanced") => Ok(totp::app_root_add_advanced_screen()),
        Some("add-picker") => Ok(totp::app_root_add_picker_screen()),
        Some("empty") => Ok(totp::app_root_empty()),
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
    totp::actions::register(app.action_registry_mut());
}

#[cfg(feature = "thin")]
extern crate runtime_dylib;

pub fn main() {
    ui::launch::run_or_exit(
        &LaunchSpec::new(APP_ID, totp::app_root)
            .shot_root(shot_root)
            .shot_viewport(|_| DEFAULT_SHOTS_VIEWPORT)
            .allowed_options(&["--screen"])
            .configure_app(configure),
    );
}
