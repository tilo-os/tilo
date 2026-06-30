//! qos-launcher binary entry.
//!
//! Initial commit lands the CLI skeleton — `run` only parses the
//! manifest and prints a summary so we can verify the manifest shim
//! end-to-end. Sandbox/Landlock/seccomp/services wiring lands in
//! subsequent commits.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use qos_launcher::manifest::read_manifest;

#[derive(Debug, Parser)]
#[command(name = "qos-launcher", about = "Launch a qOS application package in a sandbox.")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Run a package directly from an unpacked payload + manifest.
    Run {
        /// Path to the package manifest (TOML).
        #[arg(long)]
        manifest: PathBuf,
        /// Directory containing the unpacked package payload.
        #[arg(long)]
        payload_root: PathBuf,
        /// Optional path to a qos-runtime tree (mounted ro at
        /// /usr/lib/qos-runtime/1/).
        #[arg(long)]
        runtime: Option<PathBuf>,
        /// Skip well-known service-socket resolution. Required services
        /// will be warned about instead of fatal.
        #[arg(long)]
        no_services: bool,
    },
    /// Resolve an installed package by id and launch it. Not yet
    /// implemented — the package store / index lives in `qos-pkg`.
    Exec {
        /// Reverse-DNS package identifier.
        #[arg(long)]
        package: String,
        #[arg(long)]
        no_services: bool,
    },
}

fn main() -> ExitCode {
    init_tracing();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Run {
            manifest,
            payload_root,
            runtime,
            no_services,
        } => {
            let _ = (payload_root, runtime, no_services);
            match read_manifest(&manifest) {
                Ok(m) => {
                    eprintln!(
                        "qos-launcher: parsed manifest for `{}` (entrypoint: {})",
                        m.package_id, m.entrypoint.bin
                    );
                    eprintln!("qos-launcher: sandbox/run wiring not yet landed on this commit");
                    ExitCode::from(0)
                }
                Err(e) => {
                    eprintln!("qos-launcher: {e}");
                    ExitCode::from(1)
                }
            }
        }
        Cmd::Exec { package, .. } => {
            eprintln!(
                "qos-launcher exec --package {package}: not yet implemented (requires qos-pkg package-store integration)"
            );
            ExitCode::from(2)
        }
    }
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let env = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(env)
        .with_target(false)
        .try_init();
}
