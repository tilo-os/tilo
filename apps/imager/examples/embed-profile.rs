//! Compatibility entrypoint; the standalone tool avoids compiling the GUI.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    imager::profile_cli::run(&std::env::args().collect::<Vec<_>>())
}
