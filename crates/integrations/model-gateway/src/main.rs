//! Native Unix-descriptor service entry point.
#[cfg(unix)]
fn main() {
    tilo_model_gateway::service::main();
}

#[cfg(not(unix))]
fn main() {
    eprintln!("tilo-model-gateway: Unix capability transport is required");
    std::process::exit(1);
}
