//! The per-user runtime without the Tilo desktop: persistent daemon, app
//! attachment and stdio MCP entry point over the standalone services.
use tilo_runtime::standalone::Standalone;

fn main() -> std::process::ExitCode {
    // Automerge isolation workers re-enter this binary; serve them before any
    // thread starts.
    if let Some(code) = tilo_automerge_isolate::dispatch_early() {
        return std::process::ExitCode::from(code);
    }
    tilo_runtime::cli::main::<Standalone>("tilo-runtime")
}
