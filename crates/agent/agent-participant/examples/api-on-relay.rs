//! The API server on a relay with its app workers, on loopback, for an
//! SDK's end-to-end run: what tilo.sh runs, in one process.
//!
//! `api-on-relay` prints `{"api", "key", "key_file", "invitee", "person"}`
//! and serves until its standard input closes; `api-on-relay -- <command…>`
//! runs the command with `TILO_API_URL`, `TILO_API_KEY`, `TILO_KEY_FILE` (a
//! key file of a device of the person, for an SDK's native part) and
//! `TILO_INVITEE` set and exits as it does. `POST {"link": …}` to `TILO_INVITEE` and another
//! person follows the invitation link, approves a key of their own and
//! answers `{"key", "person", "joined"}`:
//!
//! ```sh
//! api-on-relay -- python3 sdk/python/examples/visa_proof.py
//! ```
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::significant_drop_tightening,
    clippy::unwrap_used,
    reason = "the test fixtures this example reuses assert with these"
)]

#[cfg(not(test))]
#[path = "../tests/common/mod.rs"]
mod common;
#[cfg(not(test))]
#[path = "../tests/harness/mod.rs"]
mod harness;
#[cfg(not(test))]
#[path = "../../../session/client/tests/support/participant.rs"]
mod participant;
#[cfg(not(test))]
#[path = "../../../session/client/tests/support/mod.rs"]
mod support;

#[cfg(not(test))]
#[expect(
    dead_code,
    reason = "the harness reuses the workspace-actions test fixtures, including helpers for other scenarios"
)]
#[path = "../tests/workspace_actions.rs"]
mod fixture;

#[cfg(not(test))]
fn main() -> std::process::ExitCode {
    if let Some(code) = tilo_automerge_isolate::dispatch_early() {
        return std::process::ExitCode::from(code);
    }
    if let Some(code) = tilo_agent_participant::worker::dispatch_early() {
        return std::process::ExitCode::from(code);
    }
    // `RUST_LOG` says what the relay, its app workers and the door log.
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let command: Vec<std::ffi::OsString> = std::env::args_os()
        .skip(1)
        .skip_while(|argument| argument != "--")
        .skip(1)
        .collect();
    let Ok(runtime) = tokio::runtime::Runtime::new() else {
        return std::process::ExitCode::FAILURE;
    };
    runtime.block_on(fixture::api_on_relay::serve(&command))
}
