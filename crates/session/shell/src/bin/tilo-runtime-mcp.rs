//! Persistent native runtime and stdio MCP entry point.
use shell::runtime_host::Session;

fn main() -> std::process::ExitCode {
    // The session hosts its workspace room in process, and the room host
    // decodes Automerge in isolation workers that re-enter this binary; serve
    // them before anything else starts.
    if let Some(code) = tilo_automerge_isolate::dispatch_early() {
        return std::process::ExitCode::from(code);
    }
    tilo_runtime::cli::main::<Session>("tilo-runtime-mcp")
}
