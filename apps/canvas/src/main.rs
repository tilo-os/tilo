//! The app's entry point on every target: the live window and the standard
//! `--shots`, `--headless-drive` and `--native-audit` modes on Linux and macOS,
//! registration with the page runtime on the web.

#[cfg(feature = "thin")]
extern crate runtime_dylib;
// The painting engines come from their engine library, not this executable.
#[cfg(feature = "runtime-engine")]
extern crate tilo_paint_engine;

ui::main!(canvas::App);
