//! Proves this engine loads and runs against the runtime it is built with.
//!
//! os/scripts/build-thin-apps.sh builds and runs it on every runtime build,
//! so the engine exists, and is known to resolve against this exact
//! `libruntime_dylib.so`, whichever apps the release selects. The self-check
//! runs the `OAuth` client's PKCE, the committed client table and the IMAP
//! parser, which reach into the runtime (`sha2`, `serde`, `toml`, `nom`),
//! without a network or an account.

use std::process::ExitCode;

// The library exports only its unmangled entry points (no crate compiles
// against it), so this binary links it by name, not as a crate.
#[link(name = "tilo_accounts_engine", kind = "dylib")]
unsafe extern "C-unwind" {
    safe fn tilo_accounts_engine_check() -> bool;
}

fn main() -> ExitCode {
    if tilo_accounts_engine_check() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
