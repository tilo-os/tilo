//! Proves this engine loads and runs against the runtime it is built with.
//!
//! os/scripts/build-thin-apps.sh builds and runs it on every runtime build,
//! so the engine exists, and is known to resolve against this exact
//! `libruntime_dylib.so`, whichever apps the release selects. The self-check
//! builds the HTTP client, reads a page and runs the runtime the agent's web
//! tools wait on, which reach into the runtime (`rustls`, `html5ever`,
//! `tokio`).

use std::process::ExitCode;

fn main() -> ExitCode {
    match tilo_agent_web_engine::self_check() {
        Ok(report) => {
            println!("tilo-agent-web-engine: {report}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("tilo-agent-web-engine: {error}");
            ExitCode::FAILURE
        }
    }
}
