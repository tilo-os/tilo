// Share the actual production model/codec/CLI; do not reimplement the format.
#[allow(dead_code)]
#[path = "../../src/model.rs"]
mod model;
#[allow(dead_code)]
#[path = "../../src/profile.rs"]
mod profile;
#[path = "../../src/profile_cli.rs"]
mod profile_cli;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    profile_cli::run(&std::env::args().collect::<Vec<_>>())
}
