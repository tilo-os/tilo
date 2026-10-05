//! Music — thin binary entry point. All logic lives in the library.
//!
//! The binary is the framework app: the live window, `--shots` scenarios,
//! `--headless` driving, a local file or `music://` link to open, and the
//! `--probe`/`--bench` commands (see `music::commands`).

#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

#[cfg(feature = "thin")]
extern crate runtime_dylib;

ui::main!(music::MusicShell);
