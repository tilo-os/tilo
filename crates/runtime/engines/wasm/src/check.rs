//! Proves this engine loads against the runtime it is built with.
//!
//! os/scripts/build-thin-apps.sh builds and starts it with every runtime build, so
//! the engine exists and resolves against this exact `libruntime_dylib.so`.

use std::process::ExitCode;

fn main() -> ExitCode {
    match tilo_wasm_engine::self_check() {
        Ok(report) => {
            println!("tilo-wasm-engine: {report}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("tilo-wasm-engine: {error}");
            ExitCode::FAILURE
        }
    }
}
