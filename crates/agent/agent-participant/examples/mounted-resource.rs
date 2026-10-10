//! An explicit acceptance run, requiring a compiled web bundle and Chromium.
//! All identities, storage and governed rooms belong to this temporary fixture.
#[cfg(not(test))]
#[expect(
    dead_code,
    reason = "the acceptance example reuses the room-actions test fixtures, including helpers for other scenarios"
)]
#[path = "../tests/room_actions.rs"]
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
    let Ok(runtime) = tokio::runtime::Runtime::new() else {
        return std::process::ExitCode::FAILURE;
    };
    runtime.block_on(fixture::mounted_resource::run());
    std::process::ExitCode::SUCCESS
}
