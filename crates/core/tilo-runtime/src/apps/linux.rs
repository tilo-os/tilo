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
    path::PathBuf,
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
) -> Result<Launch, String> {
    let (mut command, launcher) = command(apps, app, workspace, sockets, original)?;
    // The record the call named, handed to the app as its window's is: it opens it, or founds it
    // when nothing wrote it yet.
    if let Some(uri) = open {
        command.arg("--open-uri").arg(uri);
    }
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
    } else {
        command.arg("--wayland-socket").arg(wayland_socket()?);
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
    let stderr = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    if let Some(mut pipe) = child.stderr.take() {
        let captured = std::sync::Arc::clone(&stderr);
        std::thread::Builder::new()
            .name("app-start-stderr".into())
            .spawn(move || {
                use std::io::Read as _;
                let mut chunk = [0_u8; 512];
                while let Ok(read) = pipe.read(&mut chunk) {
                    if read == 0 {
                        break;
                    }
                    let mut said = captured
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if said.len() < 8192
                        && let Some(read_bytes) = chunk.get(..read)
                    {
                        said.push_str(&String::from_utf8_lossy(read_bytes));
                    }
                }
            })
            .ok();
    }
    Ok(Launch {
        stderr,
        pid: child.id(),
        child,
        registration: Some(registration),
    })
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
    let (mut command, launcher) = command(apps, app, workspace, sockets, original)?;
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
    // from: the same one a desktop session writes.
    let capabilities = launcher::capabilities::write_capability_file(
        &apps.root.join("tilo/capabilities"),
        app.app_id.as_str(),
        &apps.app_grants(app, workspace),
    )
    .map_err(|error| format!("refusing to launch {}: its grants: {error}", app.app_id))?;
    if let Some(path) = capabilities {
        command.arg("--capabilities").arg(path);
    }
    command
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env(launcher::services::BLOB_SOCKET_ENV, &sockets.blobs)
        // The launcher keeps the profile out of every grant it binds.
        .env(tilo_paths::machine::DAEMON_PROFILE_VAR, &apps.root)
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
        if std::fs::read_link(&link).ok().as_deref() != Some(target.as_path()) {
            match std::fs::remove_file(&link) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            std::os::unix::fs::symlink(target, &link)?;
        }
    }
    Ok(runtime_dir)
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
