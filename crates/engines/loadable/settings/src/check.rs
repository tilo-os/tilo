//! Proves this engine loads and runs against the runtime it is built with.
//!
//! os/scripts/build-thin-apps.sh builds and runs it on every runtime build,
//! so the engine exists, and is known to resolve against this exact
//! `libruntime_dylib.so`, whichever apps the release selects. The self-check
//! reads the wallpaper catalogue and the keyboard outlines, and asks a mount
//! service that is not there, which reach into the runtime (`toml`,
//! `serde_json`, `std`).

use std::process::ExitCode;

// The library exports only its unmangled entry points (the engine is no
// crate's dependency), so this binary links it by name, not as a crate.
#[link(name = "tilo_settings_engine", kind = "dylib")]
unsafe extern "C-unwind" {
    safe fn tilo_settings_engine_check() -> bool;
}

fn main() -> ExitCode {
    if tilo_settings_engine_check() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
