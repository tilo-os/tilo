//! Proves this engine loads and runs against the runtime it is built with.
//!
//! os/scripts/build-thin-apps.sh builds and runs it on every runtime build,
//! so the engine exists, and is known to resolve against this exact
//! `libruntime_dylib.so`, whichever apps the release selects. The self-check
//! reads the wallpaper catalogue and the keyboard outlines, and asks a mount
//! service that is not there, which reach into the runtime (`toml`,
//! `serde_json`, `std`).

use std::process::ExitCode;

fn main() -> ExitCode {
    match tilo_settings_engine::self_check() {
        Ok(report) => {
            println!("tilo-settings-engine: {report}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("tilo-settings-engine: {error}");
            ExitCode::FAILURE
        }
    }
}
