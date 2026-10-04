#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

//! Calculator — thin binary entry point. All logic lives in the library.
//!
//! The binary runs the framework app: a live window or, with
//! `--shots PATH [--scenario NAME]`, one rendered frame of the same app.

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

#[cfg(feature = "thin")]
extern crate runtime_dylib;

ui::android_main!();

fn main() {
    ui::framework::run::<calculator::CalculatorShell>();
}
