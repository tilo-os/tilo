//! Proves this engine loads and runs against the runtime it is built with.
//!
//! os/scripts/build-thin-apps.sh builds and runs it on every runtime build,
//! so the engine exists, and is known to resolve against this exact
//! `libruntime_dylib.so`, whichever apps the release selects. The self-check
//! runs the `OAuth` client's PKCE, the committed client table and the IMAP
//! parser, which reach into the runtime (`sha2`, `serde`, `toml`, `nom`),
//! without a network or an account.

use std::process::ExitCode;

fn main() -> ExitCode {
    match tilo_accounts_engine::self_check() {
        Ok(report) => {
            println!("tilo-accounts-engine: {report}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("tilo-accounts-engine: {error}");
            ExitCode::FAILURE
        }
    }
}
