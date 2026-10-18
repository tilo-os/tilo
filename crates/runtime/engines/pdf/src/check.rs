//! Proves this engine loads and runs against the runtime it is built with.
//!
//! os/scripts/build-thin-apps.sh builds and runs it on every runtime build,
//! so the engine exists, and is known to resolve against this exact
//! `libruntime_dylib.so`, whichever apps the release selects. The self-check
//! parses and rasterises a one-page document in process (hayro), which
//! reaches into the runtime (`image`, `skrifa`, the runtime's allocator).

use std::process::ExitCode;

fn main() -> ExitCode {
    match tilo_pdf_engine::self_check() {
        Ok(report) => {
            println!("tilo-pdf-engine: {report}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("tilo-pdf-engine: {error}");
            ExitCode::FAILURE
        }
    }
}
