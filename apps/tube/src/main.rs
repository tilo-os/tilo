//! Tube — thin binary entry point. All logic lives in the library; the live
//! backend probes and benchmarks are the `probe` example.

#[cfg(feature = "thin")]
extern crate runtime_dylib;
// The YouTube client comes from its engine library, not this executable.
#[cfg(feature = "runtime-engine")]
extern crate tilo_youtube_engine;

ui::main!(tube::TubeShell);
