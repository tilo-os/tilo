//! The `tilo-launcher` command line: start an app, run its CLI entrypoint, or re-enter
//! the sandbox shim.

use std::os::fd::{FromRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use desktop_entry::launch::LaunchEnv;
use launcher::landlock_shim::{SANDBOX_EXEC_SUBCOMMAND, run_sandbox_exec};
use launcher::launch_hold::{LAUNCH_REGISTRATION_FD_ENV, LAUNCH_REGISTRATION_STDIN_ENV};
use launcher::run::{LaunchMode, LaunchOptions, launch_app, launch_manifest};

fn wait_for_registration_fd(
    fd: RawFd,
) -> std::io::Result<Option<launcher::retained_view::OrdinaryLaunchAdmission>> {
    if fd < 3 {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    let socket = unsafe { std::os::unix::net::UnixStream::from_raw_fd(fd) };
    launcher::retained_view::RetainedLaunchAdmission::receive_registration(
        socket,
        &launcher::owned_service::ServiceCancellation::default(),
    )
}

#[allow(
    clippy::disallowed_methods,
    reason = "runs first in main before any thread is spawned, and the internal handoff variable must be removed so launched apps never inherit it"
)]
fn wait_for_shell_registration()
-> Result<Option<launcher::retained_view::OrdinaryLaunchAdmission>, String> {
    let on_stdin = std::env::var_os(LAUNCH_REGISTRATION_STDIN_ENV).is_some();
    let raw_fd = std::env::var(LAUNCH_REGISTRATION_FD_ENV);
    // SAFETY: process startup is single-threaded; remove the internal handoff before any launched app
    // environment can inherit it.
    unsafe {
        std::env::remove_var(LAUNCH_REGISTRATION_STDIN_ENV);
        std::env::remove_var(LAUNCH_REGISTRATION_FD_ENV);
    }
    if on_stdin {
        return launcher::launch_hold::wait_on_stdin()
            .map(|()| None)
            .map_err(|error| format!("launch-registration handoff failed: {error}"));
    }
    // A host from before the stdin handoff names an inherited descriptor.
    let Ok(raw_fd) = raw_fd else {
        return Ok(None);
    };
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
#[allow(
    clippy::large_enum_variant,
    reason = "a command line parsed once per launch: boxing a variant would need \
              clap support for boxed subcommands it does not have"
)]
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
        /// Explicit runtime bundle path.
        #[arg(long)]
        runtime: Option<PathBuf>,
        /// Skip well-known service-socket resolution; required services warn instead of failing.
        #[arg(long)]
        no_services: bool,
        /// Bind the minimum host libs a dynamically-linked ELF needs.
        #[arg(long, hide = true)]
        minimal_libs: bool,
        /// Escape hatch: fall back to `--minimal-libs` when the manifest declares a runtime but no
        /// bundle is installed.
        #[arg(long)]
        allow_missing_runtime: bool,
        /// Accept an unsigned runtime bundle.
        #[arg(long)]
        allow_unsigned_runtime: bool,
        /// Require the runtime bundle's publisher to equal this Ed25519 pubkey (hex); by default any
        /// signed publisher is accepted.
        #[arg(long)]
        require_runtime_publisher: Option<String>,
        /// Host Wayland socket path the app should connect to.
        #[arg(long)]
        wayland_socket: Option<PathBuf>,
        /// Workspace (collaboration workspace) this launch belongs to, forwarded as `TILO_WORKSPACE_ID`
        /// — the cluster id CRDT deltas are addressed to, so two apps with the same value can
        /// collaborate.
        #[arg(long)]
        workspace_id: Option<String>,
        /// JSON file holding the capabilities (`Vec<CapOverride>`) the user granted, written by the
        /// compositor before spawning.
        #[arg(long)]
        capabilities: Option<PathBuf>,
        #[command(flatten)]
        file_locations: Box<FileLocationArgs>,
        #[command(flatten)]
        action_authority: Box<ActionAuthorityArgs>,
        /// App id the caller launched; must equal the manifest's package id.
        #[arg(long)]
        app_id: Option<String>,
        /// The DRM render node the compositor renders on.
        #[arg(long)]
        render_node: Option<PathBuf>,
        /// A document for the app to open on start, as `argv[1]`.
        #[arg(long)]
        open: Option<PathBuf>,
        /// A validated URI for the app to handle.
        #[arg(long, conflicts_with = "open")]
        open_uri: Option<String>,
        /// Open `--open-uri` in a window that browses as a visitor, without the person's sign-ins: the
        /// app is handed `runtime_portal::process_link::VISITOR_WINDOW_FLAG` after the address.
        #[arg(long, requires = "open_uri")]
        open_as_visitor: bool,
        /// Standard console in a private namespace for this shell instance.
        #[arg(long, conflicts_with_all = ["service", "wayland_socket"])]
        headless_console: Option<u64>,
        /// Launch this package's headless entrypoint as a desktop-managed service.
        #[arg(long, conflicts_with_all = ["wayland_socket", "open", "open_uri"])]
        service: bool,
        /// A tilolang app whose instance a tilolang host runs: the process is its window alone, with no
        /// collaboration connection (`LaunchOptions::tilolang_process`).
        #[arg(long, conflicts_with = "service")]
        tilolang_process: bool,
        /// A tilolang app's connector for the origin named (`imap`), not a window: the process the
        /// session runs for an app whose types hold an outside service's resources, with the socket its
        /// instances attach over (`LaunchOptions::tilolang_connector`).
        #[arg(long, conflicts_with_all = ["service", "tilolang_process", "wayland_socket", "open", "open_uri"])]
        tilolang_connector: Option<String>,
        /// Trusted service argv, forwarded verbatim after the entrypoint.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        rest: Vec<String>,
    },
    /// Launch a CLI command from an already-installed package.
    RunInstalled {
        /// Reverse-DNS package id, as printed by `tilo-install list`.
        #[arg(long, alias = "pkg")]
        package: String,
        /// Which command from `[cli].commands` was invoked, forwarded as `TILO_CLI_COMMAND`.
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
    Exec {
        /// Reverse-DNS package identifier.
        #[arg(long)]
        package: String,
        #[arg(long)]
        no_services: bool,
        /// Host Wayland socket path.
        #[arg(long)]
        wayland_socket: Option<PathBuf>,
        /// Collaboration workspace to launch into.
        #[arg(long)]
        workspace_id: Option<String>,
        /// A document for the app to open, as an absolute host path.
        #[arg(long)]
        open: Option<PathBuf>,
        #[arg(long, conflicts_with = "open")]
        open_uri: Option<String>,
        /// Fall back to `--minimal-libs` when the manifest declares a runtime bundle that is not
        /// installed.
        #[arg(long)]
        allow_missing_runtime: bool,
        /// Accept an unsigned runtime bundle.
        #[arg(long)]
        allow_unsigned_runtime: bool,
    },
    /// Launch an application Tilo did **not** package: a `.desktop` entry already installed on the
    /// host, Flatpak or otherwise.
    MachineApp(MachineAppArgs),
    /// Start a Windows program's sandbox, as the compatibility service planned it, behind an app
    /// keeper in a scope of its own: the session starts each such program through this, so the
    /// program stays open while the desktop restarts in place.
    #[command(name = "windows-program", hide = true)]
    WindowsProgram(WindowsProgramArgs),
    /// Start a Windows program's sandbox on the X11 display its keeper serves (`DISPLAY`,
    /// `XAUTHORITY`): the keeper picks the display only as it starts, after the sandbox was
    /// planned, so the sandbox's command starts with this, which adds the display to the sandbox
    /// and becomes it.
    #[command(name = "windows-x11-sandbox", hide = true)]
    WindowsX11Sandbox {
        /// The sandbox's command line.
        #[arg(last = true, required = true)]
        command: Vec<std::ffi::OsString>,
    },
    /// Start the private audio graph the compatibility service prepared for a Windows program,
    /// then become the program's sandbox: the graph's daemons end with the sandbox's process, so
    /// they live exactly as long as the program, in its launch's group (`launcher::audio_graph`).
    #[command(name = "windows-audio", hide = true)]
    WindowsAudio {
        /// The directory the graph was prepared in.
        #[arg(long)]
        graph: PathBuf,
        /// The sandbox's command line.
        #[arg(last = true, required = true)]
        command: Vec<std::ffi::OsString>,
    },
    /// Start the filtered network's proxy the compatibility service prepared for a Windows
    /// program, then become the program's sandbox: the proxy ends with the sandbox's process, so
    /// it lives exactly as long as the program, in its launch's group (`launcher::netproxy`).
    #[command(name = "windows-network", hide = true)]
    WindowsNetwork {
        /// The directory the policy was prepared in.
        #[arg(long)]
        dir: PathBuf,
        /// The control socket the session reaches the proxy on, in the keeper's directory when
        /// the program is kept.
        #[arg(long)]
        control: PathBuf,
        /// The sandbox's command line.
        #[arg(last = true, required = true)]
        command: Vec<std::ffi::OsString>,
    },
    /// Serve one launch's network proxy: the launch starts this as the sandbox's child, and it
    /// ends with the sandbox (`launcher::netproxy`).
    #[command(name = "windows-network-serve", hide = true)]
    WindowsNetworkServe {
        /// The directory the policy was prepared in.
        #[arg(long)]
        dir: PathBuf,
        /// The control socket the session reaches the proxy on.
        #[arg(long)]
        control: PathBuf,
    },
    /// Build or retire the pre-built read-only system view every sandbox mounts
    /// (`launcher::sandbox_view`).
    #[command(subcommand)]
    SandboxView(SandboxViewCmd),
    /// List the apps running in this person's session, each with its cgroup's processes, memory, CPU
    /// time, tier and OOM kills: one `app-tilo-<id>-<pid>.scope` per launch in `app.slice`.
    Apps {
        /// Print JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Map and lock the shared runtime's libraries of every runtime bundle the system registry names,
    /// then sleep until stopped (`launcher::runtime_resident`).
    RuntimeResident {
        /// The registry whose entries name the runtimes to keep.
        #[arg(long, default_value_t = default_system_registry())]
        registry: String,
        /// Where the runtime bundles are installed.
        #[arg(long, default_value = launcher::runtime_resident::SYSTEM_RUNTIME_ROOT)]
        runtime_root: PathBuf,
        /// Keep the runtimes of this person's own store instead: those their registry names and the
        /// system does not hold, verified as a launch verifies them, each library only when its signed
        /// fs-verity digest is measured on the open file.
        #[arg(long, conflicts_with_all = ["registry", "runtime_root"])]
        user: bool,
    },
    /// Run a tilolang host: the launch kind that runs one author's tilolang apps in one workspace as
    /// instances, in a sandbox holding only the verified tilolang runtime and the Tilo runtime it runs
    /// on, its storage and its socket (`launcher::run::tilolang_process`).
    #[command(name = "tilolang-process", hide = true)]
    TilolangProcess {
        /// The tilolang runtime, `tilolang/<version>`.
        #[arg(long)]
        runtime: String,
        /// The Tilo runtime its apps name, `tilo-runtime/<version>`: the loader and libraries the
        /// host's binary runs on.
        #[arg(long)]
        app_runtime: String,
        /// The storage of the process's workspace and author.
        #[arg(long)]
        storage: PathBuf,
        /// What an earlier version of its app kept on this device, granted read-only: that app's
        /// documents in the process's workspace.
        #[arg(long, requires = "earlier_under")]
        earlier: Option<PathBuf>,
        /// Where the process sees `--earlier`, as a relative path beneath its mount.
        #[arg(long, requires = "earlier")]
        earlier_under: Option<PathBuf>,
    },
}

fn default_system_registry() -> String {
    package_store::Layout::new(package_store::layout::SYSTEM_DATA_DIR)
        .registry()
        .to_string_lossy()
        .into_owned()
}

#[derive(Debug, Subcommand)]
enum SandboxViewCmd {
    /// Build a new generation, make it current and retire the old ones.
    Build {
        /// Where the views live.
        #[arg(long, default_value = launcher::sandbox_view::DEFAULT_ROOT)]
        root: PathBuf,
        /// Hand each superseded view a running sandbox still holds to
        /// `tilo-sandbox-view-retire@<generation>.service`.
        #[arg(long)]
        retire_with_systemd: bool,
    },
    /// Unmount a superseded generation once no sandbox holds it.
    Retire {
        /// Where the views live.
        #[arg(long, default_value = launcher::sandbox_view::DEFAULT_ROOT)]
        root: PathBuf,
        /// The generation, `g<id>`.
        generation: String,
    },
}

#[derive(Debug, clap::Args)]
struct MachineAppArgs {
    /// Desktop-file id: the entry's file name without `.desktop` (`com.spotify.Client`,
    /// `org.gnome.TextEditor`).
    #[arg(long)]
    desktop_id: String,
    /// Run this desktop file instead of looking `--desktop-id` up in the `applications` directories —
    /// how XDG autostart entries, which live in `autostart` directories, are started.
    #[arg(long)]
    desktop_file: Option<PathBuf>,
    /// Run this program file itself, with no desktop entry: a downloaded `AppImage` or executable
    /// opened in Files.
    #[arg(long, conflicts_with = "desktop_file")]
    program: Option<PathBuf>,
    /// Host path of the compositor's Wayland socket.
    #[arg(long)]
    wayland_socket: Option<PathBuf>,
    /// The X11 display of the workspace this app is launched into, supplied by the compositor for
    /// legacy X11 applications.
    #[arg(long)]
    x11_display: Option<String>,
    /// A file to open.
    #[arg(long)]
    open: Option<PathBuf>,
    /// A URL for the application to open.
    #[arg(long)]
    open_url: Option<String>,
    /// Print the resolved argv and environment and exit without running anything.
    #[arg(long)]
    dry_run: bool,
    /// Prefer the desktop entry's declared New Window action.
    #[arg(long)]
    new_window: bool,
    /// Start the app behind the app keeper at this path, so its windows stay open while the
    /// desktop restarts in place: the keeper runs in the app's scope and the app descends from it.
    #[arg(long, requires_all = ["keep_in", "keep_as"])]
    keeper: Option<PathBuf>,
    /// The keeper's private directory (`--dir` of the keeper).
    #[arg(long, requires = "keeper")]
    keep_in: Option<PathBuf>,
    /// What the session names this launch, handed to the keeper as given (`--launch`).
    #[arg(long, requires = "keeper")]
    keep_as: Option<String>,
}

#[derive(clap::Args, Debug)]
struct WindowsProgramArgs {
    /// The app id the program's scope and windows are named for.
    #[arg(long)]
    app_id: String,
    /// The name the program is shown by.
    #[arg(long)]
    name: String,
    /// Serve the program an X11 display of its keeper's.
    #[arg(long)]
    x11: bool,
    /// The app keeper the program starts behind.
    #[arg(long)]
    keeper: PathBuf,
    /// The keeper's private directory (`--dir` of the keeper).
    #[arg(long)]
    keep_in: PathBuf,
    /// What the session names this launch, handed to the keeper as given (`--launch`).
    #[arg(long)]
    keep_as: String,
    /// Print the keeper's arguments and exit without starting anything.
    #[arg(long)]
    print_keeper: bool,
    /// The sandbox's command line.
    #[arg(last = true, required = true)]
    command: Vec<std::ffi::OsString>,
}

/// Start a Windows program behind its keeper, and end as it ended.
fn windows_program(args: &WindowsProgramArgs) -> ExitCode {
    let keep = launcher::host::Keep {
        keeper: args.keeper.clone(),
        dir: args.keep_in.clone(),
        launch: args.keep_as.clone(),
    };
    if args.print_keeper {
        println!("keeper : {}", args.keeper.display());
        for arg in launcher::host::keeper_args(&keep, args.x11, &args.command) {
            println!("\t{}", arg.to_string_lossy());
        }
        return ExitCode::SUCCESS;
    }
    match launcher::host::start_kept(&args.app_id, &args.name, &args.command, args.x11, &keep) {
        Ok(status) => {
            if let Some(signal) = std::os::unix::process::ExitStatusExt::signal(&status) {
                launcher::host::end_by(signal);
            }
            exit_code(status.code().unwrap_or(1))
        }
        Err(e) => {
            eprintln!("tilo-launcher: {e}");
            exit_code(e.to_exit_code())
        }
    }
}

/// Become the sandbox `command` with the keeper's X11 display added to it.
fn windows_x11_sandbox(mut command: Vec<std::ffi::OsString>) -> ExitCode {
    use std::os::unix::process::CommandExt as _;
    let (Some(display), Some(authority)) =
        (std::env::var_os("DISPLAY"), std::env::var_os("XAUTHORITY"))
    else {
        eprintln!("tilo-launcher: the keeper serves this program no X11 display");
        return ExitCode::FAILURE;
    };
    if let Err(error) = launcher::sandbox::give_x11_display(
        &mut command,
        &display.to_string_lossy(),
        Path::new(&authority),
    ) {
        eprintln!("tilo-launcher: {error}");
        return ExitCode::FAILURE;
    }
    let Some((program, args)) = command.split_first() else {
        return ExitCode::FAILURE;
    };
    // The descriptors the sandbox is handed stay open across the exec, as they did into this.
    let error = std::process::Command::new(program).args(args).exec();
    eprintln!("tilo-launcher: {}: {error}", program.to_string_lossy());
    ExitCode::FAILURE
}

/// Fold an `i32` exit code (from [`std::process::ExitStatus::code`] or
/// [`launcher::run::LaunchError::to_exit_code`]) into the `u8` [`ExitCode`] takes.
fn exit_code(code: i32) -> ExitCode {
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

/// How many descriptors this process's table holds from the start.
const DESCRIPTOR_TABLE_SIZE: libc::c_int = 256;

/// Grow the descriptor table to its working size while this process still has one thread.
fn presize_descriptor_table() {
    // SAFETY: duplicates stderr to the lowest free descriptor at or above the table's last slot,
    // close-on-exec, and closes that copy at once.
    unsafe {
        let copy = libc::fcntl(2, libc::F_DUPFD_CLOEXEC, DESCRIPTOR_TABLE_SIZE - 1);
        if copy >= 0 {
            libc::close(copy);
        }
    }
}

fn main() -> ExitCode {
    presize_descriptor_table();
    tilo_startup_metrics::handoff_mark("launcher-start");
    let retained_endpoint = match wait_for_shell_registration() {
        Ok(endpoint) => endpoint,
        Err(error) => {
            eprintln!("tilo-launcher: {error}");
            return ExitCode::from(126);
        }
    };
    tilo_startup_metrics::handoff_mark("shell-registered");
    // Hidden subcommand: parsed BEFORE clap so its argv shape (positional paths, `--` separator)
    // doesn't collide with clap's flag parsing.
    let raw_args: Vec<String> = std::env::args().collect();
    if raw_args.get(1).map(std::string::String::as_str) == Some(SANDBOX_EXEC_SUBCOMMAND) {
        if retained_endpoint.is_some() {
            return ExitCode::from(126);
        }
        run_sandbox_exec(raw_args.get(2..).unwrap_or(&[]));
    }

    init_tracing();
    let cli = Cli::parse();
    if retained_endpoint.is_some() && !matches!(&cli.cmd, Cmd::Run { .. }) {
        return ExitCode::from(126);
    }
    dispatch_command(cli.cmd, retained_endpoint)
}

/// Whether `command` runs an app or is about to start one: one of Tilo's, one of the machine's
/// own, or a Windows program. Left under the session's protection, a Windows program could not be
/// ended for the memory it took: over its limit it hung, and a machine out of memory lost
/// something else first.
const fn starts_an_app(command: &Cmd) -> bool {
    matches!(
        command,
        Cmd::Run { .. }
            | Cmd::RunInstalled { .. }
            | Cmd::Exec { .. }
            | Cmd::MachineApp(_)
            | Cmd::WindowsProgram(_)
    )
}

fn dispatch_command(
    command: Cmd,
    retained_endpoint: Option<launcher::retained_view::OrdinaryLaunchAdmission>,
) -> ExitCode {
    // Whatever runs an app, or is about to start one, leaves the session's protection from the OOM
    // killer, which it may have inherited from the compositor: an app is always a candidate.
    if starts_an_app(&command) {
        launcher::priority::leave_session_protection();
    }
    match command {
        command @ Cmd::Run { .. } => run_cli_args(command).map_or(ExitCode::FAILURE, |args| {
            run_gui_command(args, retained_endpoint)
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
        Cmd::MachineApp(args) => machine_app(args),
        Cmd::WindowsProgram(args) => windows_program(&args),
        Cmd::WindowsX11Sandbox { command } => windows_x11_sandbox(command),
        Cmd::WindowsAudio { graph, command } => {
            let Err(error) = launcher::audio_graph::exec_with(
                graph,
                &launcher::audio_graph::Daemons::installed(),
                &command,
            );
            eprintln!("tilo-launcher: {error}");
            ExitCode::FAILURE
        }
        Cmd::WindowsNetwork {
            dir,
            control,
            command,
        } => {
            let Err(error) = launcher::netproxy::exec_with(&dir, &control, &command);
            eprintln!("tilo-launcher: {error}");
            ExitCode::FAILURE
        }
        Cmd::WindowsNetworkServe { dir, control } => {
            let Err(error) = launcher::netproxy::serve(&dir, &control);
            eprintln!("tilo-launcher: {error}");
            ExitCode::FAILURE
        }
        Cmd::SandboxView(command) => sandbox_view(command),
        Cmd::Apps { json } => list_apps(json),
        Cmd::RuntimeResident { user: true, .. } => runtime_resident_user(),
        Cmd::RuntimeResident {
            registry,
            runtime_root,
            user: false,
        } => runtime_resident(Path::new(&registry), &runtime_root),
        Cmd::TilolangProcess {
            runtime,
            app_runtime,
            storage,
            earlier,
            earlier_under,
        } => tilolang_process(
            runtime,
            app_runtime,
            storage,
            earlier
                .zip(earlier_under)
                .map(|(dir, under)| launcher::run::tilolang_process::Earlier { dir, under }),
        ),
    }
}

/// Run the tilolang host the session started, on the link it handed over.
fn tilolang_process(
    runtime: String,
    app_runtime: String,
    storage: PathBuf,
    earlier: Option<launcher::run::tilolang_process::Earlier>,
) -> ExitCode {
    use launcher::run::tilolang_process::{ProcessLaunch, launch_tilolang_process};
    let Some(socket) = runtime_portal::process_link::take() else {
        eprintln!("tilo-launcher: the tilolang host was handed no link");
        return ExitCode::FAILURE;
    };
    let launch = ProcessLaunch {
        runtime,
        app_runtime,
        storage,
        earlier,
    };
    match launch_tilolang_process(&launch, socket, &launcher::run::LaunchOptions::default()) {
        Ok(status) => ExitCode::from(u8::try_from(status.code().unwrap_or(1)).unwrap_or(1)),
        Err(error) => {
            eprintln!("tilo-launcher: the tilolang host did not start: {error}");
            ExitCode::FAILURE
        }
    }
}

/// The arguments of a `run` command; `None` for any other command.
fn run_cli_args(command: Cmd) -> Option<RunCliArgs> {
    let Cmd::Run {
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
        file_locations,
        action_authority,
        app_id,
        render_node,
        open,
        open_uri,
        open_as_visitor,
        headless_console,
        service,
        tilolang_process,
        tilolang_connector,
        rest,
    } = command
    else {
        return None;
    };
    Some(RunCliArgs {
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
        trusted_identities: file_locations.trusted_identities,
        retained_files: file_locations.retained_files,
        browser_continuity: file_locations.browser_continuity,
        action_authority: action_authority.action_authority,
        app_id,
        render_node,
        open,
        open_uri,
        open_as_visitor,
        headless_console,
        service,
        tilolang_process,
        tilolang_connector,
        rest,
    })
}

/// Keep the libraries of this person's own runtimes resident until SIGTERM
/// (`launcher::runtime_resident`, "A runtime a person installed").
fn runtime_resident_user() -> ExitCode {
    use launcher::runtime_resident as resident;
    let (Some(registry), Some(user_root)) =
        (resident::user_registry(), resident::user_runtime_root())
    else {
        eprintln!("tilo-launcher: this session has no data directory to find its runtimes in");
        return ExitCode::FAILURE;
    };
    let registry = match package_store::Registry::load(&registry) {
        Ok(registry) => registry,
        Err(error) => {
            eprintln!("tilo-launcher: {error}");
            return ExitCode::FAILURE;
        }
    };
    let versions = resident::user_kept_versions(
        &resident::registered_versions(&registry),
        Path::new(resident::SYSTEM_RUNTIME_ROOT),
    );
    let (files, problems) = resident::user_resident_files(
        &versions,
        &user_root,
        |bundle| launcher::run::verified_user_bundle(bundle).map_err(|error| error.to_string()),
        resident::verity_vouches,
    );
    for problem in &problems {
        eprintln!("tilo-launcher: {problem}");
    }
    let mut mapped = Vec::new();
    for file in &files {
        match resident::map_open(&file.path, &file.file, resident::Lock::IfAllowed) {
            Ok(resident_file) => mapped.push(resident_file),
            Err(error) => eprintln!(
                "tilo-launcher: keeping {} resident: {error}",
                file.path.display()
            ),
        }
    }
    // The mappings hold the files; the descriptors are no longer needed.
    drop(files);
    let bytes: usize = mapped.iter().map(resident::ResidentFile::len).sum();
    let locked: usize = mapped
        .iter()
        .filter(|file| file.is_locked())
        .map(resident::ResidentFile::len)
        .sum();
    println!(
        "{} libraries of your runtimes resident, {} MiB ({} MiB locked)",
        mapped.len(),
        bytes / (1024 * 1024),
        locked / (1024 * 1024)
    );
    if mapped.is_empty() {
        // Nothing of this person's to keep; the path unit starts this again when their registry
        // changes.
        return ExitCode::SUCCESS;
    }
    loop {
        // SAFETY: pause has no preconditions; it returns only after a handled signal, and this process
        // handles none.
        unsafe { libc::pause() };
    }
}

/// Keep the registered runtimes' libraries resident until SIGTERM.
fn runtime_resident(registry: &Path, runtime_root: &Path) -> ExitCode {
    use launcher::runtime_resident as resident;
    let registry = match package_store::Registry::load(registry) {
        Ok(registry) => registry,
        Err(error) => {
            eprintln!("tilo-launcher: {error}");
            return ExitCode::FAILURE;
        }
    };
    let versions = resident::registered_versions(&registry);
    let files = match resident::resident_files(&versions, runtime_root) {
        Ok(files) => files,
        Err(error) => {
            eprintln!("tilo-launcher: reading the runtime libraries: {error}");
            return ExitCode::FAILURE;
        }
    };
    let (mapped, failed) = resident::map_all(&files);
    for (path, error) in &failed {
        eprintln!(
            "tilo-launcher: keeping {} resident: {error}",
            path.display()
        );
    }
    let bytes: usize = mapped.iter().map(resident::ResidentFile::len).sum();
    println!(
        "{} runtime libraries of {} runtime(s) resident, {} MiB",
        mapped.len(),
        versions.len(),
        bytes / (1024 * 1024)
    );
    if mapped.is_empty() && !failed.is_empty() {
        return ExitCode::FAILURE;
    }
    // The mappings are the whole job: hold them until systemd stops us, whose SIGTERM ends the process
    // with its default action.
    loop {
        // SAFETY: pause has no preconditions; it returns only after a handled signal, and this process
        // handles none.
        unsafe { libc::pause() };
    }
}

fn sandbox_view(command: SandboxViewCmd) -> ExitCode {
    match command {
        SandboxViewCmd::Build {
            root,
            retire_with_systemd,
        } => match launcher::sandbox_view::build(&root) {
            Ok(built) => {
                println!("{}", built.generation);
                for generation in built.busy {
                    if !retire_with_systemd {
                        println!("still in use: {generation}");
                        continue;
                    }
                    let unit = format!("tilo-sandbox-view-retire@{generation}.service");
                    let started = std::process::Command::new("systemctl")
                        .args(["--no-block", "start", &unit])
                        .status();
                    if !started.is_ok_and(|status| status.success()) {
                        eprintln!("tilo-launcher: could not start {unit}");
                    }
                }
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("tilo-launcher: building the sandbox view: {error}");
                ExitCode::FAILURE
            }
        },
        SandboxViewCmd::Retire { root, generation } => {
            match launcher::sandbox_view::retire(&root, &generation) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("tilo-launcher: retiring sandbox view {generation}: {error}");
                    ExitCode::FAILURE
                }
            }
        }
    }
}

fn list_apps(json: bool) -> ExitCode {
    let uid = rustix::process::getuid().as_raw();
    let apps = launcher::app_cgroup::running_apps(&launcher::app_cgroup::user_app_slice(uid));
    if json {
        match serde_json::to_string_pretty(&apps) {
            Ok(text) => println!("{text}"),
            Err(error) => {
                eprintln!("tilo-launcher: {error}");
                return ExitCode::from(1);
            }
        }
    } else {
        print!("{}", launcher::app_cgroup::format_table(&apps));
    }
    ExitCode::SUCCESS
}

#[derive(Debug, clap::Args)]
struct FileLocationArgs {
    /// Host-staged governance identity projection; read-only and Files only.
    #[arg(long)]
    trusted_identities: Option<PathBuf>,
    /// Device-local retained locations; metadata does not grant file access.
    #[arg(long)]
    retained_files: Option<PathBuf>,
    /// Verified browser migration subset; read-only and Browser GUI only.
    #[arg(long)]
    browser_continuity: Option<PathBuf>,
}

#[derive(Debug, clap::Args)]
struct ActionAuthorityArgs {
    /// Host-staged, app-scoped action authority; mounted read-only.
    #[arg(long)]
    action_authority: Option<PathBuf>,
}

/// Every field of [`Cmd::Run`], bundled so [`run_gui_command`] doesn't count against clippy's argument
/// limit — this is argv plumbing, not a sandbox policy struct (see [`ExecArgs`] below for the same
/// pattern).
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
    retained_files: Option<PathBuf>,
    browser_continuity: Option<PathBuf>,
    action_authority: Option<PathBuf>,
    app_id: Option<String>,
    render_node: Option<PathBuf>,
    open: Option<PathBuf>,
    open_uri: Option<String>,
    open_as_visitor: bool,
    headless_console: Option<u64>,
    service: bool,
    tilolang_process: bool,
    tilolang_connector: Option<String>,
    rest: Vec<String>,
}

/// Build [`LaunchOptions`] from the `run` subcommand's flags and launch either the package's GUI
/// entrypoint or its explicitly requested managed-service entrypoint.
fn run_gui_command(
    args: RunCliArgs,
    retained_endpoint: Option<launcher::retained_view::OrdinaryLaunchAdmission>,
) -> ExitCode {
    warn_deprecated_minimal_libs(args.minimal_libs);
    let opts = LaunchOptions {
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
                passthrough_argv: args.rest.clone(),
            }
        } else {
            LaunchMode::Gui
        },
        // A GUI launch's `--` tail is the app's own argv, exactly as a CLI launch's is: the app's
        // parser decides what it means.
        gui_argv: if args.service { Vec::new() } else { args.rest },
        allow_missing_runtime: args.allow_missing_runtime,
        allow_unsigned_runtime: args.allow_unsigned_runtime,
        require_runtime_publisher: args.require_runtime_publisher,
        data_home: None,
        wayland_socket: args.wayland_socket,
        workspace_id: args.workspace_id,
        capabilities_path: args.capabilities,
        trusted_identities_host_path: args.trusted_identities,
        retained_files_host_path: args.retained_files,
        browser_continuity_host_path: args.browser_continuity,
        action_authority_host_path: args.action_authority,
        app_id: args.app_id,
        render_node: args.render_node,
        open_document: args.open,
        open_uri: args.open_uri,
        open_as_visitor: args.open_as_visitor,
        headless_console: args.headless_console,
        tilolang_process: args.tilolang_process,
        tilolang_connector: args.tilolang_connector,
    };
    // Held from before the package is verified until the app exits, so an update meanwhile leaves the
    // build this launch runs alone (`launcher::lease`).
    let _payload_held = match launcher::lease::hold_payload(&args.payload_root) {
        Ok(held) => held,
        Err(e) => {
            eprintln!("tilo-launcher: {e}");
            return exit_code(e.to_exit_code());
        }
    };
    let manifest = verify_in_app_cgroup(&args.manifest, &args.payload_root, &opts);
    let result = manifest.and_then(|manifest| {
        if let Some(endpoint) = retained_endpoint {
            let cancellation = launcher::owned_service::ServiceCancellation::default();
            match endpoint {
                launcher::retained_view::OrdinaryLaunchAdmission::Home(endpoint) => {
                    launcher::retained_view::RetainedViewAdmission::receive(
                        endpoint,
                        &args.manifest,
                        &args.payload_root,
                        &opts,
                        &cancellation,
                    )
                    .and_then(|admission| {
                        launcher::retained_view::run_admitted_gui(admission, cancellation)
                    })
                }
            }
        } else {
            launch_app(&manifest, &args.payload_root, &opts)
        }
    });
    report_launch_result(result, opts.app_id.as_deref())
}

fn warn_deprecated_minimal_libs(minimal_libs: bool) {
    if minimal_libs {
        eprintln!(
            "tilo-launcher: --minimal-libs is deprecated. \
             Prefer a manifest `runtime = \"tilo-runtime/<v>\"` declaration \
             plus `tilo-install runtime install --version <v> --from-host`."
        );
    }
}

fn report_launch_result(
    result: Result<std::process::ExitStatus, launcher::run::LaunchError>,
    app_id: Option<&str>,
) -> ExitCode {
    match result {
        Ok(status) => {
            // How the app ended, said where its output goes: a signal leaves no exit code of its own,
            // and is folded into 1 below.
            if !status.success() {
                // A sandbox this computer cannot make ends every app the same way: said by its
                // own exit code, so whoever started the app names the cause. The cause and how to
                // fix it come first, how the app ended last.
                let refusal = launcher::user_namespaces::refusal().ok().flatten();
                if let Some(refusal) = &refusal {
                    for line in refusal.to_string().lines() {
                        eprintln!("{}{line}", launcher::SAYS);
                    }
                }
                eprintln!(
                    "{}{} ended: {status}",
                    launcher::SAYS,
                    app_id.unwrap_or("the app")
                );
                if refusal.is_some() {
                    return exit_code(launcher::user_namespaces::EXIT_NO_USER_NAMESPACES);
                }
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

/// Verify the package, with this launcher in the app's own cgroup by the time it returns
/// ([`enter_app_scope`]).
fn verify_in_app_cgroup(
    manifest: &std::path::Path,
    payload_root: &std::path::Path,
    opts: &LaunchOptions,
) -> Result<launcher::manifest::ManifestView, launcher::run::LaunchError> {
    // A launcher already inside an app's cgroup (an app running another program of its own) has
    // nothing to join.
    let born_in_cgroup = launcher::app_cgroup::current_is_app_cgroup();
    if born_in_cgroup {
        tilo_startup_metrics::handoff_mark("scope-joined");
    }
    // Otherwise joining the app's scope (a round trip to the user manager) and verifying the package
    // (reading and hashing its files) need nothing from each other, so they run side by side.
    std::thread::scope(|scope| {
        let joining = opts
            .app_id
            .as_deref()
            .filter(|_| !born_in_cgroup)
            .map(|app_id| {
                std::thread::Builder::new()
                    .name("launch-scope".into())
                    .spawn_scoped(scope, move || enter_app_scope(app_id))
                    .map_err(|_| app_id)
            });
        let manifest = launch_manifest(manifest, payload_root, opts);
        match joining {
            Some(Ok(handle)) => {
                if let Err(panic) = handle.join() {
                    std::panic::resume_unwind(panic);
                }
            }
            // No thread to spare: join here, as before.
            Some(Err(app_id)) => enter_app_scope(app_id),
            None => {}
        }
        manifest
    })
}

/// Make sure this launcher is in the app's own cgroup before it forks the sandbox, so the app's memory,
/// OOM handling and lifetime are its own and never the compositor's (see [`launcher::scope`]).
///
/// A session that runs inside another desktop owns no machine of its own
/// ([`tilo_paths::machine`]): the user manager it would ask is that desktop's, and a launch of this
/// session is not a unit of its login. The app starts unscoped rather than joining the host's
/// session target, which would end it — and everything else in that target — when the host's
/// desktop does.
fn enter_app_scope(app_id: &str) {
    if !tilo_paths::machine::owns_the_machine() {
        tilo_startup_metrics::handoff_mark("scope-skipped");
        return;
    }
    if let Err(error) = launcher::scope::join(app_id, app_id, launcher::scope::Oom::WholeApp) {
        eprintln!("tilo-launcher: {app_id} runs outside a session scope: {error}");
        return;
    }
    tilo_startup_metrics::handoff_mark("scope-joined");
}

/// CLI arguments for [`Cmd::Exec`], grouped to keep [`exec_installed`] under clippy's argument-count
/// limit — this is argv plumbing, not a sandbox policy struct.
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
const LAUNCH_LATER: u8 = 75;

/// The installed `package`, held for the launch, at a build this system runs.
fn find_current(
    package: &str,
) -> Result<
    (
        package_store::InstalledPackage,
        Option<advisory_lock::FileLock>,
    ),
    ExitCode,
> {
    let found = find_held(package)?;
    let provided = package_store::interfaces::system_interfaces();
    let unmet = package_store::interfaces::unmet_at(provided.as_ref(), &found.0.manifest_path);
    if unmet.is_empty() {
        return Ok(found);
    }
    let describe = tilo_app_package::interfaces::describe_unmet(&unmet);
    if unmet
        .iter()
        .any(|unmet| unmet.behind() != tilo_app_package::interfaces::Behind::Package)
    {
        eprintln!(
            "tilo-launcher: {package} needs a newer Tilo ({describe}). \
             Tilo is updating now; run it again once the update has applied."
        );
        start_system_update();
        return Err(ExitCode::from(LAUNCH_LATER));
    }
    eprintln!("tilo-launcher: updating {package} to a build for this Tilo ({describe})…");
    drop(found);
    if !update_package(package) {
        eprintln!(
            "tilo-launcher: {package} could not be updated now; run it again once Tilo can reach the store."
        );
        return Err(ExitCode::from(LAUNCH_LATER));
    }
    let found = find_held(package)?;
    let unmet = package_store::interfaces::unmet_at(provided.as_ref(), &found.0.manifest_path);
    if unmet.is_empty() {
        return Ok(found);
    }
    eprintln!(
        "tilo-launcher: waiting for an update from {package}'s developer: no build fits this Tilo yet ({}).",
        tilo_app_package::interfaces::describe_unmet(&unmet)
    );
    Err(ExitCode::from(LAUNCH_LATER))
}

/// The installed `package`, held for the launch.
fn find_held(
    package: &str,
) -> Result<
    (
        package_store::InstalledPackage,
        Option<advisory_lock::FileLock>,
    ),
    ExitCode,
> {
    match launcher::lease::find_installed_held(package) {
        Ok(Some(found)) => Ok(found),
        Ok(None) => {
            eprintln!(
                "tilo-launcher: package {package} is not installed. \
                 Run `tilo-install list` to see what is."
            );
            Err(ExitCode::from(2))
        }
        Err(e) => {
            eprintln!("tilo-launcher: {e}");
            Err(ExitCode::from(2))
        }
    }
}

/// `app update <package>`, as the shell runs it; whether it succeeded.
fn update_package(package: &str) -> bool {
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("app")));
    let Some(app) = beside
        .into_iter()
        .chain([
            PathBuf::from("/usr/libexec/app"),
            PathBuf::from("/usr/bin/app"),
        ])
        .find(|path| path.exists())
    else {
        return false;
    };
    std::process::Command::new(app)
        .args(["update", package])
        .stdin(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Start the system's update without waiting for it, as the shell does when an app needs a newer system
/// (`tilo-deployment-update.service`).
fn start_system_update() {
    let _ = std::process::Command::new("systemctl")
        .args([
            "start",
            "--no-block",
            "--no-ask-password",
            "tilo-deployment-update.service",
        ])
        .stdin(std::process::Stdio::null())
        .status();
}

fn exec_installed(package: &str, args: ExecArgs) -> ExitCode {
    // Held from the lookup until the app exits, so an update meanwhile leaves the build this launch
    // runs alone (`launcher::lease`).
    let (entry, _payload_held) = match find_current(package) {
        Ok(found) => found,
        Err(code) => return code,
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
    let opts = LaunchOptions {
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
    match launcher::run::launch_with(&manifest, &entry.payload_root, &opts) {
        Ok(status) => exit_code(status.code().unwrap_or(1)),
        Err(e) => {
            eprintln!("tilo-launcher: {e}");
            exit_code(e.to_exit_code())
        }
    }
}

/// Launch a host application.
fn machine_app(args: MachineAppArgs) -> ExitCode {
    let MachineAppArgs {
        desktop_id,
        desktop_file,
        program,
        wayland_socket,
        x11_display,
        open,
        open_url,
        dry_run,
        new_window,
        keeper,
        keep_in,
        keep_as,
    } = args;
    let keep = match (keeper, keep_in, keep_as) {
        (Some(keeper), Some(dir), Some(launch)) => Some(launcher::host::Keep {
            keeper,
            dir,
            launch,
        }),
        _ => None,
    };
    let env = LaunchEnv {
        wayland_socket,
        x11_display,
        activation_token: std::env::var("TILO_HOST_ACTIVATION_TOKEN").ok(),
        files: open
            .as_deref()
            .map(|path| vec![path.display().to_string()])
            .unwrap_or_default(),
        urls: open_url.into_iter().collect(),
        prefer_new_window: new_window,
    };
    let planned = match (program.as_deref(), desktop_file.as_deref()) {
        (Some(program), _) if dry_run => launcher::host::plan_host_program(program, &env),
        (Some(program), _) => launcher::host::make_runnable(program)
            .and_then(|()| launcher::host::plan_host_program(program, &env)),
        (None, Some(path)) => {
            launcher::host::plan_host_file(path, &env, &launcher::host::locales())
        }
        (None, None) => launcher::host::plan_host(&desktop_id, &env, &launcher::host::locales()),
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
    match planned.and_then(|plan| launcher::host::spawn(&plan, keep.as_ref())) {
        Ok(status) => {
            // A kept app's end is told as it ended: a signal that ended it ends the launcher too.
            #[cfg(target_os = "linux")]
            if let Some(signal) = std::os::unix::process::ExitStatusExt::signal(&status) {
                launcher::host::end_by(signal);
            }
            exit_code(status.code().unwrap_or(1))
        }
        Err(e) => {
            eprintln!("tilo-launcher: {e}");
            exit_code(e.to_exit_code())
        }
    }
}

/// Resolve an installed package via the package store's registry and launch its CLI-flavour entrypoint
/// under the sandbox.
fn run_installed(
    package: &str,
    command: &str,
    no_services: bool,
    minimal_libs: bool,
    rest: Vec<String>,
) -> ExitCode {
    // As in `exec_installed`.
    let (entry, _payload_held) = match find_current(package) {
        Ok(found) => found,
        Err(code) => return code,
    };

    // Read the manifest to check `[cli].commands` contains the requested name.
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

    // Installed tapp command shims are a production launch surface.
    let minimal_libs = minimal_libs
        || !matches!(
            std::env::var("TILO_LAUNCHER_NO_MINIMAL_LIBS").as_deref(),
            Ok("1" | "true")
        );
    let opts = LaunchOptions {
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
        data_home: None,
        trusted_identities_host_path: None,
        retained_files_host_path: None,
        browser_continuity_host_path: None,
        action_authority_host_path: None,
        // CLI launches are stdio-only: no display in the sandbox, and therefore no compositor whose
        // GPU choice they have to match.
        wayland_socket: None,
        // Inherit the workspace from the shell the command was typed into: a terminal opened in a
        // workspace lands its CLI apps in the same workspace rather than none.
        workspace_id: std::env::var(launcher::sandbox::WORKSPACE_ID_ENV)
            .ok()
            .filter(|s| !s.is_empty()),
        // A shell invocation via the PATH shim has no workspace behind it, so there are no
        // capabilities to honour — the package runs with what its manifest declares.
        capabilities_path: None,
        app_id: Some(package.to_string()),
        render_node: None,
        // A CLI invocation's arguments are the user's own, forwarded through `LaunchMode::Cli`; no
        // document to open on its behalf, and no GUI argv to carry.
        open_document: None,
        open_uri: None,
        gui_argv: Vec::new(),
        open_as_visitor: false,
        headless_console: None,
        tilolang_process: false,
        tilolang_connector: None,
    };
    match launcher::run::launch_with(&manifest, &entry.payload_root, &opts) {
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

/// One-shot activation values are focus capabilities, not useful launch-plan diagnostics.
fn host_launch_env_is_secret(key: &str) -> bool {
    matches!(key, "XDG_ACTIVATION_TOKEN" | "DESKTOP_STARTUP_ID")
}

/// The launcher's own diagnostics go to standard error: standard output is the app's, which a caller
/// reads as the app's answer (a record's search entries, a surface report).
fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let env = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(env)
        .with_target(false)
        .with_writer(std::io::stderr)
        // The session reads this stream into its log line by line: colour codes only where a
        // person runs the launcher at a terminal.
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        // A launcher relaying a kept app outlives the session reading its standard error: a line
        // it cannot write is dropped, never a panic of the subscriber's own report.
        .log_internal_errors(false)
        .try_init();
}

#[cfg(test)]
mod host_launch_tests {
    use super::*;

    #[test]
    fn host_launch_dry_run_redacts_one_shot_activation_values() {
        assert!(host_launch_env_is_secret("XDG_ACTIVATION_TOKEN"));
        assert!(host_launch_env_is_secret("DESKTOP_STARTUP_ID"));
        assert!(!host_launch_env_is_secret("TILO_HOST_ACTIVATION_TOKEN"));
        assert!(!host_launch_env_is_secret("WAYLAND_DISPLAY"));
    }

    /// The session starts every machine app as `tilo-launcher machine-app …`
    /// (`tilo_app_launch::plan`), behind its keeper when it has one: a launcher that answers to
    /// another name opens none of them.
    #[test]
    fn the_sessions_machine_app_launch_parses() {
        let parsed = Cli::try_parse_from([
            "tilo-launcher",
            "machine-app",
            "--desktop-id",
            "foot",
            "--wayland-socket",
            "/run/user/1000/wayland-1",
            "--keeper",
            "/usr/libexec/tilo-app-keeper",
            "--keep-in",
            "/run/user/1000/tilo-session/kept/1",
            "--keep-as",
            "foot",
        ])
        .expect("the session's launch of a machine app parses");
        let Cmd::MachineApp(args) = parsed.cmd else {
            panic!("a machine app's launch is not machine-app");
        };
        assert_eq!(args.desktop_id, "foot");
        assert_eq!(
            args.keeper.as_deref(),
            Some(Path::new("/usr/libexec/tilo-app-keeper"))
        );
    }

    /// The session starts a Windows program as `tilo-launcher windows-program …`
    /// (`tilo_session`'s Windows programs), the sandbox's command after `--`, flags and all.
    #[test]
    fn the_sessions_windows_program_launch_parses() {
        let parsed = Cli::try_parse_from([
            "tilo-launcher",
            "windows-program",
            "--app-id",
            "so.tilo.windows.probe",
            "--name",
            "Probe",
            "--keeper",
            "/usr/libexec/tilo-app-keeper",
            "--keep-in",
            "/tmp/kept/1",
            "--keep-as",
            "{}",
            "--",
            "/usr/bin/bwrap",
            "--unshare-all",
            "--",
            "/usr/bin/wine",
        ])
        .expect("the session's launch of a Windows program parses");
        assert!(
            starts_an_app(&parsed.cmd),
            "a Windows program is an app: it leaves the session's protection from the OOM killer"
        );
        let Cmd::WindowsProgram(args) = parsed.cmd else {
            panic!("a Windows program's launch is not windows-program");
        };
        assert_eq!(args.app_id, "so.tilo.windows.probe");
        assert!(!args.x11);
        assert_eq!(
            args.command,
            ["/usr/bin/bwrap", "--unshare-all", "--", "/usr/bin/wine"]
        );

        // A Proton program asks for a display of its keeper's with `--x11`, and `--print-keeper`
        // is the launcher's own: it prints the keeper's arguments and starts nothing.
        let parsed = Cli::try_parse_from([
            "tilo-launcher",
            "windows-program",
            "--app-id",
            "so.tilo.windows.probe",
            "--name",
            "Probe",
            "--x11",
            "--keeper",
            "/usr/libexec/tilo-app-keeper",
            "--keep-in",
            "/tmp/kept/1",
            "--keep-as",
            "{}",
            "--print-keeper",
            "--",
            "/usr/bin/bwrap",
            "--unshare-all",
            "--",
            "/usr/bin/wine",
        ])
        .expect("the session's launch of an X11 Windows program parses");
        let Cmd::WindowsProgram(args) = parsed.cmd else {
            panic!("a Windows program's launch is not windows-program");
        };
        assert!(args.x11);
        assert!(args.print_keeper);
        assert_eq!(
            args.command,
            ["/usr/bin/bwrap", "--unshare-all", "--", "/usr/bin/wine"]
        );
    }
}

#[cfg(test)]
mod action_authority_tests {
    use super::*;
    #[test]
    fn action_authority_cli_keeps_host_selected_path_and_workspace() {
        let parsed = Cli::try_parse_from([
            "tilo-launcher",
            "run",
            "--manifest",
            "/manifest.toml",
            "--payload-root",
            "/payload",
            "--workspace-id",
            "workspace-a",
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
        assert_eq!(workspace_id.as_deref(), Some("workspace-a"));
    }
}
