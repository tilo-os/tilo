//! The environment a launched app starts in.
//!
//! A launched app is not started from whatever the session's own process
//! happens to carry: it is started from what the session knows about this
//! device, its displays, its bus and the person — nothing else. What it
//! must carry is
//!
//! - the compositor's socket, as `WAYLAND_DISPLAY` plus the `XDG_RUNTIME_DIR`
//!   it resolves against, so a window arrives on the display that started it;
//! - the display of the workspace the app is launched into, for X11 clients;
//! - the person's session bus, so the portal and notifications an app reaches
//!   are the session's own;
//! - the person's own variables: their id, workspace roots, apps data
//!   directory, identity root and peer path, which the launcher hands into
//!   the sandbox.
//!
//! The machine's own applications get the display handoff through
//! [`desktop_entry::launch::LaunchEnv`] instead: their environment is built by
//! that crate from an allowlist, deliberately without Tilo's own variables.

use std::collections::BTreeMap;
use std::path::PathBuf;

use desktop_entry::launch::LaunchEnv;

/// What the session passes on to a Tilo package it starts: variables it was
/// itself started with that describe the person and this device. Absent
/// ones are left absent rather than invented.
const FORWARDED: &[&str] = &[
    // Who the person is and where their state lives.
    "TILO_USER_ID",
    "TILO_WORKSPACE_ROOTS",
    "TILO_APPS_DATA_DIR",
    "TILO_IDENTITY_ROOT",
    "TILO_HOME_PEERS_PATH",
    // The registries and runtime of a session started from a build under
    // test, so the apps it launches are the ones that build made. Absent
    // from an installed session, where the installed directories are used.
    "TILO_APPS_SYSTEM_DIR",
    "TILO_APPS_SYSTEM_PAYLOAD_DIR",
    "TILO_RUNTIME_BUNDLE_DIR",
    "TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE",
    // The interfaces this session's tree serves. A session started from a
    // build under test runs the apps that build made, which speak the
    // interfaces beside the code; without this the launcher judges them
    // against the host image's declaration instead, and refuses every app
    // built for a newer Tilo than the machine underneath. Absent from an
    // installed session, where the image's own declaration is the truth.
    "TILO_SYSTEM_INTERFACES",
    // Their account and shell.
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "PATH",
    // Locale, and where their files, settings and caches are.
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_CTYPE",
    "LC_MESSAGES",
    "TZ",
    // How much an app (and the connector the system runs for one) logs, as the session's own
    // log says: a development session's log is all a connector without a window has.
    "RUST_LOG",
    // A run promised the product's font catalog, and the catalog itself: a
    // package installed outside the build directory cannot find it beside its
    // own executable.
    "TILO_REQUIRE_FONTS",
    "TILO_FONT_CATALOG",
    "TILO_FONT_ASSET_CACHE",
    // The document-media socket this session serves its apps, where it serves one that is not
    // the image's (`fs serve-media`, `docs/architecture/apps.md`): the launcher reads it to
    // scope every app's capability, and a session started from a build under test serves its
    // own, because the image's daemon speaks another build's wire.
    "TILO_FS_BLOB_SOCKET",
    "XDG_DATA_HOME",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
    // The per-user directory this session itself runs in: its sockets, its
    // service files and an app's own writable state live nowhere else.
    "XDG_RUNTIME_DIR",
    "XDG_DATA_DIRS",
    "XDG_CONFIG_DIRS",
    // Input-method bridges, which a toolkit reads from the environment.
    "XMODIFIERS",
    "GTK_IM_MODULE",
    "GTK_PATH",
    "QT_IM_MODULE",
    "QT_IM_MODULES",
    "SDL_IM_MODULE",
];

/// The session's displays and bus, and the variables a package it starts is
/// given.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionEnvironment {
    /// The compositor's Wayland socket, as an absolute path.
    pub wayland_socket: Option<PathBuf>,
    /// The display of the workspace this launch belongs to, as `:N`: each
    /// workspace has its own, and only apps started in it are told it.
    pub x11_display: Option<String>,
    /// The person's session bus, when the session serves one of its own.
    pub session_bus: Option<String>,
    /// The variables the session was started with that a package is handed.
    pub forwarded: BTreeMap<String, String>,
}

impl SessionEnvironment {
    /// The environment of the process that started this session.
    #[must_use]
    pub fn from_process() -> Self {
        Self::reading(|name| std::env::var_os(name).filter(|value| !value.is_empty()))
    }

    /// The environment for a session started with `lookup`'s variables: the
    /// allowlist of [`FORWARDED`] that `lookup` knows.
    #[must_use]
    pub fn reading(lookup: impl Fn(&str) -> Option<std::ffi::OsString>) -> Self {
        let mut forwarded = BTreeMap::new();
        for name in FORWARDED {
            if let Some(value) = lookup(name) {
                forwarded.insert((*name).to_owned(), value.to_string_lossy().into_owned());
            }
        }
        Self {
            forwarded,
            ..Self::default()
        }
    }

    /// What the host applications' launcher plans from: the same displays,
    /// as that crate takes them.
    #[must_use]
    pub fn host_launch_env(&self) -> LaunchEnv {
        LaunchEnv {
            wayland_socket: self.wayland_socket.clone(),
            x11_display: self.x11_display.clone(),
            ..LaunchEnv::default()
        }
    }

    /// The environment of a launch that draws nothing — an app's origin connector
    /// (`docs/architecture/apps.md` §1): this session's runtime directory and bus, the person's
    /// variables, and no display at all, so nothing it runs reaches a compositor or an X server.
    #[must_use]
    pub fn windowless_variables(&self) -> BTreeMap<String, String> {
        let mut variables = self.variables();
        for display in ["WAYLAND_DISPLAY", "DISPLAY", "XAUTHORITY"] {
            variables.remove(display);
        }
        variables
    }

    /// The whole environment a Tilo package's launcher is started with.
    /// [`plan::Planned`] spawns it with `env_clear()` and exactly this, so a
    /// stale `WAYLAND_DISPLAY` or the session's own `DISPLAY` can never
    /// reach an app the session did not mean to hand them to.
    #[must_use]
    pub fn variables(&self) -> BTreeMap<String, String> {
        let mut variables = self.forwarded.clone();
        // The two halves move together: `WAYLAND_DISPLAY` resolves relative
        // to `XDG_RUNTIME_DIR`, and mixing sources is how an app connects to
        // a different compositor.
        if let Some(socket) = self.wayland_socket.as_deref() {
            if let Some(name) = socket.file_name() {
                variables.insert(
                    "WAYLAND_DISPLAY".to_owned(),
                    name.to_string_lossy().into_owned(),
                );
            }
            if let Some(directory) = socket.parent() {
                variables.insert(
                    "XDG_RUNTIME_DIR".to_owned(),
                    directory.to_string_lossy().into_owned(),
                );
            }
        } else if let Some(directory) = self.forwarded.get("XDG_RUNTIME_DIR") {
            variables.insert("XDG_RUNTIME_DIR".to_owned(), directory.clone());
        }
        // A workspace's display authenticates no one — the session binds its
        // sockets itself — so there is no cookie file to hand on. A stale
        // `DISPLAY` or `XAUTHORITY` from the session's own environment is
        // worse than none: both are left out when no display is named.
        variables.remove("XAUTHORITY");
        if let Some(display) = &self.x11_display {
            variables.insert("DISPLAY".to_owned(), display.clone());
        } else {
            variables.remove("DISPLAY");
        }
        if let Some(address) = &self.session_bus {
            variables.insert("DBUS_SESSION_BUS_ADDRESS".to_owned(), address.clone());
        }
        for (name, value) in desktop_variables() {
            variables.insert(name.to_owned(), value.to_owned());
        }
        variables
    }
}

/// The variables naming the desktop a process of this session runs on.
///
/// They say Tilo is the desktop, so an app reads its appearance and its menu
/// entries from the session's settings rather than from the desktop around a
/// nested session: a process started without them is treated as a guest of
/// whatever desktop it happens to run inside.
#[must_use]
pub fn desktop_variables() -> [(&'static str, &'static str); 3] {
    [
        (
            "XDG_CURRENT_DESKTOP",
            desktop_entry::launch::XDG_CURRENT_DESKTOP,
        ),
        ("XDG_SESSION_DESKTOP", desktop_entry::scan::CURRENT_DESKTOP),
        ("XDG_SESSION_TYPE", "wayland"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn environment() -> SessionEnvironment {
        SessionEnvironment::reading(|name| match name {
            "TILO_USER_ID" => Some("person-7".into()),
            "TILO_APPS_DATA_DIR" => Some("/home/you/.local/share/tilo/apps".into()),
            "TILO_IDENTITY_ROOT" => Some("/home/you/.local/share/tilo/identity".into()),
            "TILO_WORKSPACE_ROOTS" => Some("/home/you/.local/state/tilo/files/mount".into()),
            "TILO_HOME_PEERS_PATH" => Some("/run/user/1000/tilo/peers".into()),
            // A session started from a build under test: the tree's own
            // declaration, which the launcher judges its launches against.
            "TILO_SYSTEM_INTERFACES" => {
                Some("/src/tilo/os/usr/share/tilo/system-interfaces.toml".into())
            }
            "HOME" => Some("/home/you".into()),
            "RUST_LOG" => Some("warn,tilo_ui=debug".into()),
            // Not on the allowlist: it describes the session's own
            // confinement, not the person.
            "TILO_WORKSPACE_ID" => Some("someone-elses-workspace".into()),
            _ => None,
        })
    }

    #[test]
    fn a_package_starts_with_the_persons_variables_and_the_sessions_displays() {
        let mut session = environment();
        session.wayland_socket = Some(PathBuf::from("/run/user/1000/wayland-2"));
        session.x11_display = Some(":7".into());
        session.session_bus = Some("unix:path=/run/user/1000/tilo/bus".into());
        let variables = session.variables();
        for (name, value) in [
            ("TILO_USER_ID", "person-7"),
            ("TILO_APPS_DATA_DIR", "/home/you/.local/share/tilo/apps"),
            ("TILO_IDENTITY_ROOT", "/home/you/.local/share/tilo/identity"),
            (
                "TILO_WORKSPACE_ROOTS",
                "/home/you/.local/state/tilo/files/mount",
            ),
            ("TILO_HOME_PEERS_PATH", "/run/user/1000/tilo/peers"),
            (
                "TILO_SYSTEM_INTERFACES",
                "/src/tilo/os/usr/share/tilo/system-interfaces.toml",
            ),
            ("HOME", "/home/you"),
            ("RUST_LOG", "warn,tilo_ui=debug"),
            ("WAYLAND_DISPLAY", "wayland-2"),
            ("XDG_RUNTIME_DIR", "/run/user/1000"),
            ("DISPLAY", ":7"),
            (
                "DBUS_SESSION_BUS_ADDRESS",
                "unix:path=/run/user/1000/tilo/bus",
            ),
            ("XDG_CURRENT_DESKTOP", "tilo:GNOME"),
            ("XDG_SESSION_TYPE", "wayland"),
        ] {
            assert_eq!(
                variables.get(name).map(String::as_str),
                Some(value),
                "{name}"
            );
        }
        assert!(
            !variables.contains_key("TILO_WORKSPACE_ID"),
            "a workspace is chosen per launch, never inherited"
        );
    }

    /// The document-media socket a session serves its apps is the session's own, and every
    /// app it starts must be told it: the launcher falls back to the installed
    /// `/run/tilo/fs-blobs.sock` when it is not handed one, and a session started from a build
    /// under test then hands its apps a daemon of another build, whose wire they do not speak.
    #[test]
    fn a_package_is_told_the_session_s_own_document_media_socket() {
        let socket = "/tmp/run/fs-blobs.sock";
        let session = SessionEnvironment::reading(|name| match name {
            "TILO_FS_BLOB_SOCKET" => Some(socket.into()),
            _ => None,
        });
        assert_eq!(
            session
                .variables()
                .get("TILO_FS_BLOB_SOCKET")
                .map(String::as_str),
            Some(socket),
            "an app reads its content through the socket this session serves"
        );
    }

    /// A package installed outside the build directory finds the catalog the
    /// session verified, not one beside an executable that has none.
    #[test]
    fn a_package_is_told_the_catalog_the_session_verified() {
        let session = SessionEnvironment::reading(|name| match name {
            "TILO_FONT_CATALOG" => Some("/var/tmp/fonts".into()),
            "TILO_FONT_ASSET_CACHE" => Some("/var/tmp/artifacts".into()),
            "TILO_REQUIRE_FONTS" => Some("1".into()),
            _ => None,
        });
        let variables = session.variables();
        for (name, value) in [
            ("TILO_FONT_CATALOG", "/var/tmp/fonts"),
            ("TILO_FONT_ASSET_CACHE", "/var/tmp/artifacts"),
            ("TILO_REQUIRE_FONTS", "1"),
        ] {
            assert_eq!(
                variables.get(name).map(String::as_str),
                Some(value),
                "{name}"
            );
        }
    }

    #[test]
    fn the_workspace_display_is_handed_on_and_no_cookie_is() {
        let mut session = environment();
        session.x11_display = Some(":7".into());
        let variables = session.variables();
        assert_eq!(variables.get("DISPLAY").map(String::as_str), Some(":7"));
        assert!(
            !variables.contains_key("XAUTHORITY"),
            "a workspace display authenticates no one"
        );

        let session = environment();
        let variables = session.variables();
        assert!(!variables.contains_key("DISPLAY"));
        assert!(!variables.contains_key("XAUTHORITY"));
    }

    #[test]
    fn a_host_launch_takes_the_same_displays() {
        let session = SessionEnvironment {
            wayland_socket: Some("/run/user/1000/wayland-3".into()),
            x11_display: Some(":9".into()),
            ..SessionEnvironment::default()
        };
        let env = session.host_launch_env();
        assert_eq!(
            env.wayland_socket.as_deref(),
            Some(std::path::Path::new("/run/user/1000/wayland-3"))
        );
        assert_eq!(env.x11_display.as_deref(), Some(":9"));
        assert!(env.files.is_empty() && env.urls.is_empty());
    }
}
