//! What the device's control panel reaches of the system it controls.
//!
//! Settings is a client of the system's authorities — `NetworkManager`,
//! `BlueZ`, logind, timedated, `PipeWire`, bootc — not a second
//! implementation of them. Those authorities answer on the system bus, on
//! their own sockets and through their own command-line clients, and each one
//! decides what the person may change (polkit, group membership). An ordinary
//! app sandbox carries none of that: inside it `nmcli` does not exist and the
//! system bus is not there, so every read fails and the pane would have
//! nothing true to show.
//!
//! The grant is **owned by the image, not by the package**: only the package
//! ids in [`CLIENTS`] receive it, whatever a manifest says. The package in
//! turn names the contract it was built against as the `system` service of
//! its `[sandbox.services]` ([`crate::services::SYSTEM_SERVICE`]); a client
//! that does not declare it gets nothing, and one that requires a version
//! the image does not serve is refused before launch. This grant is version
//! 1 of that interface; changing what it binds or which authorities it
//! reaches in a way an older control panel could not work with is a new
//! version, declared in the image's `system-interfaces.toml`. It adds
//!
//! - the host's client binaries and configuration, read-only
//!   ([`HOST_READ_PATHS`]);
//! - the system bus through its own filtered `xdg-dbus-proxy`, which lets
//!   the app talk only to the named authorities ([`SYSTEM_BUS_FILTER_ARGS`]);
//!   there is no raw-bus fallback, a failed proxy leaves the bus absent;
//! - the `PipeWire` socket, when the session runs it (printing stays behind
//!   the Print portal: no sandbox reaches the print spooler's socket);
//! - the cloud account service's runtime directory with its control socket:
//!   sync now, pause and resume a mounted account (no credential crosses it,
//!   and file requests are served only on a socket outside this grant);
//! - the NTFS mount service's directory with its control socket
//!   ([`NTFS_MOUNT_DIR`]): the person's remembered drives, to list, forget
//!   or switch to opening safely. Opening a drive is not among them: the
//!   service opens one only for the compositor, after its own prompt;
//! - the compositor's runtime state files (`$XDG_RUNTIME_DIR/tilo`: the
//!   connected displays, input devices), read-only, with every
//!   subdirectory (capabilities, identity projections, service sockets)
//!   masked by an empty tmpfs;
//! - the installed applications, read-only: the image's store
//!   (`/usr/share/tilo-apps`) and the person's own, at the path the app's
//!   remapped `XDG_DATA_HOME` names, so the default-app and notification
//!   lists show what is really installed.
//!
//! The rest of the sandbox is unchanged: no network namespace of its own, the
//! same seccomp profile, Landlock rules for exactly the paths bound here.

use std::path::PathBuf;

use crate::dbus_proxy::{self, DBusProxyGuard};

/// The packages that act as the device's system control panel.
pub const CLIENTS: &[&str] = &["so.tilo.settings"];

/// Host paths bound read-only (and executable) at the same path.
pub const HOST_READ_PATHS: &[&str] = &[
    "/usr/bin",
    "/usr/sbin",
    "/usr/libexec",
    "/etc",
    // systemctl, timedatectl and localectl probe sd_booted() before opening
    // their bus connection. This empty marker identifies the systemd host;
    // it grants neither its PID namespace nor its private manager socket.
    "/run/systemd/system",
    package_store::layout::SYSTEM_DATA_DIR,
];

/// Where the filtered system bus appears inside the sandbox: the path every
/// client library tries when `DBUS_SYSTEM_BUS_ADDRESS` is unset.
pub const SANDBOX_SYSTEM_BUS: &str = "/run/dbus/system_bus_socket";

/// The system-bus filter: the bus daemon (for `Hello` and name lookups) and
/// the authorities the control panel is a client of. Each authority still
/// applies its own policy to every call.
pub const SYSTEM_BUS_FILTER_ARGS: &[&str] = &[
    "--filter",
    "--see=org.freedesktop.DBus",
    "--talk=org.freedesktop.NetworkManager",
    "--talk=org.freedesktop.ModemManager1",
    "--talk=org.bluez",
    "--talk=org.freedesktop.login1",
    "--talk=org.freedesktop.systemd1",
    "--talk=org.freedesktop.timedate1",
    "--talk=org.freedesktop.timesync1",
    "--talk=org.freedesktop.locale1",
    "--talk=org.freedesktop.hostname1",
    "--talk=org.freedesktop.Accounts",
    "--talk=org.freedesktop.UPower",
    "--talk=org.freedesktop.UPower.PowerProfiles",
    "--talk=net.hadess.PowerProfiles",
    "--talk=org.freedesktop.fwupd",
    "--talk=org.fedoraproject.FirewallD1",
];

/// Whether `package_id` may receive the grant.
#[must_use]
pub fn is_client(package_id: &str) -> bool {
    CLIENTS.contains(&package_id)
}

/// Whether a launch of `package_id` declaring `services` receives the grant:
/// an image-named client that declares the system interface.
#[must_use]
pub fn grants(
    package_id: &str,
    services: &std::collections::BTreeMap<String, crate::manifest::ServiceReq>,
) -> bool {
    is_client(package_id) && services.contains_key(crate::services::SYSTEM_SERVICE)
}

/// One socket bound at the same path inside the sandbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocketBind {
    pub host: PathBuf,
    pub sandbox: PathBuf,
}

/// What one launch of a control-panel package receives.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SystemAuthorityPlan {
    /// Host paths bound read-only at the same path.
    pub read_paths: Vec<PathBuf>,
    /// Sockets the app connects to: the filtered system bus and the
    /// authorities' own sockets.
    pub sockets: Vec<SocketBind>,
    /// Set when the filtered system bus is bound.
    pub system_bus: bool,
    /// The compositor's runtime state directory, bound read-only.
    pub runtime_state: Option<RuntimeState>,
    /// The person's own app store, bound read-only where the app looks for
    /// it and at its own path, which its registry's entries name.
    pub user_apps: Option<SocketBind>,
}

/// The compositor publishes what it sees of the hardware (displays, input
/// devices) as files it replaces atomically, so the directory is bound, not
/// the files: a bound file would keep showing the inode it was bound to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeState {
    pub dir: PathBuf,
    /// The directory's subdirectories, each hidden under an empty tmpfs.
    pub masked: Vec<PathBuf>,
}

/// The compositor's runtime state directory for a session of `uid`, with its
/// subdirectories to mask. `None` when the compositor has published nothing.
fn runtime_state(uid: u32) -> Option<RuntimeState> {
    runtime_state_in(PathBuf::from(format!("/run/user/{uid}/tilo")))
}

fn runtime_state_in(dir: PathBuf) -> Option<RuntimeState> {
    let mut masked: Vec<PathBuf> = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .collect();
    masked.sort();
    Some(RuntimeState { dir, masked })
}

/// The consent-gated NTFS mount service's runtime directory, holding its
/// control socket (`ntfs_mount::protocol::SOCKET_PATH`). Bound rather than
/// the socket, like the cloud service's: the socket unit may be restarted,
/// and the directory exists from boot (tmpfiles). The service answers a
/// sandboxed caller only with what the person at the seat may do outside
/// the shell's prompt: list, close and forget drives, and change how a
/// remembered one opens.
pub const NTFS_MOUNT_DIR: &str = "/run/tilo-ntfs-mount";

/// The host sockets the control panel's clients use, for a session of `uid`:
/// `PipeWire`, and the cloud account service's runtime directory, which holds
/// its mount control socket (`runtime_fs::cloud::socket_in`; no credential
/// crosses it). The directory is bound rather than the socket: it lasts the
/// whole session (user tmpfiles, `RuntimeDirectoryPreserve=`), so a service
/// restart's new socket shows through. That socket only lists, syncs, pauses
/// and resumes mounts; file requests (revisions, restore, share links) are
/// served on the file socket in `tilo-fs-cloud-files`, which is not bound.
/// And [`NTFS_MOUNT_DIR`]. A path that does not exist (no such service on
/// this host) is left out by [`resolve`].
fn authority_sockets(uid: u32) -> Vec<PathBuf> {
    vec![
        PathBuf::from(format!("/run/user/{uid}/pipewire-0")),
        PathBuf::from(format!("/run/user/{uid}/tilo-fs-cloud")),
        PathBuf::from(NTFS_MOUNT_DIR),
    ]
}

/// Where an app looks for the person's app store: under the `XDG_DATA_HOME`
/// its sandbox environment names.
fn sandbox_user_apps() -> PathBuf {
    PathBuf::from(crate::appdir::SANDBOX_APP_DIR)
        .join(".local/share")
        .join("tilo-apps")
}

/// The upstream system bus address.
fn system_bus_address() -> String {
    std::env::var("DBUS_SYSTEM_BUS_ADDRESS")
        .ok()
        .filter(|address| !address.is_empty())
        .unwrap_or_else(|| format!("unix:path={SANDBOX_SYSTEM_BUS}"))
}

/// Plan the grant for `package_id`, starting its system-bus proxy. `None`
/// for every package that is not a control panel. The guard must outlive
/// the sandbox.
#[must_use]
pub fn resolve(
    package_id: &str,
    services: &std::collections::BTreeMap<String, crate::manifest::ServiceReq>,
    uid: u32,
) -> Option<(SystemAuthorityPlan, Option<DBusProxyGuard>)> {
    if !grants(package_id, services) {
        return None;
    }
    let mut plan = SystemAuthorityPlan {
        read_paths: HOST_READ_PATHS
            .iter()
            .map(PathBuf::from)
            .filter(|path| path.exists())
            .collect(),
        ..SystemAuthorityPlan::default()
    };
    for socket in authority_sockets(uid) {
        if socket.exists() {
            plan.sockets.push(SocketBind {
                sandbox: socket.clone(),
                host: socket,
            });
        }
    }
    plan.runtime_state = runtime_state(uid);
    plan.user_apps = package_store::layout::default_user_root()
        .filter(|store| store.is_dir())
        .map(|host| SocketBind {
            host,
            sandbox: sandbox_user_apps(),
        });
    let guard = spawn_system_bus_proxy();
    if let Some(guard) = &guard {
        plan.sockets.push(SocketBind {
            host: guard.socket_path().to_path_buf(),
            sandbox: PathBuf::from(SANDBOX_SYSTEM_BUS),
        });
        plan.system_bus = true;
    }
    tracing::info!(
        package_id,
        read_paths = plan.read_paths.len(),
        sockets = plan.sockets.len(),
        system_bus = plan.system_bus,
        "system authority grant resolved"
    );
    Some((plan, guard))
}

fn spawn_system_bus_proxy() -> Option<DBusProxyGuard> {
    let Some(binary) = dbus_proxy::find_proxy_binary() else {
        tracing::warn!("xdg-dbus-proxy unavailable; the system bus stays unavailable");
        return None;
    };
    let socket = dbus_proxy::system_proxy_socket_path(
        &dbus_proxy::session_runtime_dir(),
        std::process::id(),
    );
    match dbus_proxy::spawn_filtered_proxy(
        &binary,
        &system_bus_address(),
        &socket,
        SYSTEM_BUS_FILTER_ARGS,
    ) {
        Ok(guard) => Some(guard),
        Err(error) => {
            tracing::warn!(%error, "system bus proxy failed to spawn; the system bus stays unavailable");
            None
        }
    }
}

/// Bind the plan into a bwrap argv (before its entrypoint token).
pub fn push_binds(argv: &mut Vec<String>, plan: &SystemAuthorityPlan) {
    for path in &plan.read_paths {
        let path = path.display().to_string();
        argv.extend(["--ro-bind".to_string(), path.clone(), path]);
    }
    for socket in &plan.sockets {
        argv.extend([
            "--bind".to_string(),
            socket.host.display().to_string(),
            socket.sandbox.display().to_string(),
        ]);
    }
    if let Some(state) = &plan.runtime_state {
        let dir = state.dir.display().to_string();
        argv.extend(["--ro-bind".to_string(), dir.clone(), dir]);
        for masked in &state.masked {
            argv.extend(["--tmpfs".to_string(), masked.display().to_string()]);
        }
    }
    if let Some(store) = &plan.user_apps {
        let host = store.host.display().to_string();
        argv.extend(["--ro-bind".to_string(), host.clone(), host.clone()]);
        argv.extend([
            "--ro-bind".to_string(),
            host,
            store.sandbox.display().to_string(),
        ]);
    }
    if plan.system_bus {
        argv.extend([
            "--setenv".to_string(),
            "DBUS_SYSTEM_BUS_ADDRESS".to_string(),
            format!("unix:path={SANDBOX_SYSTEM_BUS}"),
        ]);
    }
}

/// Landlock rules for the same paths: read and execute on the bound trees,
/// read and write on the sockets (`connect(2)` is write-flavoured).
pub fn push_landlock_rules(argv: &mut Vec<String>, plan: &SystemAuthorityPlan) {
    for path in &plan.read_paths {
        argv.extend(["--ro".to_string(), path.display().to_string()]);
    }
    for socket in &plan.sockets {
        argv.extend(["--rw".to_string(), socket.sandbox.display().to_string()]);
    }
    if let Some(state) = &plan.runtime_state {
        argv.extend(["--ro".to_string(), state.dir.display().to_string()]);
    }
    if let Some(store) = &plan.user_apps {
        argv.extend(["--ro".to_string(), store.host.display().to_string()]);
        argv.extend(["--ro".to_string(), store.sandbox.display().to_string()]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_control_panel_receives_the_grant() {
        let declared: std::collections::BTreeMap<String, crate::manifest::ServiceReq> = [(
            "system".to_owned(),
            crate::manifest::service_req("required@1"),
        )]
        .into();
        assert!(is_client("so.tilo.settings"));
        assert!(grants("so.tilo.settings", &declared));
        // Settings without the system interface declared is not served it.
        assert!(!grants(
            "so.tilo.settings",
            &std::collections::BTreeMap::default()
        ));
        assert!(
            resolve(
                "so.tilo.settings",
                &std::collections::BTreeMap::default(),
                1000
            )
            .is_none()
        );
        for other in [
            "so.tilo.files",
            "so.tilo.settings.evil",
            "",
            "so.tilo.browser",
        ] {
            assert!(!is_client(other), "{other}");
            assert!(!grants(other, &declared), "{other}");
            assert!(resolve(other, &declared, 1000).is_none(), "{other}");
        }
    }

    #[test]
    fn the_cloud_grant_reaches_account_control_and_never_file_requests() {
        let sockets = authority_sockets(1000);
        assert!(sockets.contains(&PathBuf::from("/run/user/1000/tilo-fs-cloud")));
        let files = std::path::Path::new("/run/user/1000/tilo-fs-cloud-files/files.sock");
        assert!(
            !sockets.iter().any(|bound| files.starts_with(bound)),
            "Settings' grant reaches the cloud file socket: {sockets:?}"
        );
    }

    #[test]
    fn the_ntfs_grant_reaches_the_mount_service_socket_for_settings_only() {
        let sockets = authority_sockets(1000);
        let control = std::path::Path::new("/run/tilo-ntfs-mount/control.sock");
        assert!(
            sockets.iter().any(|bound| control.starts_with(bound)),
            "Settings cannot reach the NTFS mount service: {sockets:?}"
        );
        // Every other package gets no plan at all, so no path of it.
        let declared: std::collections::BTreeMap<String, crate::manifest::ServiceReq> = [(
            "system".to_owned(),
            crate::manifest::service_req("required@1"),
        )]
        .into();
        assert!(resolve("so.tilo.file-explorer", &declared, 1000).is_none());
    }

    #[test]
    fn the_system_bus_filter_names_authorities_and_never_opens_the_whole_bus() {
        assert_eq!(SYSTEM_BUS_FILTER_ARGS.first(), Some(&"--filter"));
        for arg in SYSTEM_BUS_FILTER_ARGS.iter().skip(1) {
            let name = arg
                .strip_prefix("--talk=")
                .or_else(|| arg.strip_prefix("--see="))
                .unwrap_or_else(|| panic!("unexpected rule {arg}"));
            assert!(!name.contains('*'), "{arg} must name one service");
            assert!(name.contains('.'), "{arg}");
        }
        assert!(SYSTEM_BUS_FILTER_ARGS.contains(&"--talk=org.freedesktop.NetworkManager"));
    }

    #[test]
    fn binds_and_landlock_rules_cover_the_same_paths() {
        let plan = SystemAuthorityPlan {
            read_paths: vec!["/usr/bin".into(), "/etc".into()],
            sockets: vec![SocketBind {
                host: "/run/user/1000/tilo/dbus-proxy/7-system.sock".into(),
                sandbox: SANDBOX_SYSTEM_BUS.into(),
            }],
            system_bus: true,
            runtime_state: Some(RuntimeState {
                dir: "/run/user/1000/tilo".into(),
                masked: vec!["/run/user/1000/tilo/services".into()],
            }),
            user_apps: Some(SocketBind {
                host: "/home/a/.local/share/tilo-apps".into(),
                sandbox: sandbox_user_apps(),
            }),
        };
        let mut binds = Vec::new();
        push_binds(&mut binds, &plan);
        assert_eq!(
            binds,
            [
                "--ro-bind",
                "/usr/bin",
                "/usr/bin",
                "--ro-bind",
                "/etc",
                "/etc",
                "--bind",
                "/run/user/1000/tilo/dbus-proxy/7-system.sock",
                SANDBOX_SYSTEM_BUS,
                "--ro-bind",
                "/run/user/1000/tilo",
                "/run/user/1000/tilo",
                "--tmpfs",
                "/run/user/1000/tilo/services",
                "--ro-bind",
                "/home/a/.local/share/tilo-apps",
                "/home/a/.local/share/tilo-apps",
                "--ro-bind",
                "/home/a/.local/share/tilo-apps",
                "/run/tilo/app/.local/share/tilo-apps",
                "--setenv",
                "DBUS_SYSTEM_BUS_ADDRESS",
                "unix:path=/run/dbus/system_bus_socket",
            ]
        );
        let mut rules = Vec::new();
        push_landlock_rules(&mut rules, &plan);
        assert_eq!(
            rules,
            [
                "--ro",
                "/usr/bin",
                "--ro",
                "/etc",
                "--rw",
                SANDBOX_SYSTEM_BUS,
                "--ro",
                "/run/user/1000/tilo",
                "--ro",
                "/home/a/.local/share/tilo-apps",
                "--ro",
                "/run/tilo/app/.local/share/tilo-apps",
            ]
        );
    }

    #[test]
    fn systemd_clients_receive_the_boot_marker_without_the_manager_socket() {
        let plan = SystemAuthorityPlan {
            read_paths: HOST_READ_PATHS.iter().map(PathBuf::from).collect(),
            ..SystemAuthorityPlan::default()
        };
        let mut binds = Vec::new();
        push_binds(&mut binds, &plan);
        assert!(
            binds.windows(3).any(|args| {
                args == ["--ro-bind", "/run/systemd/system", "/run/systemd/system"]
            })
        );
        let mut rules = Vec::new();
        push_landlock_rules(&mut rules, &plan);
        assert!(
            rules
                .windows(2)
                .any(|args| args == ["--ro", "/run/systemd/system"])
        );
        for private in ["/run/systemd/private", "/run/systemd", "/run/dbus"] {
            assert!(!binds.iter().any(|arg| arg == private));
            assert!(!rules.iter().any(|arg| arg == private));
        }
    }

    #[test]
    fn the_runtime_state_shows_its_files_and_hides_every_subdirectory() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("tilo");
        for sub in ["capabilities", "identity-projections", "services"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        std::fs::write(dir.join("display-state.json"), b"{}").unwrap();
        let state = runtime_state_in(dir.clone()).unwrap();
        assert_eq!(state.dir, dir);
        assert_eq!(
            state.masked,
            ["capabilities", "identity-projections", "services"].map(|sub| dir.join(sub))
        );
        assert!(runtime_state_in(root.path().join("absent")).is_none());
    }
}
