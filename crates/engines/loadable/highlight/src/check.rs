//! Proves this engine loads and runs against the runtime it is built with.
//!
//! os/scripts/build-thin-apps.sh builds and runs it on every runtime build,
//! so the engine exists, and is known to resolve against this exact
//! `libruntime_dylib.so`, whichever apps the release selects.

use std::process::ExitCode;

// The library exports only its unmangled entry points (the toolkit opens it,
// and no crate compiles against it), so this binary links it by name.
#[link(name = "tilo_highlight_engine", kind = "dylib")]
unsafe extern "C-unwind" {
    safe fn tilo_highlight_engine_check() -> bool;
}

fn main() -> ExitCode {
    if tilo_highlight_engine_check() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
