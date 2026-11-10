//! The MCP front door on a relay with its app workers, on loopback, for an
//! SDK's end-to-end run: what tilo.sh runs, in one process.
//!
//! `door-on-relay` prints `{"door", "key", "reviewer_key", "person"}` and
//! serves until its standard input closes; `door-on-relay -- <command…>`
//! runs the command with `TILO_DOOR`, `TILO_API_KEY` and
//! `TILO_REVIEWER_KEY` set and exits as it does:
//!
//! ```sh
//! door-on-relay -- python3 sdk/python/examples/visa_proof.py
//! ```
#[cfg(not(test))]
#[expect(
    dead_code,
    reason = "the harness reuses the workspace-actions test fixtures, including helpers for other scenarios"
)]
#[path = "../tests/workspace_actions.rs"]
mod fixture;

#[cfg(not(test))]
use fixture::support;

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
    runtime.block_on(fixture::door_on_relay::serve(&command))
}
