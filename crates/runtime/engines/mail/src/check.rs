//! Proves this engine loads and runs against the runtime it is built with.
//!
//! os/scripts/build-thin-apps.sh builds and runs it on every runtime build,
//! so the engine exists, and is known to resolve against this exact
//! `libruntime_dylib.so`, whichever apps the release selects. The self-check
//! writes a message, reads it back and addresses its envelope, which reach
//! into the runtime (`std`, the allocator).

use std::process::ExitCode;

fn main() -> ExitCode {
    match tilo_mail_engine::self_check() {
        Ok(report) => {
            println!("tilo-mail-engine: {report}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("tilo-mail-engine: {error}");
            ExitCode::FAILURE
        }
    }
}
