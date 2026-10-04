//! Containers — standard Tilo application entry point.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

ui::main!(containers::ContainersShell);
