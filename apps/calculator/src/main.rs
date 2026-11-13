#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

//! Thin binary entry point; the framework runs the window and the `--shots`,
//! `--headless` and `--native-audit` modes.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

ui::android_main!();

ui::main!(calculator::CalculatorShell);
