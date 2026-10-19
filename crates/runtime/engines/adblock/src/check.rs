//! Proves this engine loads and runs against the runtime it is built with.
//!
//! os/scripts/build-thin-apps.sh builds and runs it on every runtime build,
//! so the engine exists, and is known to resolve against this exact
//! `libruntime_dylib.so`, whichever apps the release selects.

use std::process::ExitCode;

fn main() -> ExitCode {
    match tilo_adblock_engine::self_check() {
        Ok(report) => {
            println!("tilo-adblock-engine: {report}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("tilo-adblock-engine: {error}");
            ExitCode::FAILURE
        }
    }
}
