use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use tilo_dev_protocol::Manifest;
use tilo_devd::apply::{Apply, RecordingApply};
use tilo_devd::systemd::SystemdApply;
use tilo_devd::{Daemon, server};

/// Guest daemon for Tilo dev VMs. Humans and agents use `tilo-dev` on the host.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve JSON-RPC on stdin/stdout (what `tilo-dev` runs over SSH).
    Rpc {
        /// State root holding layers/ and slots/.
        #[arg(long, default_value = "/var/lib/tilo-dev")]
        root: PathBuf,
        /// Component manifest used when a swap does not carry the component's scope.
        #[arg(long, default_value = "/usr/share/tilo/components.toml")]
        manifest: PathBuf,
        /// What the VM's base image was built with, for layer ABI checks.
        #[arg(long, default_value = "/usr/share/tilo-dev/base.json")]
        base: PathBuf,
        /// `record` performs no side effects; for protocol tests only.
        #[arg(long, value_enum, default_value_t = Backend::Systemd)]
        apply: Backend,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Backend {
    Systemd,
    Record,
}

fn main() -> ExitCode {
    let Command::Rpc {
        root,
        manifest,
        base,
        apply,
    } = Cli::parse().command;
    let base = std::fs::read(&base)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let manifest = match Manifest::load(&manifest) {
        Ok(manifest) => manifest,
        Err(error) => {
            eprintln!("tilo-devd: ignoring manifest: {error}");
            None
        }
    };
    let backend: Box<dyn Apply> = match apply {
        Backend::Systemd => Box::new(SystemdApply::new(&root)),
        Backend::Record => Box::new(RecordingApply::default()),
    };
    let mut daemon = Daemon::new(&root, backend, manifest, base);
    match server::serve(
        &mut daemon,
        std::io::stdin().lock(),
        std::io::stdout().lock(),
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("tilo-devd: {error}");
            ExitCode::FAILURE
        }
    }
}
