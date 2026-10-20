//! The app's entry point on every target: the live window and the standard
//! `--shots`, `--headless-drive` and `--native-audit` modes on Linux and macOS,
//! registration with the page runtime on the web.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

ui::main!(poker::App);
