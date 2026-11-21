//! The app's entry point on every target: the live window and the standard `--shots`,
//! `--headless-drive` and `--native-audit` modes on Linux and macOS.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

ui::main!(activity::App);
