//! Proves this engine loads and runs against the runtime it is built with.
//!
//! os/scripts/build-thin-apps.sh builds and runs it on every runtime build,
//! so the engine exists, and is known to resolve against this exact
//! `libruntime_dylib.so`, whichever apps the release selects. The self-check
//! reads the English catalogue and a translated one, and searches it, which
//! reach into the runtime (`tilo-search`, the allocator).

use std::process::ExitCode;

fn main() -> ExitCode {
    match tilo_emoji_engine::self_check() {
        Ok(report) => {
            println!("tilo-emoji-engine: {report}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("tilo-emoji-engine: {error}");
            ExitCode::FAILURE
        }
    }
}
