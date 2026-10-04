#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

//! Contacts — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//! * default        — run the live window.
//! * `--shots PATH` — render the window over the hermetic sample book to a
//!   PNG and exit; `--scenario NAME` stages one state of it (`detail`
//!   brings the selected person to the front of a narrow window).

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

use ui::launch::LaunchSpec;

#[cfg(feature = "thin")]
extern crate runtime_dylib;

pub fn main() {
    if capture_requested(std::env::args().skip(1)) {
        ui::framework::run::<contacts::ContactsShell>();
    } else {
        ui::launch::run_or_exit(&launch_spec());
    }
}

/// Whether the command line asks for captures, which the framework app
/// stages from its scenarios over the sample book.
fn capture_requested(mut args: impl Iterator<Item = String>) -> bool {
    args.any(|arg| {
        ["--shots", "--shots-dir", "--responsive-matrix"]
            .iter()
            .any(|flag| arg == *flag || arg.starts_with(&format!("{flag}=")))
    })
}

fn launch_spec() -> LaunchSpec {
    LaunchSpec::new("so.tilo.contacts", contacts::app_root)
        .history(&contacts::framework_history::HISTORY)
        .configure_app(|app| contacts::actions::register(app.action_registry_mut()))
}

/// The production launch declares its history.
#[test]
fn production_launch_registers_history() {
    launch_spec().validate_history_declaration().unwrap();
}
