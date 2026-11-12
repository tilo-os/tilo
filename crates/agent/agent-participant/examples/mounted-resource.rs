//! An explicit acceptance run, requiring a compiled web bundle and Chromium.
//! All identities, storage and governed workspaces belong to this temporary fixture.
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
    reason = "the acceptance example reuses the workspace-actions test fixtures, including helpers for other scenarios"
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
    let Ok(runtime) = tokio::runtime::Runtime::new() else {
        return std::process::ExitCode::FAILURE;
    };
    runtime.block_on(fixture::mounted_resource::run());
    std::process::ExitCode::SUCCESS
}
