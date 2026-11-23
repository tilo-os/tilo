//! This computer's one Tilo daemon (plain `tilo-runtime`, which `tilo` starts and the user service runs),
//! app attachment, and the stdio MCP entry point an agent's client runs.

fn main() -> std::process::ExitCode {
    tilo_runtime::cli::host("tilo-runtime")
}
