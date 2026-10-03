//! Persistent native runtime and stdio MCP entry point.
use shell::runtime_host::Session;

fn main() -> std::process::ExitCode {
    tilo_runtime::cli::main::<Session>("tilo-runtime-mcp")
}
