//! The environment a launched app starts in.
//!
//! A launched app is not started from whatever the session's own process
//! happens to carry: it is started from what the session knows about this
//! device, its displays, its bus and the person — nothing else. What it
//! must carry is
//!
//! - the compositor's socket, as `WAYLAND_DISPLAY` plus the `XDG_RUNTIME_DIR`
//!   it resolves against, so a window arrives on the display that started it;
//! - the rootless Xwayland display and its authority, for X11 clients;
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
    /// The rootless Xwayland display this session runs, as `:N`.
    pub x11_display: Option<String>,
    /// The MIT-MAGIC-COOKIE file for [`Self::x11_display`].
    pub x11_authority: Option<PathBuf>,
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
            x11_authority: self.x11_authority.clone(),
            ..LaunchEnv::default()
        }
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
        match (&self.x11_display, &self.x11_authority) {
            (Some(display), Some(authority)) => {
                variables.insert("DISPLAY".to_owned(), display.clone());
                variables.insert(
                    "XAUTHORITY".to_owned(),
                    authority.to_string_lossy().into_owned(),
                );
            }
            // An X11 display without its cookie is unusable and a stale
            // `DISPLAY` is worse than none: both are left out together.
            _ => {
                variables.remove("DISPLAY");
                variables.remove("XAUTHORITY");
            }
        }
        if let Some(address) = &self.session_bus {
            variables.insert("DBUS_SESSION_BUS_ADDRESS".to_owned(), address.clone());
        }
        variables.insert(
            "XDG_CURRENT_DESKTOP".to_owned(),
            desktop_entry::launch::XDG_CURRENT_DESKTOP.to_owned(),
        );
        variables.insert(
            "XDG_SESSION_DESKTOP".to_owned(),
            desktop_entry::scan::CURRENT_DESKTOP.to_owned(),
        );
        variables.insert("XDG_SESSION_TYPE".to_owned(), "wayland".to_owned());
        variables
    }
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
            "HOME" => Some("/home/you".into()),
            // Not on the allowlist: it describes the session's own
            // confinement, not the person.
            "TILO_WORKSPACE_ID" => Some("someone-elses-room".into()),
            _ => None,
        })
    }

    #[test]
    fn a_package_starts_with_the_persons_variables_and_the_sessions_displays() {
        let mut session = environment();
        session.wayland_socket = Some(PathBuf::from("/run/user/1000/wayland-2"));
        session.x11_display = Some(":7".into());
        session.x11_authority = Some(PathBuf::from("/run/user/1000/tilo/Xauthority"));
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
            ("HOME", "/home/you"),
            ("WAYLAND_DISPLAY", "wayland-2"),
            ("XDG_RUNTIME_DIR", "/run/user/1000"),
            ("DISPLAY", ":7"),
            ("XAUTHORITY", "/run/user/1000/tilo/Xauthority"),
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
            "a room is chosen per launch, never inherited"
        );
    }

    #[test]
    fn an_x11_display_and_its_cookie_are_handed_over_together_or_not_at_all() {
        let mut session = environment();
        session.x11_display = Some(":7".into());
        let variables = session.variables();
        assert!(!variables.contains_key("DISPLAY"));
        assert!(!variables.contains_key("XAUTHORITY"));

        let mut session = environment();
        session.x11_authority = Some("/run/user/1000/tilo/Xauthority".into());
        let variables = session.variables();
        assert!(!variables.contains_key("DISPLAY"));
        assert!(
            !variables.contains_key("XAUTHORITY"),
            "a cookie for no display is as stale as the display it belonged to"
        );
    }

    #[test]
    fn a_host_launch_takes_the_same_displays() {
        let session = SessionEnvironment {
            wayland_socket: Some("/run/user/1000/wayland-3".into()),
            x11_display: Some(":9".into()),
            x11_authority: Some("/run/user/1000/tilo/Xauthority".into()),
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
