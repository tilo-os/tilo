//! Proves this engine loads and runs against the runtime it is built with.
//!
//! os/scripts/build-thin-apps.sh builds and runs it on every runtime build,
//! so the engine exists, and is known to resolve against this exact
//! `libruntime_dylib.so`, whichever apps the release selects. The self-check
//! projects a contact, confirms and resolves one of its endpoints, which
//! reaches into the runtime (`tilo-resource`, `uuid`).

use std::process::ExitCode;

fn main() -> ExitCode {
    match tilo_contact_directory_engine::self_check() {
        Ok(report) => {
            println!("tilo-contact-directory-engine: {report}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("tilo-contact-directory-engine: {error}");
            ExitCode::FAILURE
        }
    }
}
