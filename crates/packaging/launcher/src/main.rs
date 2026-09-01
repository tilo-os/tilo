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

use std::path::{Path, PathBuf};
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
        /// Host-staged governance identity projection. Honoured only for
        /// Files and mounted read-only at its fixed projection path.
        #[arg(long)]
        trusted_identities: Option<PathBuf>,
        /// App id the capabilities are addressed to; defaults to the manifest's
        /// package id. Capabilities naming another app are ignored.
        #[arg(long)]
        app_id: Option<String>,
        /// Exact host identity record selected for this workspace. The shell
        /// resolves this from its private identity registry.
        #[arg(long)]
        identity: Option<PathBuf>,
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
    /// Admit a `qos-file` bearer from stdin through the per-user filesystem
    /// service, then open Files at the resulting non-secret mount path.
    AdmitShare {
        /// Host Wayland socket passed to Files.
        #[arg(long)]
        wayland_socket: Option<PathBuf>,
        /// Allow a missing runtime bundle for local bring-up.
        #[arg(long)]
        allow_missing_runtime: bool,
        /// Accept an unsigned runtime bundle for local bring-up.
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

/// Fold an `i32` exit code (from [`std::process::ExitStatus::code`] or
/// [`RunError::to_exit_code`]) into the `u8` [`ExitCode`] takes. Both
/// sources are always in `0..=255` in practice — a POSIX wait status is
/// already masked to the low byte, and `RunError`'s codes are small
/// constants — so the `1` fallback (generic failure) never fires for a real
/// exit.
fn exit_code(code: i32) -> ExitCode {
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

fn main() -> ExitCode {
    // Hidden subcommand: parsed BEFORE clap so its argv shape (positional
    // paths, `--` separator) doesn't collide with clap's flag parsing.
    let raw_args: Vec<String> = std::env::args().collect();
    if raw_args.get(1).map(std::string::String::as_str) == Some(SANDBOX_EXEC_SUBCOMMAND) {
        run_sandbox_exec(raw_args.get(2..).unwrap_or(&[]));
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
            trusted_identities,
            app_id,
            identity,
            render_node,
            open,
            open_uri,
        } => run_gui_command(RunCliArgs {
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
            trusted_identities,
            app_id,
            identity,
            render_node,
            open,
            open_uri,
        }),
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
            ExecArgs {
                no_services,
                wayland_socket,
                workspace_id,
                open,
                open_uri,
                allow_missing_runtime,
                allow_unsigned_runtime,
                capabilities_path: None,
            },
        ),
        Cmd::AdmitShare {
            wayland_socket,
            allow_missing_runtime,
            allow_unsigned_runtime,
        } => admit_share_and_open(
            wayland_socket,
            allow_missing_runtime,
            allow_unsigned_runtime,
        ),
        Cmd::RunHost {
            desktop_id,
            wayland_socket,
            open,
            open_url,
            dry_run,
        } => run_host(
            &desktop_id,
            wayland_socket,
            open.as_deref(),
            open_url,
            dry_run,
        ),
    }
}

/// Every field of [`Cmd::Run`], bundled so [`run_gui_command`] doesn't count
/// against clippy's argument limit — this is argv plumbing, not a sandbox
/// policy struct (see [`ExecArgs`] below for the same pattern).
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent CLI flag mirrored 1:1 from Cmd::Run; grouping them would just move the same four toggles into a sub-struct destructured back apart at the one call site"
)]
struct RunCliArgs {
    manifest: PathBuf,
    payload_root: PathBuf,
    runtime: Option<PathBuf>,
    no_services: bool,
    minimal_libs: bool,
    allow_missing_runtime: bool,
    allow_unsigned_runtime: bool,
    require_runtime_publisher: Option<String>,
    wayland_socket: Option<PathBuf>,
    workspace_id: Option<String>,
    capabilities: Option<PathBuf>,
    trusted_identities: Option<PathBuf>,
    app_id: Option<String>,
    identity: Option<PathBuf>,
    render_node: Option<PathBuf>,
    open: Option<PathBuf>,
    open_uri: Option<String>,
}

/// Build [`RunOptions`] from the `run` subcommand's flags and launch the
/// package's GUI entrypoint.
fn run_gui_command(args: RunCliArgs) -> ExitCode {
    if args.minimal_libs {
        eprintln!(
            "qos-launcher: --minimal-libs is deprecated. \
             Prefer a manifest `runtime = \"qos-runtime/<v>\"` declaration \
             plus `qos-install runtime install --version <v> --from-host`."
        );
    }
    let opts = RunOptions {
        no_services: args.no_services,
        runtime_root: args.runtime,
        bwrap_bin: None,
        sandbox_shim_bin: None,
        minimal_libs: args.minimal_libs,
        landlock_enforce: true,
        mode: LaunchMode::Gui,
        allow_missing_runtime: args.allow_missing_runtime,
        allow_unsigned_runtime: args.allow_unsigned_runtime,
        require_runtime_publisher: args.require_runtime_publisher,
        identity_host_path: args.identity,
        wayland_socket: args.wayland_socket,
        workspace_id: args.workspace_id,
        capabilities_path: args.capabilities,
        trusted_identities_host_path: args.trusted_identities,
        app_id: args.app_id,
        render_node: args.render_node,
        open_document: args.open,
        open_uri: args.open_uri,
    };
    match run(&args.manifest, &args.payload_root, &opts) {
        Ok(status) => {
            if let Some(code) = status.code() {
                exit_code(code)
            } else {
                ExitCode::from(1)
            }
        }
        Err(e) => {
            eprintln!("qos-launcher: {e}");
            exit_code(e.to_exit_code())
        }
    }
}

fn admit_share_and_open(
    wayland_socket: Option<PathBuf>,
    allow_missing_runtime: bool,
    allow_unsigned_runtime: bool,
) -> ExitCode {
    match admit_share_from_stdin() {
        Ok(path) => match received_file_capability(&path) {
            Ok((capability_path, guard)) => {
                let result = exec_installed(
                    "dev.qos.file-explorer",
                    ExecArgs {
                        no_services: false,
                        wayland_socket,
                        workspace_id: None,
                        open: Some(path),
                        open_uri: None,
                        allow_missing_runtime,
                        allow_unsigned_runtime,
                        capabilities_path: Some(capability_path),
                    },
                );
                drop(guard);
                result
            }
            Err(error) => {
                eprintln!("qos-launcher: {error}");
                ExitCode::from(2)
            }
        },
        Err(error) => {
            eprintln!("qos-launcher: {error}");
            ExitCode::from(2)
        }
    }
}

fn admit_share_from_stdin() -> Result<PathBuf, String> {
    use std::io::Read;
    use std::os::unix::net::UnixStream;

    const MAX_URI_BYTES: usize = 16 * 1024;
    let mut uri = Vec::new();
    std::io::stdin()
        .take((MAX_URI_BYTES + 1) as u64)
        .read_to_end(&mut uri)
        .map_err(|_| "could not read shared-file link from stdin".to_string())?;
    if uri.len() > MAX_URI_BYTES {
        return Err("shared-file link exceeds its bound".into());
    }
    while matches!(uri.last(), Some(b'\r' | b'\n')) {
        uri.pop();
    }
    let uri =
        String::from_utf8(uri).map_err(|_| "shared-file link is not valid text".to_string())?;
    if uri.is_empty() {
        return Err("shared-file link is empty".into());
    }

    let socket = launcher::services::socket_for("qos-fs");
    let mut stream = UnixStream::connect(&socket)
        .map_err(|_| "received-file service is unavailable".to_string())?;
    request_received_mount(uri, &mut stream)
}

fn request_received_mount(
    uri: String,
    stream: &mut (impl std::io::Read + std::io::Write),
) -> Result<PathBuf, String> {
    let request = collab_types::RecipientControlRequestV1 {
        protocol_version: collab_types::RECIPIENT_CONTROL_VERSION,
        operation: collab_types::RecipientControlOperationV1::AdmitV2 { uri },
    };
    let request = collab_types::canonical_bytes(&request)
        .map_err(|_| "could not encode received-file request".to_string())?;
    if request.is_empty() || request.len() > collab_types::MAX_RECIPIENT_CONTROL_FRAME_BYTES {
        return Err("received-file request exceeds its bound".into());
    }
    let request_len = u32::try_from(request.len())
        .map_err(|_| "received-file request exceeds its wire bound".to_string())?;
    stream
        .write_all(&request_len.to_be_bytes())
        .and_then(|()| stream.write_all(&request))
        .map_err(|_| "received-file service is unavailable".to_string())?;
    let mut prefix = [0u8; 4];
    stream
        .read_exact(&mut prefix)
        .map_err(|_| "received-file service did not respond".to_string())?;
    let length = u32::from_be_bytes(prefix) as usize;
    if length == 0 || length > collab_types::MAX_RECIPIENT_CONTROL_FRAME_BYTES {
        return Err("received-file service returned an oversized response".into());
    }
    let mut response = vec![0u8; length];
    stream
        .read_exact(&mut response)
        .map_err(|_| "received-file service response was truncated".to_string())?;
    let response: collab_types::RecipientControlResponseV1 =
        collab_types::canonical::from_canonical_bytes(&response)
            .map_err(|_| "received-file service returned an invalid response".to_string())?;
    let status = match response.result {
        collab_types::RecipientControlResultV1::Admitted { status, .. } => status,
        collab_types::RecipientControlResultV1::Error { code } => {
            return Err(format!("received-file service refused the link ({code:?})"));
        }
        collab_types::RecipientControlResultV1::Status(_) => {
            return Err("received-file service returned the wrong response".into());
        }
    };
    let mount = status
        .mount
        .ok_or_else(|| "received-file service did not provide a mount".to_string())?;
    let path = PathBuf::from(mount.path);
    if !path.is_absolute() {
        return Err("received-file service returned a relative mount path".into());
    }
    Ok(path)
}

struct TemporaryCapability(PathBuf);

impl Drop for TemporaryCapability {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn received_file_capability(
    path: &std::path::Path,
) -> Result<(PathBuf, TemporaryCapability), String> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| "received-file mount is unavailable".to_string())?;
    if !path.is_absolute() || metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("received-file mount is not a safe directory".into());
    }

    let root = launcher::services::services_root()
        .parent()
        .ok_or_else(|| "runtime directory is unavailable".to_string())?
        .join("launcher-grants");
    match std::fs::create_dir(&root) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => {
            return Err("could not create the temporary capability directory".to_string());
        }
    }
    let root_metadata = std::fs::symlink_metadata(&root)
        .map_err(|_| "could not inspect the temporary capability directory".to_string())?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err("temporary capability directory is not a safe directory".into());
    }
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
        .map_err(|_| "could not secure the temporary capability directory".to_string())?;
    let capability = collab_types::CapOverride::new(
        collab_types::Subject::app("dev.qos.file-explorer"),
        collab_types::Scope::FsPath(path.display().to_string()),
        collab_types::Effect::Grant,
        collab_types::Verb::Read,
    );
    let bytes = serde_json::to_vec(&vec![capability])
        .map_err(|_| "could not encode the received-file capability".to_string())?;
    let destination = root.join(format!("received-{}.json", uuid::Uuid::new_v4().simple()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&destination)
        .map_err(|_| "could not create the received-file capability".to_string())?;
    let guard = TemporaryCapability(destination.clone());
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| "could not persist the received-file capability".to_string())?;
    Ok((destination, guard))
}

/// CLI arguments for [`Cmd::Exec`], grouped to keep [`exec_installed`] under
/// clippy's argument-count limit — this is argv plumbing, not a sandbox
/// policy struct.
struct ExecArgs {
    no_services: bool,
    wayland_socket: Option<PathBuf>,
    workspace_id: Option<String>,
    open: Option<PathBuf>,
    open_uri: Option<String>,
    allow_missing_runtime: bool,
    allow_unsigned_runtime: bool,
    capabilities_path: Option<PathBuf>,
}

/// Resolve `package` in the registry and launch its GUI entrypoint.
///
/// The registry lookup is the whole difference from `run` — the rest
/// is the same sandboxed launch, built from the same manifest by the
/// same code. The app still gets exactly one writable directory, and
/// no flag here can change that.
fn exec_installed(package: &str, args: ExecArgs) -> ExitCode {
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
        no_services: args.no_services,
        mode: LaunchMode::Gui,
        landlock_enforce: true,
        allow_missing_runtime: args.allow_missing_runtime,
        allow_unsigned_runtime: args.allow_unsigned_runtime,
        wayland_socket: args.wayland_socket,
        workspace_id: args.workspace_id,
        app_id: Some(package.to_string()),
        capabilities_path: args.capabilities_path,
        open_document: args.open,
        open_uri: args.open_uri,
        ..Default::default()
    };
    match launcher::run::run_with(&manifest, &entry.payload_root, &opts) {
        Ok(status) => exit_code(status.code().unwrap_or(1)),
        Err(e) => {
            eprintln!("qos-launcher: {e}");
            exit_code(e.to_exit_code())
        }
    }
}

/// Launch a host application. Unconfined — see [`launcher::host`].
fn run_host(
    desktop_id: &str,
    wayland_socket: Option<PathBuf>,
    open: Option<&Path>,
    open_url: Option<String>,
    dry_run: bool,
) -> ExitCode {
    let env = LaunchEnv {
        wayland_socket,
        files: open
            .map(|p| vec![p.display().to_string()])
            .unwrap_or_default(),
        urls: open_url.into_iter().collect(),
    };
    if dry_run {
        return match launcher::host::plan_host(desktop_id, &env) {
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
    match launcher::host::run_host(desktop_id, &env) {
        Ok(status) => exit_code(status.code().unwrap_or(1)),
        Err(e) => {
            eprintln!("qos-launcher: {e}");
            exit_code(e.to_exit_code())
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
        sandbox_shim_bin: None,
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
        trusted_identities_host_path: None,
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
                exit_code(code)
            } else {
                ExitCode::from(1)
            }
        }
        Err(e) => {
            eprintln!("qos-launcher: {e}");
            exit_code(e.to_exit_code())
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

#[cfg(test)]
mod admit_share_tests {
    use super::*;
    use std::io::{Cursor, Read, Write};

    struct ScriptedStream {
        written: Vec<u8>,
        response: Cursor<Vec<u8>>,
    }

    impl Read for ScriptedStream {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            self.response.read(buffer)
        }
    }

    impl Write for ScriptedStream {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.written.extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn response(path: &str) -> ScriptedStream {
        let response = collab_types::RecipientControlResponseV1 {
            protocol_version: collab_types::RECIPIENT_CONTROL_VERSION,
            result: collab_types::RecipientControlResultV1::Admitted {
                inserted: true,
                status: collab_types::RecipientSubscriptionStatusV1 {
                    key: collab_types::RecipientSubscriptionKeyV1 {
                        workspace_id: uuid::Uuid::nil(),
                        record_id: collab_types::FileRecordId(uuid::Uuid::from_u128(1)),
                    },
                    permission: collab_types::FilePermission::View,
                    last_success_at_ms: None,
                    last_error: None,
                    mount: Some(collab_types::RecipientMountStatusV1 {
                        path: path.into(),
                        running: true,
                    }),
                },
            },
        };
        let bytes = collab_types::canonical_bytes(&response).unwrap();
        let mut framed = Vec::new();
        let length = u32::try_from(bytes.len()).unwrap();
        framed.extend_from_slice(&length.to_be_bytes());
        framed.extend_from_slice(&bytes);
        ScriptedStream {
            written: Vec::new(),
            response: Cursor::new(framed),
        }
    }

    #[test]
    fn admission_sends_the_bearer_only_in_the_service_frame() {
        let uri = "qos-file://v2/payload#bearer-secret".to_string();
        let mut stream = response("/home/alice/Shared/record");
        let mount = request_received_mount(uri.clone(), &mut stream).unwrap();
        assert_eq!(mount, PathBuf::from("/home/alice/Shared/record"));
        let length = u32::from_be_bytes(stream.written[..4].try_into().unwrap()) as usize;
        let request: collab_types::RecipientControlRequestV1 =
            collab_types::canonical::from_canonical_bytes(&stream.written[4..4 + length]).unwrap();
        assert!(matches!(
            request.operation,
            collab_types::RecipientControlOperationV1::AdmitV2 { uri: got } if got == uri
        ));
    }

    #[test]
    fn admission_refuses_a_relative_mount_from_the_service() {
        let mut stream = response("relative/path");
        assert!(request_received_mount("redacted".into(), &mut stream).is_err());
    }
}
