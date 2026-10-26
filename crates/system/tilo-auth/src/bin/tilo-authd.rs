//! `tilo-authd`: the one place a person's secret is checked.

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use tilo_auth::accounts::SystemLegacy;
use tilo_auth::daemon::{Config, Daemon, server};

#[derive(Parser)]
#[command(
    name = "tilo-authd",
    about = "Checks every person's secret and owns their records"
)]
struct Cli {
    /// TPM2 TCTI. Omit for the kernel resource manager at /dev/tpmrm0.
    #[arg(long)]
    tcti: Option<String>,
    #[arg(long, default_value = tilo_auth::SOCKET)]
    socket: PathBuf,
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let daemon = Arc::new(Daemon::new(
        Config {
            tcti: cli.tcti,
            socket: cli.socket,
            ..Config::default()
        },
        Box::new(SystemLegacy::default()),
    )?);
    let sweeper = Arc::clone(&daemon);
    std::thread::spawn(move || sweeper.forget_wrong_entries_when_due());
    daemon.take_boot_handoff();
    daemon.settle_volume_key();
    let listener = server::bind(&daemon)?;
    server::serve(&daemon, &listener);
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("tilo-authd: {e:#}");
            ExitCode::FAILURE
        }
    }
}
