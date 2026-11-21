//! The app's entry point on every target: the live window, the capture and headless
//! modes.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

ui::main!(api_tester::ApiTesterShell);
