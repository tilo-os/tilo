//! The per-user runtime without the Tilo desktop: persistent daemon, app
//! attachment and stdio MCP entry point over the standalone services.
use tilo_runtime::standalone::Standalone;

fn main() -> std::process::ExitCode {
    tilo_runtime::cli::host::<Standalone>("tilo-runtime")
}
