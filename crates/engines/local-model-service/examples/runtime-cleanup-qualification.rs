//! Developer-only fixed process qualification; no model or catalog is admitted.
use clap::Parser;
use local_model_service::supervision::developer_cleanup_probe;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::PathBuf;

#[derive(Parser)]
struct Arguments {
    /// An already imported immutable OCI image; never a mutable tag.
    #[arg(long)]
    image: String,
    /// A new private output directory; existing evidence is never overwritten.
    #[arg(long)]
    out: PathBuf,
}

fn run() -> Result<(), String> {
    let arguments = Arguments::parse();
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&arguments.out)
        .map_err(|error| error.to_string())?;
    let directory = arguments
        .out
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let receipt =
        developer_cleanup_probe(&arguments.image, &directory).map_err(|error| error.to_string())?;
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(directory.join("receipt.json"))
        .map_err(|error| error.to_string())?;
    let bytes = serde_json::to_vec_pretty(&receipt).map_err(|error| error.to_string())?;
    output
        .write_all(&bytes)
        .map_err(|error| error.to_string())?;
    output.write_all(b"\n").map_err(|error| error.to_string())?;
    output.sync_all().map_err(|error| error.to_string())?;
    println!("exact owned-container cleanup and independent owned control preservation observed");
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("runtime cleanup qualification failed: {error}");
        std::process::exit(1);
    }
}
