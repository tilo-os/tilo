//! `tilo-sibling-unlock`: unlock an encrypted root with another of the
//! owner's devices.
//!
//! * `probe DEVICE`: exit 0 when the volume carries a sibling-unlock token.

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;
use tilo_sibling_unlock::luks;

#[derive(Parser)]
#[command(
    name = "tilo-sibling-unlock",
    about = "Unlock an encrypted root with another of your devices"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Exit 0 when DEVICE carries a sibling-unlock token, 1 otherwise.
    Probe { device: PathBuf },
}

fn main() -> Result<ExitCode> {
    match Cli::parse().command {
        Command::Probe { device } => Ok(match luks::token(&device) {
            Ok(Some(_)) => ExitCode::SUCCESS,
            _ => ExitCode::FAILURE,
        }),
    }
}
