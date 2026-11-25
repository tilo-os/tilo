//! The gateway's service entry point: a local socket on Unix, a named pipe on Windows.
fn main() {
    tilo_model_gateway::service::main();
}
