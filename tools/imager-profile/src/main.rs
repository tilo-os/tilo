// Share the actual production profile model and codec; do not reimplement the
// format.
mod cli;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    cli::run(&std::env::args().collect::<Vec<_>>())
}
