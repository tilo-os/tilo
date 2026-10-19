//! Proves this engine loads and runs against the runtime it is built with.
//!
//! os/scripts/build-thin-apps.sh builds and runs it on every runtime build,
//! so the engine exists, and is known to resolve against this exact
//! `libruntime_dylib.so`, whichever apps the release selects. The self-check
//! reads a Chromium history database through browser-continuity's capture,
//! which reaches into the runtime (`tilo-files`, `serde`, the allocator).

use std::process::ExitCode;

fn main() -> ExitCode {
    match tilo_browser_import_engine::self_check() {
        Ok(report) => {
            println!("tilo-browser-import-engine: {report}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("tilo-browser-import-engine: {error}");
            ExitCode::FAILURE
        }
    }
}
