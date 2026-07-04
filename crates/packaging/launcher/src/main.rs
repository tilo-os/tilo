//! qos-launcher binary entry.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use launcher::landlock_shim::{SANDBOX_EXEC_SUBCOMMAND, run_sandbox_exec};
use launcher::registry;
use launcher::run::{LaunchMode, RunError, RunOptions, run};

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
        /// Bind the minimum host libraries a dynamically-linked ELF needs
        /// (ld-linux + libc + libgcc_s + /etc/ld.so.cache) — stopgap until
        /// the qos-runtime bundle path lands. Read-only. Deliberately
        /// NARROWER than the whole /usr or /lib64 tree.
        #[arg(long)]
        minimal_libs: bool,
    },
    /// Launch a CLI command from an already-installed package. Used
    /// by the PATH shim (`qos-cli-shim`) so a user typing `notes` in
    /// their terminal ends up here.
    RunInstalled {
        /// Reverse-DNS package id, as printed by `qos-install list`.
        #[arg(long, alias = "pkg")]
        package: String,
        /// Which command from the manifest's `[cli].commands` was
        /// invoked. Forwarded into the sandbox as `QOS_CLI_COMMAND`.
        #[arg(long)]
        command: String,
        /// Skip well-known service-socket resolution.
        #[arg(long)]
        no_services: bool,
        /// Bind minimal loader libs (see `run --minimal-libs`).
        #[arg(long)]
        minimal_libs: bool,
        /// Everything after `--` is forwarded verbatim to the
        /// entrypoint as its argv[1..].
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        rest: Vec<String>,
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
        } => {
            let opts = RunOptions {
                no_services,
                runtime_root: runtime,
                bwrap_bin: None,
                minimal_libs,
                landlock_enforce: true,
                mode: LaunchMode::Gui,
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
        Cmd::RunInstalled {
            package,
            command,
            no_services,
            minimal_libs,
            rest,
        } => run_installed(&package, &command, no_services, minimal_libs, rest),
        Cmd::Exec { package, .. } => {
            eprintln!(
                "qos-launcher exec --package {package}: not yet implemented (requires qos-pkg package-store integration)"
            );
            ExitCode::from(2)
        }
    }
}

/// Resolve an installed package via the registry and launch its
/// CLI-flavour entrypoint under the sandbox.
///
/// A future `app-host` daemon will replace the registry file with
/// a socket-based lookup; when that lands, this function should
/// try the socket first and fall back to the file reader.
fn run_installed(
    package: &str,
    command: &str,
    no_services: bool,
    minimal_libs: bool,
    rest: Vec<String>,
) -> ExitCode {
    let entry = match registry::find_installed(package) {
        Ok(Some(e)) => e,
        Ok(None) => {
            eprintln!(
                "qos-launcher: package {package} is not installed. Run `qos-install list` to see what is."
            );
            return ExitCode::from(2);
        }
        Err(e) => {
            eprintln!("qos-launcher: {e}");
            return ExitCode::from(2);
        }
    };

    // Read the manifest so we can check `[cli].commands` contains
    // the requested name. The launcher already re-reads the manifest
    // for its own sandbox setup, so this is duplicated work; we
    // accept it for the clarity of a fail-fast error message.
    let manifest = match launcher::manifest::read_manifest(&entry.manifest_path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("qos-launcher: manifest read: {e}");
            return ExitCode::from(2);
        }
    };
    if !manifest
        .cli_commands
        .iter()
        .any(|c| c == command)
    {
        eprintln!(
            "qos-launcher: {package} does not declare CLI command {command:?} — check its manifest's [cli] block."
        );
        return ExitCode::from(2);
    }
    if manifest.entrypoint.cli_bin().is_none() {
        eprintln!(
            "qos-launcher: {package}'s entrypoint is not a CLI (type != cli / both). Add `type = \"cli\"` to the manifest."
        );
        return ExitCode::from(2);
    }

    let opts = RunOptions {
        no_services,
        runtime_root: None,
        bwrap_bin: None,
        minimal_libs,
        landlock_enforce: true,
        mode: LaunchMode::Cli {
            command: command.to_string(),
            passthrough_argv: rest,
        },
    };
    match launcher::run::run_with(&manifest, &entry.payload_root, &opts) {
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
