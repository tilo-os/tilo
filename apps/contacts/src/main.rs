#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

//! Contacts — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//! * default        — run the live window.
//! * `--shots PATH` — render the window over the hermetic sample book to a
//!   PNG and exit; `--scenario NAME` stages one state of it, `--detail`
//!   brings the selected person to the front of a narrow window.

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

/// Every state a capture can stage.
const SCENARIOS: &[&str] = &[
    "new",
    "edit",
    "calendar",
    "export",
    "unsaved",
    "groups",
    "favorites",
    "context",
    "more",
    "confirm",
    "import-conflict",
    "duplicate-review",
    "empty",
];

#[allow(
    clippy::unnecessary_wraps,
    reason = "signature is fixed by ui::launch::ShotFactory"
)]
fn shot_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    // Captures use the hermetic sample book, so a frame is populated and
    // never reads or writes the person's address book.
    let mut app = contacts::ContactsApp::sample();
    if let Some(scenario) = args.scenario.as_deref() {
        app.stage_scenario(scenario);
    }
    if args.has("--detail") {
        app.shots_show_detail();
    }
    Ok(Box::new(app))
}

#[cfg(feature = "thin")]
extern crate runtime_dylib;

pub fn main() {
    ui::launch::run_or_exit(&launch_spec());
}

fn launch_spec() -> LaunchSpec {
    LaunchSpec::new("so.tilo.contacts", contacts::app_root)
        .history(&contacts::framework_history::HISTORY)
        .shot_root(shot_root)
        .allowed_flags(&["--detail"])
        .scenarios(SCENARIOS)
        .configure_app(|app| contacts::actions::register(app.action_registry_mut()))
}

/// The production launch declares its history.
#[test]
fn production_launch_registers_history() {
    launch_spec().validate_history_declaration().unwrap();
}
