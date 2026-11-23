//! Proves this engine loads and runs against the runtime it is built with.
//!
//! os/scripts/build-thin-apps.sh builds and runs it on every runtime build,
//! so the engine exists, and is known to resolve against this exact
//! `libruntime_dylib.so`, whichever apps the release selects. The self-check
//! packs a file into a tar.zst, lists it and extracts it again, which reach
//! into the runtime (`std`, the allocator, the filesystem).

use std::process::ExitCode;

fn main() -> ExitCode {
    match tilo_archive_engine::self_check() {
        Ok(report) => {
            println!("tilo-archive-engine: {report}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("tilo-archive-engine: {error}");
            ExitCode::FAILURE
        }
    }
}
