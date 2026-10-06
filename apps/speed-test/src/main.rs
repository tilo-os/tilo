//! The app's entry point: the live window, and the standard `--shots`,
//! `--headless-drive` and `--native-audit` modes, from one declaration.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

ui::main!(speed_test::SpeedTestShell);
