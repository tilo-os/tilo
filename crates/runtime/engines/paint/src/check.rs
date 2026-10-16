//! Proves this engine loads and runs against the runtime it is built with.
//!
//! os/scripts/build-thin-apps.sh builds and runs it on every runtime build,
//! so the engine exists, and is known to resolve against this exact
//! `libruntime_dylib.so`, whichever apps the release selects. The self-check
//! paints a stroke with every built-in medium, which runs the simulation's
//! own code and reaches into the runtime (`tiny-skia`, `rayon`, `blake3`).

use std::process::ExitCode;

fn main() -> ExitCode {
    match tilo_paint_engine::tilo_wet_media::self_check() {
        Ok(report) => {
            println!("tilo-paint-engine: {report}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("tilo-paint-engine: {error}");
            ExitCode::FAILURE
        }
    }
}
