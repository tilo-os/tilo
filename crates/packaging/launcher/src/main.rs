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
        /// Accept an unsigned runtime bundle. Default is to refuse
        /// unsigned bundles: the OS image ships signed bundles baked
        /// by `os/scripts/bake-runtime-bundle.sh`, and user-installed
        /// bundles are signed on install by `qos-install`. Only
        /// needed for local bring-up / debugging.
        #[arg(long)]
        allow_unsigned_runtime: bool,
        /// Require the runtime bundle's publisher to equal this
        /// Ed25519 pubkey (hex). By default any signed publisher is
        /// accepted; a strict deployment pins one here.
        #[arg(long)]
        require_runtime_publisher: Option<String>,
        /// Everything after `--` is forwarded verbatim to the
        /// entrypoint as its argv[1..]. Lets `qos-install run <pkg>
        /// -- <args>` propagate through this launcher into the
        /// sandboxed program.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        rest: Vec<String>,
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
            allow_missing_runtime,
            allow_unsigned_runtime,
            require_runtime_publisher,
            rest,
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
                mode: LaunchMode::Gui { passthrough_argv: rest },
                allow_missing_runtime,
                allow_unsigned_runtime,
                require_runtime_publisher,
                identity_host_path: None,
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

    // QOS_LAUNCHER_DISABLE_LANDLOCK=1 is a debug escape hatch —
    // matches the equivalent knob threaded through `run` today
    // via the `--no-landlock` flag (unimplemented in the CLI but
    // wired through RunOptions.landlock_enforce). Once the
    // landlock shim is fully wired, drop the env var.
    let landlock_enforce = !matches!(
        std::env::var("QOS_LAUNCHER_DISABLE_LANDLOCK").as_deref(),
        Ok("1") | Ok("true")
    );
    // For CLI-flavour launches through the PATH shim we default
    // --minimal-libs on: shim invocations don't have a good place
    // to pass launcher flags, and CLI apps are dynamically-linked
    // ELFs today until qos-runtime bundles land. The user can
    // still opt out via `qos-launcher run-installed --command X`
    // directly without --minimal-libs.
    let minimal_libs = minimal_libs || !matches!(
        std::env::var("QOS_LAUNCHER_NO_MINIMAL_LIBS").as_deref(),
        Ok("1") | Ok("true")
    );
    let opts = RunOptions {
        no_services,
        runtime_root: None,
        bwrap_bin: None,
        minimal_libs,
        landlock_enforce,
        mode: LaunchMode::Cli {
            command: command.to_string(),
            passthrough_argv: rest,
        },
        allow_missing_runtime: false,
        // CLI-shim launches happen from a user shell without extra
        // flags; honour the same env-escape-hatch the launcher uses
        // for landlock so shell-shim users can opt out for
        // debugging without changing their PATH invocation.
        allow_unsigned_runtime: matches!(
            std::env::var("QOS_LAUNCHER_ALLOW_UNSIGNED_RUNTIME").as_deref(),
            Ok("1") | Ok("true")
        ),
        require_runtime_publisher: std::env::var("QOS_LAUNCHER_REQUIRE_RUNTIME_PUBLISHER")
            .ok()
            .filter(|s| !s.is_empty()),
        identity_host_path: None,
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
