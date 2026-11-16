//! Proves this engine loads and runs against the runtime it is built with.
//!
//! os/scripts/build-thin-apps.sh builds and runs it on every runtime build,
//! so the engine exists, and is known to resolve against this exact
//! `libruntime_dylib.so`, whichever apps the release selects. The self-check
//! evaluates cells whose functions reach into the runtime (`chrono`,
//! `chrono-tz`, `statrs`, `regex`).

use std::process::ExitCode;

fn main() -> ExitCode {
    match tilo_spreadsheet_engine::self_check() {
        Ok(report) => {
            println!("tilo-spreadsheet-engine: {report}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("tilo-spreadsheet-engine: {error}");
            ExitCode::FAILURE
        }
    }
}
