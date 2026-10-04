//! Authenticator — standard Tilo application entry point.
//!
//! Live windows and `--shots` captures both run on the resource framework;
//! `--shots --scenario NAME` stages one of the app's screens.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(feature = "thin")]
extern crate runtime_dylib;

pub fn main() {
    ui::launch::run_or_exit(&totp::launch_spec());
}
