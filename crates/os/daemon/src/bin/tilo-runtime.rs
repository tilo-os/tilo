//! This computer's one Tilo daemon (plain `tilo-runtime`, which `tilo` starts and the user service runs),
//! app attachment, and the stdio MCP entry point an agent's client runs.

fn main() -> std::process::ExitCode {
    // Settings this process's host installed none of are what the profile's session left
    // for processes outside it.
    settings_store::register_outside();
    tilo_runtime::device::capabilities::mount(tilo_daemon::capabilities());
    tilo_runtime::cli::host("tilo-runtime")
}
