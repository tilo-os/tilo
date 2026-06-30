//! qos-launcher binary entry.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use qos_launcher::run::{RunError, RunOptions, run};

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
            let opts = RunOptions {
                no_services,
                runtime_root: runtime,
                bwrap_bin: None,
            };
            match run(&manifest, &payload_root, &opts) {
                Ok(status) => {
                    if let Some(code) = status.code() {
                        ExitCode::from(code as u8)
                    } else {
                        ExitCode::from(1)
                    }
                }
                Err(e) => {
                    eprintln!("qos-launcher: {e}");
                    ExitCode::from(e.to_exit_code() as u8)
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

// Expose RunError so the binary can use #[derive] paths cleanly.
#[allow(dead_code)]
fn _link_run_error(_: &RunError) {}
