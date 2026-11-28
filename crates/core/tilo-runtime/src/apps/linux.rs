//! Launching through `tilo-launcher` on Linux.
//!
//! The launcher finds session services only at
//! `$XDG_RUNTIME_DIR/tilo/services/<name>.sock`, and document media at
//! `TILO_FS_BLOB_SOCKET`. The profile binds its sockets in a directory of its
//! own, short enough for a socket's address, so each launch hands the launcher a
//! private runtime directory beside them whose `tilo/services` links to the
//! sockets. It never sits inside the profile: a person's profile path is as deep
//! as their home and state keep it, and a socket there would not fit. The
//! launcher connects and authenticates each service itself, exactly as under
//! the desktop session; nothing it checks is relaxed.

use super::{Apps, Launch};
use crate::daemon::Sockets;
use app_catalogue::apps::InstalledApp;
use std::{
    io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// The launcher's view of `XDG_RUNTIME_DIR`, beside the daemon's service sockets.
const LAUNCH_RUNTIME_DIR: &str = "launch";
/// A Unix socket path longer than this cannot be connected (`sun_path`).
const MAX_SOCKET_PATH: usize = 107;

pub(super) fn launch(
    apps: &Apps,
    app: &InstalledApp,
    workspace: &str,
    sockets: &Sockets,
    original: &super::original::OriginalLaunch,
    shows: super::Shows,
    open: Option<&str>,
    runs: &super::Runs,
) -> Result<Launch, String> {
    // A tilolang app's instance runs in the app's tilolang process; its window is a spare window
    // process bound to that instance, as a session's is.
    let manifest = launcher::manifest::read_manifest(&app.manifest_path)
        .map_err(|error| format!("refusing to launch {}: {error}", app.app_id))?;
    if shows == super::Shows::Window
        && manifest
            .entrypoint
            .runtime
            .as_deref()
            .is_some_and(launcher::manifest::is_tilolang)
    {
        let hosted = apps.hosted_window(&app.app_id, workspace)?;
        return window(apps, (app, workspace), (&manifest, original), hosted);
    }
    spawn(
        apps,
        (app, workspace),
        (sockets, original),
        (shows, open),
        runs,
    )
}

/// Start the launcher for [`launch`].
fn spawn(
    apps: &Apps,
    (app, workspace): (&InstalledApp, &str),
    (sockets, original): (&Sockets, &super::original::OriginalLaunch),
    (shows, open): (super::Shows, Option<&str>),
    runs: &super::Runs,
) -> Result<Launch, String> {
    let (mut command, launcher) = command(
        apps,
        app,
        workspace,
        sockets,
        original,
        runs.writes.as_ref(),
    )?;
    if let Some(window) = runs.window {
        // The tab the shell holds for this launch: the app's window takes it.
        command.env("TILO_CONTINUITY_INSTANCE", window.to_string());
    }
    // The record the call named, handed to the app as its window's is: it opens it, or founds it
    // when nothing wrote it yet.
    if let Some(uri) = open {
        command.arg("--open-uri").arg(uri);
    }
    // The connection to the app's origin connector, handed to the process running the app's code
    // here: it reaches the service the device runs, as a hosted instance is handed its own.
    let origin_reader = runs
        .origin
        .as_ref()
        .map(|origin| {
            origin
                .try_clone()
                .map_err(|error| format!("the origin connection: {error}"))
        })
        .transpose()?;
    if shows == super::Shows::Nothing {
        // The toolkit's loop runs the app's code even drawing nothing, and the first window the
        // shell asks of the process takes its place, so it is handed the display where this
        // device has one. Where it has none (a server, nobody signed in), the app's own loop
        // takes its actions all the same. It ends after the idle time a tilolang process does;
        // the dispatch starts it again.
        if let Ok(socket) = wayland_socket() {
            command.arg("--wayland-socket").arg(socket);
        }
        command.env(ui::process::NO_WINDOW_ENV, "1").env(
            ui::process::NO_WINDOW_IDLE_ENV,
            crate::IDLE.as_secs().to_string(),
        );
    } else if shows == super::Shows::Connection {
        // No display and no idle end: it keeps the app's connection until the device stops it.
        command.env(ui::process::NO_WINDOW_ENV, "1");
    } else if shows == super::Shows::Capture {
        // A capture draws into its own pictures: it is handed no display and opens no window.
    } else {
        command.arg("--wayland-socket").arg(wayland_socket()?);
    }
    if origin_reader.is_some() {
        command.arg("--tilolang-origin-reader");
    }
    let joined = joined_links(&mut command, workspace, runs)?;
    // A capture names what it captures, exactly as a person's own run of the app does; the `--`
    // tail is the app's own and comes after every flag the launcher reads.
    if !runs.args.is_empty() {
        command.arg("--").args(&runs.args);
    }
    // The launcher waits here until the caller has recorded its pid, so it
    // can never connect before the broker could attest it.
    let registration = launcher::launch_hold::install(&mut command)
        .map_err(|error| format!("refusing to launch: the launcher's launch hold: {error}"))?;
    // What the program says before it ends is what a start that ends as it starts answers: the
    // sandbox's refusal, a missing runtime, an app that cannot load its module.
    command.stderr(std::process::Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not start {}: {error}", launcher.display()))?;
    Ok(Launch {
        stderr: said_by(&mut child),
        pid: child.id(),
        child,
        registration: Some(registration),
        origin_reader,
        joined,
        _link: None,
        window_link: None,
    })
}

/// Open the window of `app`'s instance `hosted` in `workspace` on the person's own Wayland
/// display, where no Tilo session draws it: the spare window process a session starts
/// (`tilo-launcher tilolang-window`, in the same sandbox), handed a connection to the display and
/// bound over its link to the app and the instance, exactly as a session binds one
/// (`tilolang_process::window::spare`). The launcher waits at its launch hold until the launch is
/// recorded, and is handed the link with its release.
pub(super) fn window(
    apps: &Apps,
    (app, workspace): (&InstalledApp, &str),
    (manifest, original): (
        &launcher::manifest::ManifestView,
        &super::original::OriginalLaunch,
    ),
    hosted: super::instances::HostedWindow,
) -> Result<Launch, String> {
    use runtime_portal::process_link::{Binding, Packet as _, ToProcess, WindowRequest};
    let refuse = |why: String| format!("refusing to open {}'s window: {why}", app.app_id);
    let runtime = launcher::manifest::code_runtime(manifest, &app.payload_root)
        .ok_or_else(|| refuse("it is no tilolang app".to_owned()))?;
    let app_runtime = manifest
        .runtime
        .as_deref()
        .ok_or_else(|| refuse("it names no Tilo runtime".to_owned()))?;
    let (module, surface) = window_files(&original.installed).map_err(refuse)?;
    let display = std::os::unix::net::UnixStream::connect(wayland_socket()?)
        .map_err(|error| refuse(format!("the Wayland display: {error}")))?;
    let (ours, theirs) = runtime_portal::process_link::pair()
        .map_err(|error| refuse(format!("its link: {error}")))?;
    let (surface, surface_hash) = surface.map_or((None, None), |(fd, hash)| (Some(fd), Some(hash)));
    [
        ToProcess::Display {
            wayland: std::os::fd::OwnedFd::from(display),
        },
        ToProcess::Patch {
            instance: hosted.instance,
            patch: hosted.patch,
        },
        ToProcess::Bind {
            binding: Box::new(Binding {
                app: app.app_id.to_string(),
                workspace: workspace.to_owned(),
                surface_hash,
                window: WindowRequest {
                    instance: hosted.instance,
                    reveal: None,
                    open: None,
                    visitor: false,
                },
            }),
            module,
            // No shell runs beside it.
            shell_ui: None,
            surface,
        },
    ]
    .into_iter()
    .try_for_each(|packet| packet.send(&ours))
    .map_err(|error| refuse(format!("its link: {error}")))?;
    let launcher = resolve_launcher(&apps.launcher_candidates)?;
    let mut command = Command::new(&launcher);
    command
        .arg(launcher::run::tilolang_window::SUBCOMMAND)
        .args(["--runtime", runtime, "--app-runtime", app_runtime])
        .arg("--workspace-id")
        .arg(workspace);
    if !package_store::dev_env::DEV_ENV_OVERRIDES {
        for name in launcher::dev_env::DEV_OVERRIDE_VARS {
            command.env_remove(name);
        }
    }
    let registration = launcher::launch_hold::install(&mut command)
        .map_err(|error| refuse(format!("the launcher's launch hold: {error}")))?;
    command.stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not start {}: {error}", launcher.display()))?;
    drop(command);
    Ok(Launch {
        stderr: said_by(&mut child),
        pid: child.id(),
        child,
        registration: Some(registration),
        origin_reader: None,
        joined: Vec::new(),
        _link: Some(ours),
        window_link: Some(theirs),
    })
}

/// What a window of `installed` is bound with, read through its signed record as a session reads
/// it: its compiled module and its `surface.toml` with the file's hash, each in a sealed memfd.
fn window_files(
    installed: &package_store::registry::InstalledPackage,
) -> Result<(std::os::fd::OwnedFd, Option<(std::os::fd::OwnedFd, String)>), String> {
    let mut files = package_store::integrity::signed_files(
        installed,
        &[
            runtime_portal::process_link::MODULE_FILE,
            package_store::integrity::SURFACE_FILE,
        ],
    )
    .map_err(|error| format!("its package: {error}"))?
    .into_iter();
    let (module, surface) = (files.next().flatten(), files.next().flatten());
    let module = module.ok_or("its module is not the one its package signed")?;
    let sealed = |name: &str, bytes: &[u8]| {
        tilo_render_protocol::shm::sealed_blob(name, bytes)
            .map_err(|error| format!("its {name}: {error}"))
    };
    let surface = match surface {
        Some(bytes) => Some((
            sealed("surface", &bytes)?,
            blake3::hash(&bytes).to_hex().to_string(),
        )),
        None => None,
    };
    Ok((sealed("module", &module)?, surface))
}

/// What `child` says on its standard error, kept (bounded) to answer a start that ends as it
/// starts, and carried on to the daemon's own standard error, its log among them.
fn said_by(child: &mut std::process::Child) -> std::sync::Arc<std::sync::Mutex<String>> {
    let stderr = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    if let Some(mut pipe) = child.stderr.take() {
        let captured = std::sync::Arc::clone(&stderr);
        std::thread::Builder::new()
            .name("app-start-stderr".into())
            .spawn(move || {
                use std::io::{Read as _, Write as _};
                let mut chunk = [0_u8; 512];
                while let Ok(read) = pipe.read(&mut chunk) {
                    let Some(read_bytes) = chunk.get(..read).filter(|bytes| !bytes.is_empty())
                    else {
                        break;
                    };
                    // The app's words still go where the daemon's own go, its log among them,
                    // as they did when the app wrote to the daemon's standard error itself.
                    let _ = std::io::stderr().write_all(read_bytes);
                    let mut said = captured
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if said.len() < 8192 {
                        said.push_str(&String::from_utf8_lossy(read_bytes));
                    }
                }
            })
            .ok();
    }
    stderr
}

/// A capture's instance joins its workspace over the connections made for it: the launch names the
/// instance and its person, and the connections are released after the origin connection.
fn joined_links(
    command: &mut std::process::Command,
    workspace: &str,
    runs: &super::Runs,
) -> Result<Vec<std::os::fd::OwnedFd>, String> {
    let Some(joined) = &runs.joined else {
        return Ok(Vec::new());
    };
    command
        .arg("--tilolang-joined")
        .arg(format!("{} {workspace} {}", joined.instance, joined.person));
    [&joined.collaboration, &joined.identity, &joined.content]
        .into_iter()
        .map(|connection| {
            connection
                .try_clone()
                .map_err(|error| format!("the joined instance's connection: {error}"))
        })
        .collect()
}

/// Start `app`'s connector for `origin` in `workspace`: the app's own launch with no window
/// (`tilo-launcher run --tilolang-connector <origin>`), in the sandbox its signed manifest
/// declares for that origin, handed `socket` as its own service through the launch hold.
pub(super) fn connector(
    apps: &Apps,
    app: &InstalledApp,
    workspace: &str,
    sockets: &Sockets,
    original: &super::original::OriginalLaunch,
    (origin, socket): (&str, std::os::fd::BorrowedFd<'_>),
) -> Result<std::process::Child, String> {
    let (mut command, launcher) = command(apps, app, workspace, sockets, original, None)?;
    // What a launch that fails says is read here, to answer why it did not come up.
    command.stderr(std::process::Stdio::piped());
    // A connector draws nothing, opens nothing and reads no record: no display goes with it.
    command.arg("--tilolang-connector").arg(origin);
    let hold = launcher::launch_hold::install(&mut command)
        .map_err(|error| format!("{}'s connector has no launch hold: {error}", app.app_id))?;
    let child = command
        .spawn()
        .map_err(|error| format!("could not start {}: {error}", launcher.display()))?;
    drop(command);
    if let Err(error) = hold.release_with_window_link(socket) {
        let mut child = child;
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!(
            "{}'s connector was not released: {error}",
            app.app_id
        ));
    }
    Ok(child)
}

/// The launcher's command for `app` in `workspace`, as installed, with the person's grants and
/// the profile's services, and the launcher it runs: a window or a connector adds its own.
fn command(
    apps: &Apps,
    app: &InstalledApp,
    workspace: &str,
    sockets: &Sockets,
    original: &super::original::OriginalLaunch,
    writes: Option<&super::capture::Writes>,
) -> Result<(Command, PathBuf), String> {
    let launcher = resolve_launcher(&apps.launcher_candidates)?;
    let runtime_dir = service_directory(sockets)
        .map_err(|error| format!("refusing to launch: the launcher's services: {error}"))?;
    let mut command = Command::new(&launcher);
    command
        .arg("run")
        .arg("--manifest")
        .arg(&app.manifest_path)
        .arg("--payload-root")
        .arg(&app.payload_root)
        .arg("--workspace-id")
        .arg(workspace)
        .arg("--app-id")
        .arg(app.app_id.as_str())
        .arg("--expected-package-hash")
        .arg(&original.package_hash);
    // The person's grants for this app, in the file the launcher reads them
    // from: the same one a desktop session writes. A capture's directories are its launch's
    // alone, so its file is written apart, where no other launch of the app reads it.
    let capabilities = launcher::capabilities::write_capability_file(
        &writes.map_or_else(
            || apps.root.join("tilo/capabilities"),
            |writes| writes.grants_dir.clone(),
        ),
        app.app_id.as_str(),
        &apps.launch_app_grants(app, workspace, writes),
    )
    .map_err(|error| format!("refusing to launch {}: its grants: {error}", app.app_id))?;
    if let Some(path) = capabilities {
        command.arg("--capabilities").arg(path);
    }
    command
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env(launcher::services::BLOB_SOCKET_ENV, &sockets.blobs)
        // The launcher keeps the profile out of every grant it binds.
        .env(tilo_machine::DAEMON_PROFILE_VAR, &apps.root)
        // What the workspace trusts, as this device holds it: the launcher verifies the app's
        // publisher against it, and never asks this daemon's action listing, which answers only
        // once every workspace's providers do and so waits on launches like this one.
        .env(
            tilo_control::packages::WORKSPACE_TRUST_ENV,
            trust_reading(apps.package_trust(workspace)),
        )
        // The launcher connects every service itself; nothing inherited
        // from this process stands for one.
        .env_remove("TILO_SVC_COLLAB_FD")
        .env_remove("TILO_SVC_IDENTITY_FD")
        .env_remove("TILO_SVC_BLOBS_FD")
        .env_remove("TILO_RUNTIME_CONNECTION_OWNER_PID")
        .env_remove("TILO_RUNTIME_RESUME_LIVE")
        .stdin(Stdio::null());
    if !package_store::dev_env::DEV_ENV_OVERRIDES {
        for name in launcher::dev_env::DEV_OVERRIDE_VARS {
            command.env_remove(name);
        }
    }
    Ok((command, launcher))
}

/// `trust` as the launcher reads a workspace's trust list ([`tilo_control::packages::trust`]).
pub(super) fn trust_reading(
    (attestors, revoked): crate::device::package_runs::PackageTrust,
) -> String {
    serde_json::json!({ "attestors": attestors, "revoked": revoked }).to_string()
}

/// The host session's Wayland socket, which the launcher binds into the
/// sandbox as the app's only display.
///
/// A daemon started before its login's compositor, as a session's own daemon is, reads the
/// display the login publishes for the programs beside the session
/// ([`tilo_paths::login::WAYLAND_DISPLAY_FILE`]), so a headless session's apps open on the
/// compositor it runs. A daemon a desktop starts already knows the display in its environment.
fn wayland_socket() -> Result<PathBuf, String> {
    let display = std::env::var_os("WAYLAND_DISPLAY")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(published_display)
        .ok_or("refusing to launch: this session has no Wayland display")?;
    let socket = if display.is_absolute() {
        display
    } else {
        tilo_paths::xdg::runtime_dir()
            .ok_or("refusing to launch: the Wayland display has no runtime directory")?
            .join(display)
    };
    if !socket.exists() {
        return Err(format!(
            "refusing to launch: the Wayland display {} does not exist",
            socket.display()
        ));
    }
    Ok(socket)
}

/// The display name the session's compositor published to its login, if it published one.
fn published_display() -> Option<PathBuf> {
    let dir = tilo_paths::login::session_dir(std::env::var_os("XDG_RUNTIME_DIR"))?;
    let name = std::fs::read_to_string(dir.join(tilo_paths::login::WAYLAND_DISPLAY_FILE)).ok()?;
    let name = name.trim();
    (!name.is_empty()).then(|| PathBuf::from(name))
}

/// Prepare `<sockets>/launch/tilo/services` with the profile's collaboration and
/// identity sockets and the daemon's native endpoint under the names the
/// launcher looks for; the runtime directory to hand it.
///
/// The runtime directory is a private directory beside the daemon's service
/// sockets, never under the profile: a profile's path is the person's home and
/// state, which may be longer than a socket's address holds, while the sockets
/// already live where a socket fits.
fn service_directory(sockets: &Sockets) -> io::Result<PathBuf> {
    let services_dir = sockets.collab.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "a service socket has no directory",
        )
    })?;
    let runtime_dir = services_dir.join(LAUNCH_RUNTIME_DIR);
    let services = runtime_dir.join("tilo/services");
    tilo_files::native::private::directory(&runtime_dir)?;
    tilo_files::native::private::directory(&runtime_dir.join("tilo"))?;
    tilo_files::native::private::directory(&services)?;
    for (name, target) in [
        (launcher::services::COLLAB_SERVICE, &sockets.collab),
        (launcher::services::IDENTITY_SERVICE, &sockets.identity),
        (launcher::services::NATIVE_SERVICE, &sockets.native),
    ] {
        let link = services.join(format!("{name}.sock"));
        if link.as_os_str().len() > MAX_SOCKET_PATH {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "the launcher's runtime path is too long for {}",
                    link.display()
                ),
            ));
        }
        link_service(target, &link)?;
    }
    Ok(runtime_dir)
}

/// Make `link` name `target`. The services are one set for the device, so two launches of an app
/// side by side ask for the same links: a link already there to `target` is the one this launch
/// needs.
///
/// The link is made under a name of its own and renamed into place. A rename replaces the name in
/// one step, so a launch that is checking the link never finds it missing while another launch
/// makes it, and two launches reaching the same link both end with one naming `target`. Removing
/// the old name first left exactly that window, and a launch that lost it refused a link the
/// other had already made.
fn link_service(target: &Path, link: &Path) -> io::Result<()> {
    if is_link_to(link, target) {
        return Ok(());
    }
    let staged = staged_link_name(link);
    std::os::unix::fs::symlink(target, &staged)?;
    match std::fs::rename(&staged, link) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = std::fs::remove_file(&staged);
            // A name a link cannot take (a directory someone put there) is refused rather than
            // passed over: only a launch's own link may stand at it.
            Err(error)
        }
    }
}

/// A name beside `link` for the link this launch is about to rename into place: unique to the
/// launch, and no name a launch looks a service up by.
fn staged_link_name(link: &Path) -> PathBuf {
    static STAGED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let name = link
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let staged = STAGED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    link.with_file_name(format!(".{name}.{}.{staged}", std::process::id()))
}

/// Whether `link` is a link to exactly `target`.
fn is_link_to(link: &Path, target: &Path) -> bool {
    std::fs::read_link(link).ok().as_deref() == Some(target)
}

/// The first launcher binary that exists.
///
/// # Errors
///
/// Refuses the launch when none does: there is no unconfined fallback.
pub(super) fn resolve_launcher(candidates: &[PathBuf]) -> Result<PathBuf, String> {
    candidates
        .iter()
        // On Tilo OS the old name beside a program in /usr/bin is a link to
        // `tilo`, not the launcher; the real one is in /usr/libexec/tilo.
        .find(|candidate| {
            candidate.is_file()
                && !(candidate.is_symlink()
                    && std::fs::canonicalize(candidate)
                        .ok()
                        .is_some_and(|target| target.file_name() != candidate.file_name()))
        })
        .cloned()
        .ok_or_else(|| {
            format!(
                "refusing to launch: tilo-launcher was not found (searched {}); Tilo never \
                 runs an app unsandboxed",
                candidates
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    tilo_test_env::scenario! {
        fn the_launcher_is_resolved_and_finds_the_profiles_services;

        phase the_launcher_finds_the_profiles_services_where_it_looks;
        {
            let temp = tempfile::tempdir().unwrap();
            let root = std::fs::canonicalize(temp.path()).unwrap();
            let sockets = Sockets {
                collab: root.join("collab.sock"),
                identity: root.join("identity.sock"),
                blobs: root.join("blobs.sock"),
                native: root.join("native.sock"),
            };
            let _collab = std::os::unix::net::UnixListener::bind(&sockets.collab).unwrap();
            let _identity = std::os::unix::net::UnixListener::bind(&sockets.identity).unwrap();
            let _native = std::os::unix::net::UnixListener::bind(&sockets.native).unwrap();
            let runtime_dir = service_directory(&sockets).unwrap();
            assert_eq!(service_directory(&sockets).unwrap(), runtime_dir);
            assert_eq!(runtime_dir, root.join(LAUNCH_RUNTIME_DIR));
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("XDG_RUNTIME_DIR", &runtime_dir);
            assert_eq!(
                launcher::services::collab_socket(),
                runtime_dir.join("tilo/services/collab.sock")
            );
            std::os::unix::net::UnixStream::connect(launcher::services::collab_socket()).unwrap();
            std::os::unix::net::UnixStream::connect(launcher::services::identity_socket()).unwrap();
            // The daemon's native endpoint, which the launcher connects for an app that names it.
            std::os::unix::net::UnixStream::connect(launcher::services::socket_for(
                launcher::services::NATIVE_SERVICE,
            ))
            .unwrap();
            let mode = std::fs::metadata(runtime_dir.join("tilo/services"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0);
        }

        phase a_launch_runtime_lives_beside_the_sockets_however_deep_the_profile;
        {
            // The person's profile is their home and state, which may be deeper than a socket's
            // address holds: the launcher's runtime directory is beside the daemon's sockets, so
            // the profile's depth never refuses a launch.
            let temp = tempfile::tempdir().unwrap();
            let base = std::fs::canonicalize(temp.path()).unwrap();
            let mut profile = base.join("home/.local/state/tilo/runtime");
            while profile
                .join("run/tilo/services/collab.sock")
                .as_os_str()
                .len()
                <= MAX_SOCKET_PATH
            {
                profile = profile.join("a-state-directory-named-long");
            }
            let sockets_dir = base.join("sockets");
            std::fs::create_dir(&sockets_dir).unwrap();
            let sockets = Sockets {
                collab: sockets_dir.join("collab.sock"),
                identity: sockets_dir.join("identity.sock"),
                blobs: sockets_dir.join("blobs.sock"),
                native: sockets_dir.join("native.sock"),
            };
            let runtime_dir = service_directory(&sockets).unwrap();
            assert_eq!(runtime_dir, sockets_dir.join(LAUNCH_RUNTIME_DIR));
            assert!(!runtime_dir.starts_with(&profile));
            for name in [
                launcher::services::COLLAB_SERVICE,
                launcher::services::IDENTITY_SERVICE,
                launcher::services::NATIVE_SERVICE,
            ] {
                let link = runtime_dir.join(format!("tilo/services/{name}.sock"));
                assert!(link.as_os_str().len() <= MAX_SOCKET_PATH, "{link:?}");
            }
        }

        phase a_runtime_that_does_not_fit_a_socket_is_refused_by_name;
        {
            let temp = tempfile::tempdir().unwrap();
            let mut dir = std::fs::canonicalize(temp.path()).unwrap();
            while dir
                .join(LAUNCH_RUNTIME_DIR)
                .join("tilo/services/collab.sock")
                .as_os_str()
                .len()
                <= MAX_SOCKET_PATH
            {
                dir = dir.join("a-directory-named-long");
            }
            let sockets = Sockets {
                collab: dir.join("collab.sock"),
                identity: dir.join("identity.sock"),
                blobs: dir.join("blobs.sock"),
                native: dir.join("native.sock"),
            };
            let error = service_directory(&sockets).unwrap_err();
            assert!(
                error.to_string().contains("too long for"),
                "{error}"
            );
        }

        phase a_link_another_launch_made_to_the_same_target_is_this_launches_link;
        {
            let temp = tempfile::tempdir().unwrap();
            let target = temp.path().join("collab.sock");
            std::fs::write(&target, b"").unwrap();
            let link = temp.path().join("services/collab.sock");
            std::fs::create_dir_all(link.parent().unwrap()).unwrap();
            // Another launch made the link this one needs: the same target, taken as it is.
            std::os::unix::fs::symlink(&target, &link).unwrap();
            assert!(is_link_to(&link, &target));
            link_service(&target, &link).unwrap();
            assert_eq!(std::fs::read_link(&link).unwrap(), target);
            // An entry that is no link to the target is never one this launch made.
            let other = temp.path().join("other.sock");
            std::fs::write(&other, b"").unwrap();
            std::fs::remove_file(&link).unwrap();
            std::os::unix::fs::symlink(&other, &link).unwrap();
            assert!(!is_link_to(&link, &target));
            let plain = temp.path().join("plain");
            std::fs::write(&plain, b"").unwrap();
            assert!(!is_link_to(&plain, &target));
            let directory = temp.path().join("directory");
            std::fs::create_dir(&directory).unwrap();
            assert!(!is_link_to(&directory, &target));
            assert!(!is_link_to(&temp.path().join("missing"), &target));
            // A stale link is replaced by the one this launch needs.
            link_service(&target, &link).unwrap();
            assert_eq!(std::fs::read_link(&link).unwrap(), target);
            // An entry that cannot be a link refuses the launch rather than passing it on.
            let refusal = temp.path().join("refusal/collab.sock");
            std::fs::create_dir_all(refusal.parent().unwrap()).unwrap();
            std::fs::create_dir(&refusal).unwrap();
            assert!(link_service(&target, &refusal).is_err());
        }

        phase launches_side_by_side_share_the_services_they_made;
        {
            // Two situations of one app start side by side, so each launch makes the same links:
            // the ones that lose the race find the link another made instead of failing.
            let temp = tempfile::tempdir().unwrap();
            let root = std::fs::canonicalize(temp.path()).unwrap();
            let sockets = Sockets {
                collab: root.join("collab.sock"),
                identity: root.join("identity.sock"),
                blobs: root.join("blobs.sock"),
                native: root.join("native.sock"),
            };
            let _collab = std::os::unix::net::UnixListener::bind(&sockets.collab).unwrap();
            let _identity = std::os::unix::net::UnixListener::bind(&sockets.identity).unwrap();
            let _native = std::os::unix::net::UnixListener::bind(&sockets.native).unwrap();
            let together = std::sync::Barrier::new(4);
            std::thread::scope(|scope| {
                for _ in 0..4 {
                    scope.spawn(|| {
                        for _ in 0..50 {
                            together.wait();
                            service_directory(&sockets).expect("a launch finds the services");
                        }
                    });
                }
            });
            let services = root.join(LAUNCH_RUNTIME_DIR).join("tilo/services");
            for (name, target) in [
                (launcher::services::COLLAB_SERVICE, &sockets.collab),
                (launcher::services::IDENTITY_SERVICE, &sockets.identity),
                (launcher::services::NATIVE_SERVICE, &sockets.native),
            ] {
                let link = services.join(format!("{name}.sock"));
                assert_eq!(&std::fs::read_link(&link).unwrap(), target);
            }
        }

        phase a_link_a_launch_replaces_is_never_missing;
        {
            // A launch that finds a stale link replaces it. The replacement is one step, so a
            // launch looking the link up while another replaces it never finds the name gone:
            // removing the old name first left it missing, and a launch that looked then refused
            // a link another had already made.
            use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
            let temp = tempfile::tempdir().unwrap();
            let target = temp.path().join("collab.sock");
            std::fs::write(&target, b"").unwrap();
            let stale = temp.path().join("stale.sock");
            std::fs::write(&stale, b"").unwrap();
            let link = temp.path().join("services/collab.sock");
            std::fs::create_dir_all(link.parent().unwrap()).unwrap();
            let mut missing = 0;
            for _ in 0..300 {
                let _ = std::fs::remove_file(&link);
                std::os::unix::fs::symlink(&stale, &link).unwrap();
                let replacements = AtomicUsize::new(0);
                let stop = AtomicBool::new(false);
                std::thread::scope(|scope| {
                    for _ in 0..3 {
                        scope.spawn(|| {
                            link_service(&target, &link).expect("a launch replaces the link");
                            if replacements.fetch_add(1, Ordering::Relaxed) == 2 {
                                stop.store(true, Ordering::Relaxed);
                            }
                        });
                    }
                    // The name is a link at every instant, whatever a launch is making of it.
                    let mut reads = 0_u32;
                    while !stop.load(Ordering::Relaxed) || reads < 50 {
                        if std::fs::read_link(&link).is_err() {
                            missing += 1;
                        }
                        reads = reads.saturating_add(1);
                    }
                });
                assert_eq!(
                    std::fs::read_link(&link).unwrap(),
                    target,
                    "the launch's link stands after its replacement"
                );
            }
            assert_eq!(missing, 0, "the link was missing {missing} times");
        }

        phase no_launcher_binary_refuses_rather_than_running_unconfined;
        {
            let temp = tempfile::tempdir().unwrap();
            let missing = vec![temp.path().join("nowhere/tilo-launcher")];
            let err = resolve_launcher(&missing).unwrap_err();
            assert!(err.contains("refusing to launch"), "{err}");
            assert!(err.contains("nowhere/tilo-launcher"), "{err}");
            // A directory of that name is not a launcher either.
            std::fs::create_dir_all(&missing[0]).unwrap();
            assert!(resolve_launcher(&missing).is_err());
        }
    }

    /// A login publishes the display its session's compositor listens on; a daemon started
    /// before it reads that, so a headless session's apps open on the compositor it runs.
    #[test]
    fn a_daemon_reads_the_display_its_login_published() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = std::fs::canonicalize(dir.path()).unwrap();
        let login = tilo_paths::login::session_dir(Some(runtime.clone().into_os_string())).unwrap();
        std::fs::create_dir_all(&login).unwrap();
        std::fs::write(
            login.join(tilo_paths::login::WAYLAND_DISPLAY_FILE),
            "wayland-0\n",
        )
        .unwrap();
        std::fs::write(runtime.join("wayland-0"), b"").unwrap();
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set("XDG_RUNTIME_DIR", &runtime);
        environment.remove("WAYLAND_DISPLAY");
        assert_eq!(wayland_socket().unwrap(), runtime.join("wayland-0"));
    }

    #[test]
    fn a_launch_with_no_display_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = std::fs::canonicalize(dir.path()).unwrap();
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set("XDG_RUNTIME_DIR", &runtime);
        environment.remove("WAYLAND_DISPLAY");
        assert!(wayland_socket().is_err());
    }
}
