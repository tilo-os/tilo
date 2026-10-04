//! Maps — thin binary entry point. All logic lives in the library.
//!
//! The binary runs [`maps::MapsShell`]: the live window, or a `--shots`
//! capture of one of its scenarios (`--scenario NAME`, every one with
//! `--shots-dir DIR`).

#[cfg(feature = "thin")]
extern crate runtime_dylib;

ui::main!(maps::MapsShell);
