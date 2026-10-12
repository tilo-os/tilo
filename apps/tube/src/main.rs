//! Tube — thin binary entry point. All logic lives in the library; the live
//! backend probes and benchmarks are the `probe` example.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

ui::main!(tube::TubeShell);
