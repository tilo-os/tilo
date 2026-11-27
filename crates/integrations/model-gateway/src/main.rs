//! The gateway's service entry point: a local socket on Unix, a named pipe on Windows.
fn main() {
    // Settings this process's host installed none of are what the profile's session left
    // for processes outside it.
    settings_store::register_outside();
    tilo_model_gateway::service::main();
}
