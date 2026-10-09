//! tilo-launcher binary entry.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use std::io::Read;
use std::os::fd::{FromRawFd, RawFd};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use desktop_entry::launch::LaunchEnv;
use launcher::landlock_shim::{SANDBOX_EXEC_SUBCOMMAND, run_sandbox_exec};
use launcher::registration_gate::LAUNCH_REGISTRATION_FD_ENV;
use launcher::run::{IdentitySource, LaunchMode, RunError, RunOptions, run};

fn wait_for_registration_fd(fd: RawFd) -> std::io::Result<()> {
    if fd < 3 {
        return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
    }
    // SAFETY: the compositor transfers ownership of this inherited descriptor
    // specifically through `LAUNCH_REGISTRATION_FD_ENV`.
    let mut gate = unsafe { std::fs::File::from_raw_fd(fd) };
    let mut release = [0_u8; 1];
    gate.read_exact(&mut release)
}

#[allow(
    clippy::disallowed_methods,
    reason = "runs first in main before any thread is spawned, and the internal handoff variable must be removed so launched apps never inherit it"
)]
fn wait_for_shell_registration() -> Result<(), String> {
    let Ok(raw_fd) = std::env::var(LAUNCH_REGISTRATION_FD_ENV) else {
        return Ok(());
    };
    // SAFETY: process startup is single-threaded; remove the internal handoff
    // before any launched app environment can inherit it.
    unsafe { std::env::remove_var(LAUNCH_REGISTRATION_FD_ENV) };
    let fd = raw_fd
        .parse::<RawFd>()
        .map_err(|_| "invalid launch-registration descriptor".to_string())?;
    wait_for_registration_fd(fd)
        .map_err(|error| format!("launch-registration handoff failed: {error}"))
}

#[derive(Debug, Parser)]
#[command(
    name = "tilo-launcher",
    about = "Launch a Tilo application package in a sandbox."
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
        /// Refuse if the actual signed installation differs from this root.
        #[arg(long)]
        expected_package_hash: Option<String>,
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
        /// — prefer a manifest `runtime = "tilo-runtime/<v>"` declaration; silently
        /// overridden when a runtime bundle is mounted.
        #[arg(long, hide = true)]
        minimal_libs: bool,
        /// Escape hatch: fall back to `--minimal-libs` when the manifest
        /// declares a runtime but no bundle is installed.
        #[arg(long)]
        allow_missing_runtime: bool,
        /// Accept an unsigned runtime bundle. Default refuses unsigned bundles
        /// (the OS ships signed ones, `tilo-install` signs on install) — only
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
        /// `TILO_WORKSPACE_ID` — the cluster id CRDT deltas are addressed to, so
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
        #[command(flatten)]
        action_authority: Box<ActionAuthorityArgs>,
        /// App id the caller launched; must equal the manifest's package id.
        /// Capabilities naming another app are ignored.
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
        /// Launch this package's headless entrypoint as a desktop-managed
        /// service. Valid only for `kind = "service"`; no display or open
        /// resource is handed into the sandbox.
        #[arg(long, conflicts_with_all = ["wayland_socket", "open", "open_uri"])]
        service: bool,
        /// Trusted service argv, forwarded verbatim after the entrypoint.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        rest: Vec<String>,
    },
    /// Launch a CLI command from an already-installed package. Used by the
    /// PATH shim (`tilo-cli-shim`).
    RunInstalled {
        /// Reverse-DNS package id, as printed by `tilo-install list`.
        #[arg(long, alias = "pkg")]
        package: String,
        /// Which command from `[cli].commands` was invoked, forwarded as
        /// `TILO_CLI_COMMAND`.
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
    /// the package store's registry.
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
    /// Admit a `tilo-file` bearer from stdin through the per-user filesystem
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
    /// Launch an application Tilo did **not** package: a `.desktop` entry
    /// already installed on the host, Flatpak or otherwise.
    ///
    /// Runs the entry's own command line UNCONFINED — no Tilo sandbox, no
    /// room. A Flatpak brings its own sandbox; a distribution binary has no
    /// manifest to build one from. See the `launcher::host` module docs.
    RunHost(RunHostArgs),
}

#[derive(Debug, clap::Args)]
struct RunHostArgs {
    /// Desktop-file id, as printed by `tilo-desktop-scan`
    /// (`com.spotify.Client`, `org.gnome.TextEditor`).
    #[arg(long)]
    desktop_id: String,
    /// Run this desktop file instead of looking `--desktop-id` up in the
    /// `applications` directories — how XDG autostart entries, which live in
    /// `autostart` directories, are started.
    #[arg(long)]
    desktop_file: Option<PathBuf>,
    /// Host path of the compositor's Wayland socket. `WAYLAND_DISPLAY` and
    /// `XDG_RUNTIME_DIR` are both derived from it.
    #[arg(long)]
    wayland_socket: Option<PathBuf>,
    /// Rootless Xwayland display supplied by the compositor for legacy
    /// X11 applications.
    #[arg(long, requires = "x11_authority")]
    x11_display: Option<String>,
    /// Xauthority file paired with `--x11-display`.
    #[arg(long, requires = "x11_display")]
    x11_authority: Option<PathBuf>,
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
    /// Prefer the desktop entry's declared New Window action.
    #[arg(long)]
    new_window: bool,
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
    if let Err(error) = wait_for_shell_registration() {
        eprintln!("tilo-launcher: {error}");
        return ExitCode::from(126);
    }
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
            expected_package_hash,
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
            action_authority,
            app_id,
            identity,
            render_node,
            open,
            open_uri,
            service,
            rest,
        } => run_gui_command(RunCliArgs {
            manifest,
            expected_package_hash,
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
            action_authority: action_authority.action_authority,
            app_id,
            identity,
            render_node,
            open,
            open_uri,
            service,
            rest,
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
        Cmd::RunHost(args) => run_host(args),
    }
}

#[derive(Debug, clap::Args)]
struct ActionAuthorityArgs {
    /// Host-staged, app-scoped action authority; mounted read-only.
    #[arg(long)]
    action_authority: Option<PathBuf>,
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
    expected_package_hash: Option<String>,
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
    action_authority: Option<PathBuf>,
    app_id: Option<String>,
    identity: Option<PathBuf>,
    render_node: Option<PathBuf>,
    open: Option<PathBuf>,
    open_uri: Option<String>,
    service: bool,
    rest: Vec<String>,
}

/// Build [`RunOptions`] from the `run` subcommand's flags and launch either
/// the package's GUI entrypoint or its explicitly requested managed-service
/// entrypoint.
fn run_gui_command(args: RunCliArgs) -> ExitCode {
    if args.minimal_libs {
        eprintln!(
            "tilo-launcher: --minimal-libs is deprecated. \
             Prefer a manifest `runtime = \"tilo-runtime/<v>\"` declaration \
             plus `tilo-install runtime install --version <v> --from-host`."
        );
    }
    let opts = RunOptions {
        expected_package_hash: args.expected_package_hash,
        no_services: args.no_services,
        runtime_root: args.runtime,
        bwrap_bin: None,
        sandbox_shim_bin: None,
        minimal_libs: args.minimal_libs,
        landlock_enforce: true,
        landlock_best_effort: launcher::provider_sandbox::landlock_best_effort(&args.payload_root),
        mode: if args.service {
            LaunchMode::Service {
                passthrough_argv: args.rest,
            }
        } else {
            LaunchMode::Gui
        },
        allow_missing_runtime: args.allow_missing_runtime,
        allow_unsigned_runtime: args.allow_unsigned_runtime,
        require_runtime_publisher: args.require_runtime_publisher,
        identity: args
            .identity
            .map_or(IdentitySource::UserStore, IdentitySource::Record),
        data_home: None,
        wayland_socket: args.wayland_socket,
        workspace_id: args.workspace_id,
        capabilities_path: args.capabilities,
        trusted_identities_host_path: args.trusted_identities,
        action_authority_host_path: args.action_authority,
        app_id: args.app_id,
        render_node: args.render_node,
        open_document: args.open,
        open_uri: args.open_uri,
    };
    if let Some(app_id) = opts.app_id.as_deref() {
        enter_app_scope(app_id);
    }
    match run(&args.manifest, &args.payload_root, &opts) {
        Ok(status) => {
            // How the app ended, said where its output goes: a signal leaves
            // no exit code of its own, and is folded into 1 below.
            if !status.success() {
                eprintln!(
                    "tilo-launcher: {} ended: {status}",
                    opts.app_id.as_deref().unwrap_or("the app")
                );
            }
            if let Some(code) = status.code() {
                exit_code(code)
            } else {
                ExitCode::from(1)
            }
        }
        Err(e) => {
            eprintln!("tilo-launcher: {e}");
            exit_code(e.to_exit_code())
        }
    }
}

/// Move this launcher into the app's own systemd user scope before it forks
/// the sandbox, so the app's memory, OOM handling and lifetime are its own
/// and never the compositor's (see [`launcher::scope`]). The compositor
/// always names the app; without a reachable user manager the app still
/// starts, where the launcher already runs.
fn enter_app_scope(app_id: &str) {
    if let Err(error) = launcher::scope::join(app_id, app_id) {
        eprintln!("tilo-launcher: {app_id} runs outside a session scope: {error}");
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
                    "so.tilo.file-explorer",
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
                eprintln!("tilo-launcher: {error}");
                ExitCode::from(2)
            }
        },
        Err(error) => {
            eprintln!("tilo-launcher: {error}");
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

    let socket = launcher::services::socket_for("tilo-fs");
    let mut stream = UnixStream::connect(&socket)
        .map_err(|_| "received-file service is unavailable".to_string())?;
    request_received_mount(uri, &mut stream)
}

fn request_received_mount(
    uri: String,
    stream: &mut (impl std::io::Read + std::io::Write),
) -> Result<PathBuf, String> {
    let request = fs_protocol::RecipientControlRequestV1 {
        protocol_version: fs_protocol::RECIPIENT_CONTROL_VERSION,
        operation: fs_protocol::RecipientControlOperationV1::AdmitV2 { uri },
    };
    let request = collab_types::canonical_bytes(&request)
        .map_err(|_| "could not encode received-file request".to_string())?;
    let framing = fs_protocol::RECIPIENT_CONTROL_FRAMING;
    let request = framing
        .encode(&request)
        .map_err(|_| "received-file request exceeds its bound".to_string())?;
    stream
        .write_all(&request)
        .map_err(|_| "received-file service is unavailable".to_string())?;
    let response = match framing.read(stream) {
        Ok(Some(response)) => response,
        Ok(None) => return Err("received-file service did not respond".into()),
        Err(wire_frame::FrameError::TooLarge(_) | wire_frame::FrameError::EmptyFrame) => {
            return Err("received-file service returned an oversized response".into());
        }
        Err(_) => return Err("received-file service response was truncated".into()),
    };
    let response: fs_protocol::RecipientControlResponseV1 =
        collab_types::canonical::from_canonical_bytes(&response)
            .map_err(|_| "received-file service returned an invalid response".to_string())?;
    let status = match response.result {
        fs_protocol::RecipientControlResultV1::Admitted { status, .. } => status,
        fs_protocol::RecipientControlResultV1::Error { code } => {
            return Err(format!("received-file service refused the link ({code:?})"));
        }
        fs_protocol::RecipientControlResultV1::Status(_) => {
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
        collab_types::Subject::app("so.tilo.file-explorer"),
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
    let entry = match package_store::find_installed(package) {
        Ok(Some(e)) => e,
        Ok(None) => {
            eprintln!(
                "tilo-launcher: package {package} is not installed. \
                 Run `tilo-install list` to see what is."
            );
            return ExitCode::from(2);
        }
        Err(e) => {
            eprintln!("tilo-launcher: {e}");
            return ExitCode::from(2);
        }
    };
    let manifest = match launcher::manifest::read_manifest(&entry.manifest_path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("tilo-launcher: manifest read: {e}");
            return ExitCode::from(2);
        }
    };
    if manifest.entrypoint.gui_bin().is_none() {
        eprintln!(
            "tilo-launcher: {package} declares no GUI entrypoint. \
             Use `tilo-launcher run-installed --package {package} --command <name>` \
             for a CLI package."
        );
        return ExitCode::from(2);
    }
    let opts = RunOptions {
        no_services: args.no_services,
        mode: LaunchMode::Gui,
        landlock_enforce: true,
        landlock_best_effort: launcher::provider_sandbox::landlock_best_effort(&entry.payload_root),
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
            eprintln!("tilo-launcher: {e}");
            exit_code(e.to_exit_code())
        }
    }
}

/// Launch a host application. Unconfined — see [`launcher::host`].
fn run_host(args: RunHostArgs) -> ExitCode {
    let RunHostArgs {
        desktop_id,
        desktop_file,
        wayland_socket,
        x11_display,
        x11_authority,
        open,
        open_url,
        dry_run,
        new_window,
    } = args;
    let env = LaunchEnv {
        wayland_socket,
        x11_display,
        x11_authority,
        activation_token: std::env::var("TILO_HOST_ACTIVATION_TOKEN").ok(),
        files: open
            .as_deref()
            .map(|path| vec![path.display().to_string()])
            .unwrap_or_default(),
        urls: open_url.into_iter().collect(),
        prefer_new_window: new_window,
    };
    let planned = match desktop_file.as_deref() {
        Some(path) => launcher::host::plan_host_file(path, &env),
        None => launcher::host::plan_host(&desktop_id, &env),
    };
    if dry_run {
        return match planned {
            Ok(plan) => {
                println!("desktop-id : {}", plan.desktop_id);
                println!("name       : {}", plan.display_name);
                println!("source     : {}", plan.source.label());
                println!("confined   : {}", plan.is_confined());
                println!("program    : {}", plan.program.display());
                println!("args       : {:?}", plan.args);
                println!("windows    : {:?}", plan.window_identities);
                for (k, v) in &plan.env {
                    if host_launch_env_is_secret(k) {
                        continue;
                    }
                    println!("env        : {k}={v}");
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("tilo-launcher: {e}");
                ExitCode::from(2)
            }
        };
    }
    match planned.and_then(|plan| launcher::host::spawn(&plan)) {
        Ok(status) => exit_code(status.code().unwrap_or(1)),
        Err(e) => {
            eprintln!("tilo-launcher: {e}");
            exit_code(e.to_exit_code())
        }
    }
}

/// Resolve an installed package via the package store's registry and
/// launch its CLI-flavour entrypoint under the sandbox.
fn run_installed(
    package: &str,
    command: &str,
    no_services: bool,
    minimal_libs: bool,
    rest: Vec<String>,
) -> ExitCode {
    let entry = match package_store::find_installed(package) {
        Ok(Some(e)) => e,
        Ok(None) => {
            eprintln!(
                "tilo-launcher: package {package} is not installed. Run `tilo-install list` to see what is."
            );
            return ExitCode::from(2);
        }
        Err(e) => {
            eprintln!("tilo-launcher: {e}");
            return ExitCode::from(2);
        }
    };

    // Read the manifest to check `[cli].commands` contains the requested
    // name. Duplicates work the launcher does for its own sandbox setup,
    // accepted for the clarity of a fail-fast error message.
    let manifest = match launcher::manifest::read_manifest(&entry.manifest_path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("tilo-launcher: manifest read: {e}");
            return ExitCode::from(2);
        }
    };
    if !manifest.cli_commands.iter().any(|c| c == command) {
        eprintln!(
            "tilo-launcher: {package} does not declare CLI command {command:?} — check its manifest's [cli] block."
        );
        return ExitCode::from(2);
    }
    if manifest.entrypoint.cli_bin().is_none() {
        eprintln!(
            "tilo-launcher: {package}'s entrypoint is not a CLI (type != cli / both). Add `type = \"cli\"` to the manifest."
        );
        return ExitCode::from(2);
    }

    // Installed tapp command shims are a production launch surface. Their
    // inherited environment is app/user input, not trusted launcher policy,
    // so it must never be able to turn off either confinement or runtime
    // verification. Tests and developer harnesses that intentionally need a
    // weaker policy call the library with an explicit `RunOptions` value.
    // CLI-flavour launches through the PATH shim default --minimal-libs
    // on: shim invocations have no place to pass launcher flags, and CLI
    // apps are dynamically-linked ELFs until tilo-runtime bundles land.
    let minimal_libs = minimal_libs
        || !matches!(
            std::env::var("TILO_LAUNCHER_NO_MINIMAL_LIBS").as_deref(),
            Ok("1" | "true")
        );
    let opts = RunOptions {
        no_services,
        expected_package_hash: None,
        runtime_root: None,
        bwrap_bin: None,
        sandbox_shim_bin: None,
        minimal_libs,
        landlock_enforce: true,
        landlock_best_effort: launcher::provider_sandbox::landlock_best_effort(&entry.payload_root),
        mode: LaunchMode::Cli {
            command: command.to_string(),
            passthrough_argv: rest,
        },
        allow_missing_runtime: false,
        allow_unsigned_runtime: false,
        require_runtime_publisher: std::env::var("TILO_LAUNCHER_REQUIRE_RUNTIME_PUBLISHER")
            .ok()
            .filter(|s| !s.is_empty()),
        identity: IdentitySource::UserStore,
        data_home: None,
        trusted_identities_host_path: None,
        action_authority_host_path: None,
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
            eprintln!("tilo-launcher: {e}");
            exit_code(e.to_exit_code())
        }
    }
}

/// One-shot activation values are focus capabilities, not useful launch-plan
/// diagnostics.  Keep them out of dry-run/error-adjacent terminal output.
fn host_launch_env_is_secret(key: &str) -> bool {
    matches!(key, "XDG_ACTIVATION_TOKEN" | "DESKTOP_STARTUP_ID")
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
#[allow(
    dead_code,
    reason = "the link-only helper keeps RunError reachable for derive-generated binary paths"
)]
fn _link_run_error(_: &RunError) {}

#[cfg(test)]
mod admit_share_tests {
    use super::*;
    use std::io::{Cursor, Read, Write};
    use std::os::fd::IntoRawFd;
    use std::os::unix::net::UnixStream;

    struct ScriptedStream {
        written: Vec<u8>,
        response: Cursor<Vec<u8>>,
    }

    #[test]
    fn share_admission_and_registration_hand_over_only_what_is_owed() {
        // Phase `registration_descriptor_releases_process_entry_exactly_once`.
        {
            let (mut release, wait) = UnixStream::pair().unwrap();
            let writer = std::thread::spawn(move || release.write_all(&[1]));
            wait_for_registration_fd(wait.into_raw_fd()).unwrap();
            writer.join().unwrap().unwrap();
        }
        // Phase `admission_sends_the_bearer_only_in_the_service_frame`.
        {
            let uri = "tilo-file://v2/payload#bearer-secret".to_string();
            let mut stream = response("/home/alice/Shared/record");
            let mount = request_received_mount(uri.clone(), &mut stream).unwrap();
            assert_eq!(mount, PathBuf::from("/home/alice/Shared/record"));
            // One frame: a big-endian `u32` length, then the canonical request.
            let body_len = u32::try_from(stream.written.len() - 4).unwrap();
            assert_eq!(stream.written[..4], body_len.to_be_bytes());
            let length = u32::from_be_bytes(stream.written[..4].try_into().unwrap()) as usize;
            let request: fs_protocol::RecipientControlRequestV1 =
                collab_types::canonical::from_canonical_bytes(&stream.written[4..4 + length])
                    .unwrap();
            assert!(matches!(
                request.operation,
                fs_protocol::RecipientControlOperationV1::AdmitV2 { uri: got } if got == uri
            ));
        }
        // Phase `admission_refuses_a_relative_mount_from_the_service`.
        {
            let mut stream = response("relative/path");
            assert!(request_received_mount("redacted".into(), &mut stream).is_err());
        }
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
        let response = fs_protocol::RecipientControlResponseV1 {
            protocol_version: fs_protocol::RECIPIENT_CONTROL_VERSION,
            result: fs_protocol::RecipientControlResultV1::Admitted {
                inserted: true,
                status: fs_protocol::RecipientSubscriptionStatusV1 {
                    key: fs_protocol::RecipientSubscriptionKeyV1 {
                        workspace_id: uuid::Uuid::nil(),
                        record_id: fs_protocol::FileRecordId(uuid::Uuid::from_u128(1)),
                    },
                    permission: fs_protocol::FilePermission::View,
                    last_success_at_ms: None,
                    last_error: None,
                    mount: Some(fs_protocol::RecipientMountStatusV1 {
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
    fn host_launch_dry_run_redacts_one_shot_activation_values() {
        assert!(host_launch_env_is_secret("XDG_ACTIVATION_TOKEN"));
        assert!(host_launch_env_is_secret("DESKTOP_STARTUP_ID"));
        assert!(!host_launch_env_is_secret("TILO_HOST_ACTIVATION_TOKEN"));
        assert!(!host_launch_env_is_secret("WAYLAND_DISPLAY"));
    }
}

#[cfg(test)]
mod action_authority_tests {
    use super::*;
    #[test]
    fn action_authority_cli_keeps_host_selected_path_and_room() {
        let parsed = Cli::try_parse_from([
            "tilo-launcher",
            "run",
            "--manifest",
            "/manifest.toml",
            "--payload-root",
            "/payload",
            "--workspace-id",
            "room-a",
            "--action-authority",
            "/private/current.json",
        ])
        .unwrap();
        let Cmd::Run {
            action_authority,
            workspace_id,
            ..
        } = parsed.cmd
        else {
            panic!("run subcommand did not parse");
        };
        assert_eq!(
            action_authority.action_authority,
            Some(PathBuf::from("/private/current.json"))
        );
        assert_eq!(workspace_id.as_deref(), Some("room-a"));
    }
}
