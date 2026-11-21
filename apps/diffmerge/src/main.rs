//! The app's entry point on every target: the live window and the capture and headless
//! modes.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

ui::main!(diffmerge::DiffmergeShell);
