//! Launching through `tilo-launcher` on Linux.
//!
//! The launcher finds session services only at
//! `$XDG_RUNTIME_DIR/tilo/services/<name>.sock`, and document media at
//! `TILO_FS_BLOB_SOCKET`. The profile binds its sockets in its own private
//! directory, so each launch hands the launcher a private runtime directory
//! inside the profile whose `tilo/services` links to them. The launcher
//! connects and authenticates each service itself, exactly as under the
//! desktop session; nothing it checks is relaxed.

use super::{Apps, Launch, Sockets};
use crate::unix;
use std::{
    io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use tilo_installed_apps::InstalledApp;

/// The launcher's view of `XDG_RUNTIME_DIR`, inside the profile.
const LAUNCH_RUNTIME_DIR: &str = "run";
/// A Unix socket path longer than this cannot be connected (`sun_path`).
const MAX_SOCKET_PATH: usize = 107;

pub(super) fn launch(apps: &Apps, app: &InstalledApp, sockets: &Sockets) -> Result<Launch, String> {
    let launcher = resolve_launcher(&apps.launcher_candidates)?;
    let wayland = wayland_socket()?;
    let runtime_dir = service_directory(&apps.root, sockets)
        .map_err(|error| format!("refusing to launch: the launcher's services: {error}"))?;
    let identity = tilo_identity_host::registry::selected_identity_path(&apps.room)
        .map_err(|error| format!("refusing to launch: the workspace identity: {error}"))?;
    let mut command = Command::new(&launcher);
    command
        .arg("run")
        .arg("--manifest")
        .arg(&app.manifest_path)
        .arg("--payload-root")
        .arg(&app.payload_root)
        .arg("--workspace-id")
        .arg(&apps.room)
        .arg("--app-id")
        .arg(&app.package_id)
        .arg("--wayland-socket")
        .arg(&wayland);
    if let Some(identity) = identity {
        command.arg("--identity").arg(identity);
    }
    command
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env(launcher::services::BLOB_SOCKET_ENV, &sockets.blobs)
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
    // The launcher waits here until the caller has recorded its pid, so it
    // can never connect before the broker could attest it.
    let registration = launcher::registration_gate::install(&mut command).map_err(|error| {
        format!("refusing to launch: the launcher's registration gate: {error}")
    })?;
    let child = command
        .spawn()
        .map_err(|error| format!("could not start {}: {error}", launcher.display()))?;
    Ok(Launch {
        pid: child.id(),
        child,
        registration: Some(registration),
    })
}

/// The host session's Wayland socket, which the launcher binds into the
/// sandbox as the app's only display.
fn wayland_socket() -> Result<PathBuf, String> {
    let display = std::env::var_os("WAYLAND_DISPLAY")
        .filter(|value| !value.is_empty())
        .ok_or("refusing to launch: this session has no Wayland display")?;
    let display = PathBuf::from(display);
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

/// Prepare `<root>/run/tilo/services` with the profile's collaboration and
/// identity sockets under the names the launcher looks for; the runtime
/// directory to hand it.
fn service_directory(root: &Path, sockets: &Sockets) -> io::Result<PathBuf> {
    let runtime_dir = root.join(LAUNCH_RUNTIME_DIR);
    let services = runtime_dir.join("tilo/services");
    unix::private_directory(&runtime_dir)?;
    unix::private_directory(&runtime_dir.join("tilo"))?;
    unix::private_directory(&services)?;
    for (name, target) in [
        (launcher::services::COLLAB_SERVICE, &sockets.collab),
        (launcher::services::IDENTITY_SERVICE, &sockets.identity),
    ] {
        let link = services.join(format!("{name}.sock"));
        if link.as_os_str().len() > MAX_SOCKET_PATH {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("the profile path is too long for {}", link.display()),
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
fn resolve_launcher(candidates: &[PathBuf]) -> Result<PathBuf, String> {
    candidates
        .iter()
        .find(|candidate| candidate.is_file())
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

    #[test]
    fn the_launcher_finds_the_profiles_services_where_it_looks() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let sockets = Sockets {
            collab: root.join("collab.sock"),
            identity: root.join("identity.sock"),
            blobs: root.join("blobs.sock"),
        };
        let _collab = std::os::unix::net::UnixListener::bind(&sockets.collab).unwrap();
        let _identity = std::os::unix::net::UnixListener::bind(&sockets.identity).unwrap();
        let runtime_dir = service_directory(&root, &sockets).unwrap();
        // Idempotent across launches.
        assert_eq!(service_directory(&root, &sockets).unwrap(), runtime_dir);
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_RUNTIME_DIR", &runtime_dir);
        assert_eq!(
            launcher::services::collab_socket(),
            runtime_dir.join("tilo/services/collab.sock")
        );
        std::os::unix::net::UnixStream::connect(launcher::services::collab_socket()).unwrap();
        std::os::unix::net::UnixStream::connect(launcher::services::identity_socket()).unwrap();
        let mode = std::fs::metadata(runtime_dir.join("tilo/services"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0);
    }

    #[test]
    fn no_launcher_binary_refuses_rather_than_running_unconfined() {
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
