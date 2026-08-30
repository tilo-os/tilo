//! qos-launcher binary entry.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use desktop_entry::launch::LaunchEnv;
use launcher::landlock_shim::{SANDBOX_EXEC_SUBCOMMAND, run_sandbox_exec};
use launcher::registry;
use launcher::run::{LaunchMode, RunError, RunOptions, run};

#[derive(Debug, Parser)]
#[command(
    name = "qos-launcher",
    about = "Launch a qOS application package in a sandbox."
)]
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
        /// Explicit runtime bundle path. Wins over the manifest's
        /// `runtime = "..."` lookup; mostly for debugging.
        #[arg(long)]
        runtime: Option<PathBuf>,
        /// Skip well-known service-socket resolution; required services warn
        /// instead of failing.
        #[arg(long)]
        no_services: bool,
        /// Bind the minimum host libs a dynamically-linked ELF needs. Deprecated
        /// — prefer a manifest `runtime = "qos-runtime/<v>"` declaration; silently
        /// overridden when a runtime bundle is mounted.
        #[arg(long, hide = true)]
        minimal_libs: bool,
        /// Escape hatch: fall back to `--minimal-libs` when the manifest
        /// declares a runtime but no bundle is installed.
        #[arg(long)]
        allow_missing_runtime: bool,
        /// Accept an unsigned runtime bundle. Default refuses unsigned bundles
        /// (the OS ships signed ones, `qos-install` signs on install) — only
        /// needed for local bring-up / debugging.
        #[arg(long)]
        allow_unsigned_runtime: bool,
        /// Require the runtime bundle's publisher to equal this Ed25519 pubkey
        /// (hex); by default any signed publisher is accepted.
        #[arg(long)]
        require_runtime_publisher: Option<String>,
        /// Host Wayland socket path the app should connect to. Bound into the
        /// sandbox with `WAYLAND_DISPLAY` set to match; omit for apps with no
        /// display.
        #[arg(long)]
        wayland_socket: Option<PathBuf>,
        /// Workspace (collaboration room) this launch belongs to, forwarded as
        /// `QOS_WORKSPACE_ID` — the cluster id CRDT deltas are addressed to, so
        /// two apps with the same value can collaborate. Omit for launches with
        /// no session.
        #[arg(long)]
        workspace_id: Option<String>,
        /// JSON file holding the capabilities (`Vec<CapOverride>`) the user
        /// granted, written by the compositor before spawning. Omitted, missing
        /// or malformed all mean the same: exactly the manifest's sandbox,
        /// nothing widened.
        #[arg(long)]
        capabilities: Option<PathBuf>,
        /// App id the capabilities are addressed to; defaults to the manifest's
        /// package id. Capabilities naming another app are ignored.
        #[arg(long)]
        app_id: Option<String>,
        /// The DRM render node the compositor renders on. The app is pinned to
        /// the same GPU — only that node and its matching loaders are visible.
        /// Omit when no compositor is involved.
        #[arg(long)]
        render_node: Option<PathBuf>,
        /// A document for the app to open on start, as `argv[1]`. An argument
        /// only — the path isn't bound by naming it here; readability is
        /// decided by the manifest plus `--capabilities`.
        #[arg(long)]
        open: Option<PathBuf>,
        /// A validated URI for the app to handle. Forwarded as one opaque
        /// argv value; it is never parsed as a command line.
        #[arg(long, conflicts_with = "open")]
        open_uri: Option<String>,
    },
    /// Launch a CLI command from an already-installed package. Used by the
    /// PATH shim (`qos-cli-shim`).
    RunInstalled {
        /// Reverse-DNS package id, as printed by `qos-install list`.
        #[arg(long, alias = "pkg")]
        package: String,
        /// Which command from `[cli].commands` was invoked, forwarded as
        /// `QOS_CLI_COMMAND`.
        #[arg(long)]
        command: String,
        /// Skip well-known service-socket resolution.
        #[arg(long)]
        no_services: bool,
        /// Bind minimal loader libs (see `run --minimal-libs`).
        #[arg(long)]
        minimal_libs: bool,
        /// Everything after `--` is forwarded verbatim to the entrypoint's argv[1..].
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        rest: Vec<String>,
    },
    /// Resolve an installed package by id and launch its GUI entrypoint.
    /// Counterpart to `run`: takes the reverse-DNS id and looks it up in
    /// the registry `qos-install` writes.
    Exec {
        /// Reverse-DNS package identifier.
        #[arg(long)]
        package: String,
        #[arg(long)]
        no_services: bool,
        /// Host Wayland socket path. Omit for a package with no GUI entrypoint.
        #[arg(long)]
        wayland_socket: Option<PathBuf>,
        /// Collaboration room to launch into. See `run --workspace-id`.
        #[arg(long)]
        workspace_id: Option<String>,
        /// A document for the app to open, as an absolute host path. An
        /// argument, not a permission — see `run --open`.
        #[arg(long)]
        open: Option<PathBuf>,
        #[arg(long, conflicts_with = "open")]
        open_uri: Option<String>,
        /// Fall back to `--minimal-libs` when the manifest declares a runtime
        /// bundle that is not installed.
        #[arg(long)]
        allow_missing_runtime: bool,
        /// Accept an unsigned runtime bundle. Bring-up / debugging only.
        #[arg(long)]
        allow_unsigned_runtime: bool,
    },
    /// Launch an application qOS did **not** package: a `.desktop` entry
    /// already installed on the host, Flatpak or otherwise.
    ///
    /// Runs the entry's own command line UNCONFINED — no qOS sandbox, no
    /// room. A Flatpak brings its own sandbox; a distribution binary has no
    /// manifest to build one from. See the `launcher::host` module docs.
    RunHost {
        /// Desktop-file id, as printed by `qos-desktop-scan`
        /// (`com.spotify.Client`, `org.gnome.TextEditor`).
        #[arg(long)]
        desktop_id: String,
        /// Host path of the compositor's Wayland socket. `WAYLAND_DISPLAY` and
        /// `XDG_RUNTIME_DIR` are both derived from it.
        #[arg(long)]
        wayland_socket: Option<PathBuf>,
        /// A file to open. Expands the entry's `%f`/`%F` field codes (and
        /// `%u`/`%U` as a `file://` URL).
        #[arg(long)]
        open: Option<PathBuf>,
        /// A URL for the application to open. Expands `%u`/`%U`.
        #[arg(long)]
        open_url: Option<String>,
        /// Print the resolved argv and environment and exit without running
        /// anything.
        #[arg(long)]
        dry_run: bool,
    },
}

fn main() -> ExitCode {
    // Hidden subcommand: parsed BEFORE clap so its argv shape (positional
    // paths, `--` separator) doesn't collide with clap's flag parsing.
    let raw_args: Vec<String> = std::env::args().collect();
    if raw_args.get(1).map(std::string::String::as_str) == Some(SANDBOX_EXEC_SUBCOMMAND) {
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
            wayland_socket,
            workspace_id,
            capabilities,
            app_id,
            render_node,
            open,
            open_uri,
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
                mode: LaunchMode::Gui,
                allow_missing_runtime,
                allow_unsigned_runtime,
                require_runtime_publisher,
                identity_host_path: None,
                wayland_socket,
                workspace_id,
                capabilities_path: capabilities,
                app_id,
                render_node,
                open_document: open,
                open_uri,
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
        Cmd::Exec {
            package,
            no_services,
            wayland_socket,
            workspace_id,
            open,
            open_uri,
            allow_missing_runtime,
            allow_unsigned_runtime,
        } => exec_installed(
            &package,
            no_services,
            wayland_socket,
            workspace_id,
            open,
            open_uri,
            allow_missing_runtime,
            allow_unsigned_runtime,
        ),
        Cmd::RunHost {
            desktop_id,
            wayland_socket,
            open,
            open_url,
            dry_run,
        } => run_host(desktop_id, wayland_socket, open, open_url, dry_run),
    }
}

/// Resolve `package` in the registry and launch its GUI entrypoint.
///
/// The registry lookup is the whole difference from `run` — the rest
/// is the same sandboxed launch, built from the same manifest by the
/// same code. The app still gets exactly one writable directory, and
/// no flag here can change that.
fn exec_installed(
    package: &str,
    no_services: bool,
    wayland_socket: Option<PathBuf>,
    workspace_id: Option<String>,
    open: Option<PathBuf>,
    open_uri: Option<String>,
    allow_missing_runtime: bool,
    allow_unsigned_runtime: bool,
) -> ExitCode {
    let entry = match registry::find_installed(package) {
        Ok(Some(e)) => e,
        Ok(None) => {
            eprintln!(
                "qos-launcher: package {package} is not installed. \
                 Run `qos-install list` to see what is."
            );
            return ExitCode::from(2);
        }
        Err(e) => {
            eprintln!("qos-launcher: {e}");
            return ExitCode::from(2);
        }
    };
    let manifest = match launcher::manifest::read_manifest(&entry.manifest_path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("qos-launcher: manifest read: {e}");
            return ExitCode::from(2);
        }
    };
    if manifest.entrypoint.gui_bin().is_none() {
        eprintln!(
            "qos-launcher: {package} declares no GUI entrypoint. \
             Use `qos-launcher run-installed --package {package} --command <name>` \
             for a CLI package."
        );
        return ExitCode::from(2);
    }
    let opts = RunOptions {
        no_services,
        mode: LaunchMode::Gui,
        landlock_enforce: true,
        allow_missing_runtime,
        allow_unsigned_runtime,
        wayland_socket,
        workspace_id,
        app_id: Some(package.to_string()),
        open_document: open,
        open_uri,
        ..Default::default()
    };
    match launcher::run::run_with(&manifest, &entry.payload_root, &opts) {
        Ok(status) => ExitCode::from(status.code().unwrap_or(1) as u8),
        Err(e) => {
            eprintln!("qos-launcher: {e}");
            ExitCode::from(e.to_exit_code() as u8)
        }
    }
}

/// Launch a host application. Unconfined — see [`launcher::host`].
fn run_host(
    desktop_id: String,
    wayland_socket: Option<PathBuf>,
    open: Option<PathBuf>,
    open_url: Option<String>,
    dry_run: bool,
) -> ExitCode {
    let env = LaunchEnv {
        wayland_socket,
        files: open
            .as_ref()
            .map(|p| vec![p.display().to_string()])
            .unwrap_or_default(),
        urls: open_url.into_iter().collect(),
    };
    if dry_run {
        return match launcher::host::plan_host(&desktop_id, &env) {
            Ok(plan) => {
                println!("desktop-id : {}", plan.desktop_id);
                println!("name       : {}", plan.display_name);
                println!("source     : {}", plan.source.label());
                println!("confined   : {}", plan.is_confined());
                println!("program    : {}", plan.program.display());
                println!("args       : {:?}", plan.args);
                println!("windows    : {:?}", plan.window_identities);
                for (k, v) in &plan.env {
                    println!("env        : {k}={v}");
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("qos-launcher: {e}");
                ExitCode::from(2)
            }
        };
    }
    match launcher::host::run_host(&desktop_id, &env) {
        Ok(status) => ExitCode::from(status.code().unwrap_or(1) as u8),
        Err(e) => {
            eprintln!("qos-launcher: {e}");
            ExitCode::from(e.to_exit_code() as u8)
        }
    }
}

/// Resolve an installed package via the registry and launch its
/// CLI-flavour entrypoint under the sandbox.
///
/// A future `app-host` daemon will replace the registry file with a
/// socket-based lookup.
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

    // Read the manifest to check `[cli].commands` contains the requested
    // name. Duplicates work the launcher does for its own sandbox setup,
    // accepted for the clarity of a fail-fast error message.
    let manifest = match launcher::manifest::read_manifest(&entry.manifest_path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("qos-launcher: manifest read: {e}");
            return ExitCode::from(2);
        }
    };
    if !manifest.cli_commands.iter().any(|c| c == command) {
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

    // QOS_LAUNCHER_DISABLE_LANDLOCK=1 is a debug escape hatch, matching the
    // knob threaded through `run` via `RunOptions.landlock_enforce`. Drop
    // once the landlock shim is fully wired.
    let landlock_enforce = !matches!(
        std::env::var("QOS_LAUNCHER_DISABLE_LANDLOCK").as_deref(),
        Ok("1" | "true")
    );
    // CLI-flavour launches through the PATH shim default --minimal-libs
    // on: shim invocations have no place to pass launcher flags, and CLI
    // apps are dynamically-linked ELFs until qos-runtime bundles land.
    let minimal_libs = minimal_libs
        || !matches!(
            std::env::var("QOS_LAUNCHER_NO_MINIMAL_LIBS").as_deref(),
            Ok("1" | "true")
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
        // CLI-shim launches happen from a user shell without extra flags;
        // honour the same env-escape-hatch as landlock so shim users can opt
        // out for debugging.
        allow_unsigned_runtime: matches!(
            std::env::var("QOS_LAUNCHER_ALLOW_UNSIGNED_RUNTIME").as_deref(),
            Ok("1" | "true")
        ),
        require_runtime_publisher: std::env::var("QOS_LAUNCHER_REQUIRE_RUNTIME_PUBLISHER")
            .ok()
            .filter(|s| !s.is_empty()),
        identity_host_path: None,
        // CLI launches are stdio-only: no display in the sandbox, and
        // therefore no compositor whose GPU choice they have to match.
        wayland_socket: None,
        // Inherit the room from the shell the command was typed into: a
        // terminal opened in a workspace lands its CLI apps in the same room
        // rather than none. Outside a session the child gets nothing.
        //
        // This carries no authority: it names a room, not a permission. The
        // capabilities that decide what the app may touch are resolved
        // separately, and deliberately not inherited.
        workspace_id: std::env::var(launcher::sandbox::WORKSPACE_ID_ENV)
            .ok()
            .filter(|s| !s.is_empty()),
        // A shell invocation via the PATH shim has no workspace behind it, so
        // there are no capabilities to honour — the package runs with what
        // its manifest declares.
        capabilities_path: None,
        app_id: Some(package.to_string()),
        render_node: None,
        // A CLI invocation's arguments are the user's own, forwarded through
        // `LaunchMode::Cli`; no document to open on its behalf.
        open_document: None,
        open_uri: None,
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
    let env = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(env)
        .with_target(false)
        .try_init();
}

// Expose RunError so the binary can use #[derive] paths cleanly.
#[allow(dead_code)]
fn _link_run_error(_: &RunError) {}
