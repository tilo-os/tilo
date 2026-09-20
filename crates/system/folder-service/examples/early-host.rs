// Test host for the early-dispatch composition contract, not a deployed helper.
fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    std::process::exit(tilo_folder_service::startup::dispatch_early(&args)?.unwrap_or(2));
}
