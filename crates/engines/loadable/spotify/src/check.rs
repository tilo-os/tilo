//! Proves this engine loads and runs against the runtime it is built with.
//!
//! os/scripts/build-thin-apps.sh builds and runs it on every runtime build,
//! so the engine exists, and is known to resolve against this exact
//! `libruntime_dylib.so`, whichever apps the release selects. The self-check
//! runs librespot's own code (track identities, session and player
//! configuration) without an account, a network or an audio device.

use std::process::ExitCode;

fn main() -> ExitCode {
    match tilo_spotify_engine::tilo_spotify::self_check() {
        Ok(report) => {
            println!("tilo-spotify-engine: {report}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("tilo-spotify-engine: {error}");
            ExitCode::FAILURE
        }
    }
}
