//! The runtime host a Tilo login runs: the persistent daemon, app attachment
//! and stdio MCP entry point over the profile's own services, beside the
//! desktop session. `runtime_portal::native` looks for this name inside a
//! login, where an app attaches to the session's runtime rather than to one of
//! its own.
use tilo_runtime::standalone::Standalone;

fn main() -> std::process::ExitCode {
    tilo_runtime::cli::host::<Standalone>("tilo-runtime-mcp")
}
