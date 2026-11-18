//! Proves this engine loads and answers against the runtime it is built with.
//!
//! os/scripts/build-thin-apps.sh builds and calls it on every runtime build,
//! so the engine exists, and is known to resolve against this exact
//! `libruntime_dylib.so`, whichever apps the release selects. The self-check
//! expands a stored rule and reads the dates it answers in a named zone.

use std::process::ExitCode;

fn main() -> ExitCode {
    match tilo_recurrence_engine::self_check() {
        Ok(report) => {
            println!("tilo-recurrence-engine: {report}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("tilo-recurrence-engine: {error}");
            ExitCode::FAILURE
        }
    }
}
