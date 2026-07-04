//! qos-launcher binary entry.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use launcher::landlock_shim::{SANDBOX_EXEC_SUBCOMMAND, run_sandbox_exec};
use launcher::run::{RunError, RunOptions, run};

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
        /// Explicit path to a runtime bundle directory. Wins over the
        /// manifest's `runtime = "..."` lookup. Mostly for debugging.
        #[arg(long)]
        runtime: Option<PathBuf>,
        /// Skip well-known service-socket resolution. Required services
        /// will be warned about instead of fatal.
        #[arg(long)]
        no_services: bool,
        /// Bind the minimum host libraries a dynamically-linked ELF needs
        /// (ld-linux + libc + libgcc_s + /etc/ld.so.cache). Deprecated —
        /// prefer a manifest `runtime = "qos-runtime/<v>"` declaration
        /// plus `qos-install runtime install`. Left in for backward
        /// compat; silently overridden when a runtime bundle is
        /// mounted.
        #[arg(long, hide = true)]
        minimal_libs: bool,
        /// When the manifest declares a runtime but no bundle is
        /// installed, don't refuse to launch — fall back to whatever
        /// --minimal-libs implies. Escape hatch.
        #[arg(long)]
        allow_missing_runtime: bool,
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
    // Hidden subcommand: parsed BEFORE clap so its argv shape
    // (positional paths, `--` separator) doesn't collide with clap's
    // flag parsing. Diverges — either execvps the entrypoint or exits.
    let raw_args: Vec<String> = std::env::args().collect();
    if raw_args.get(1).map(|s| s.as_str()) == Some(SANDBOX_EXEC_SUBCOMMAND) {
        run_sandbox_exec(&raw_args[2..]);
    }

    init_tracing();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Run {
            manifest,
            payload_root,
            runtime,
            no_services,
            minimal_libs,
            allow_missing_runtime,
        } => {
            if minimal_libs {
                eprintln!(
                    "qos-launcher: --minimal-libs is deprecated. \
                     Prefer a manifest `runtime = \"qos-runtime/<v>\"` declaration \
                     plus `qos-install runtime install --version <v> --from-host`."
                );
            }
            let opts = RunOptions {
                no_services,
                runtime_root: runtime,
                bwrap_bin: None,
                minimal_libs,
                landlock_enforce: true,
                allow_missing_runtime,
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
