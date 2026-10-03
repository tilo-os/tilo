//! Turning a [`Launch`] into the guest process that runs it.

use std::path::{Path, PathBuf};

use legacy_agent_protocol::waypipe::{self, PORT_ENV};
use legacy_agent_protocol::{Display, Launch, WAYPIPE_PORT};

use crate::user::GuestUser;

/// The X display the guest's Xwayland serves forwarded X11 apps on.
pub const X_DISPLAY: &str = ":0";

/// Variables the agent owns. A launch cannot override them: they decide which
/// user the app runs as and where its windows go.
const AGENT_OWNED: &[&str] = &[
    "HOME",
    "USER",
    "LOGNAME",
    "XDG_RUNTIME_DIR",
    "WAYLAND_DISPLAY",
    "WAYLAND_SOCKET",
    "DISPLAY",
    "DBUS_SESSION_BUS_ADDRESS",
    PORT_ENV,
];

/// One guest process, ready to spawn.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Spec {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: PathBuf,
    /// Said on the app's stderr before it starts, when the requested
    /// directory is not visible in the guest.
    pub note: Option<String>,
}

/// What the guest looks like right now, for [`spec`].
pub struct GuestView<'a> {
    pub user: &'a GuestUser,
    /// `Some` once the forwarded Xwayland is serving.
    pub x_display: Option<&'a str>,
    pub session_bus: Option<&'a Path>,
    pub is_dir: &'a dyn Fn(&Path) -> bool,
}

/// The port the guest's forwarder connects to for this launch.
#[must_use]
pub fn waypipe_port(launch: &Launch) -> u32 {
    launch
        .env
        .iter()
        .find(|(name, _)| name == PORT_ENV)
        .and_then(|(_, port)| port.parse().ok())
        .unwrap_or(WAYPIPE_PORT)
}

#[must_use]
pub fn spec(launch: &Launch, guest: &GuestView<'_>) -> Spec {
    let user = guest.user;
    let argv = match launch.display {
        Display::Forwarded => waypipe::server_argv("waypipe", waypipe_port(launch), &launch.argv),
        Display::None => launch.argv.clone(),
    };
    let mut env = vec![
        ("HOME".to_string(), user.home.display().to_string()),
        ("USER".to_string(), user.name.clone()),
        ("LOGNAME".to_string(), user.name.clone()),
        ("SHELL".to_string(), user.shell.clone()),
        (
            "PATH".to_string(),
            "/usr/local/bin:/usr/bin:/bin:/var/lib/flatpak/exports/bin".to_string(),
        ),
        ("LANG".to_string(), "C.UTF-8".to_string()),
        (
            "XDG_RUNTIME_DIR".to_string(),
            user.runtime_dir().display().to_string(),
        ),
        (
            "XDG_DATA_DIRS".to_string(),
            format!(
                "{}/.local/share/flatpak/exports/share:/var/lib/flatpak/exports/share:/usr/local/share:/usr/share",
                user.home.display()
            ),
        ),
    ];
    if let Some(bus) = guest.session_bus {
        env.push((
            "DBUS_SESSION_BUS_ADDRESS".to_string(),
            format!("unix:path={}", bus.display()),
        ));
    }
    if launch.display == Display::Forwarded {
        // GTK and Qt pick Wayland from the display waypipe creates; an app
        // that only speaks X11 finds the forwarded Xwayland here.
        if let Some(display) = guest.x_display {
            env.push(("DISPLAY".to_string(), display.to_string()));
        }
    }
    for (name, value) in &launch.env {
        if AGENT_OWNED.contains(&name.as_str()) {
            continue;
        }
        env.retain(|(existing, _)| existing != name);
        env.push((name.clone(), value.clone()));
    }
    let (cwd, note) = match launch.cwd.as_deref().map(Path::new) {
        Some(cwd) if (guest.is_dir)(cwd) => (cwd.to_path_buf(), None),
        Some(cwd) => (
            user.home.clone(),
            Some(format!(
                "tilo-legacy: {} is not shared with this VM; starting in {}.\n\
                 Share it with `tilo-legacy grant {}`.\n",
                cwd.display(),
                user.home.display(),
                cwd.display()
            )),
        ),
        None => (user.home.clone(), None),
    };
    Spec {
        argv,
        env,
        cwd,
        note,
    }
}

/// The forwarded Xwayland: one per guest, under its own `waypipe server`.
#[must_use]
pub fn xwayland_argv(port: u32) -> Vec<String> {
    waypipe::server_argv(
        "waypipe",
        port,
        &["xwayland-satellite".to_string(), X_DISPLAY.to_string()],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use legacy_agent_protocol::Stdio;

    fn user() -> GuestUser {
        GuestUser {
            name: "tilo".into(),
            uid: 1000,
            gid: 1000,
            home: "/home/tilo".into(),
            shell: "/bin/bash".into(),
        }
    }

    fn launch(display: Display, cwd: &str) -> Launch {
        Launch {
            argv: vec!["code".into(), ".".into()],
            cwd: Some(cwd.into()),
            env: vec![
                ("HOME".into(), "/root".into()),
                ("LANG".into(), "de_DE.UTF-8".into()),
                (PORT_ENV.into(), "9001".into()),
            ],
            stdio: Stdio::Relay,
            display,
        }
    }

    fn get<'a>(spec: &'a Spec, name: &str) -> Option<&'a str> {
        spec.env
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    #[test]
    fn launches_run_as_the_guest_user_under_the_forwarder() {
        let user = user();
        let shared = |path: &Path| path.starts_with("/var/home/me");
        // phase: a windowed app runs under waypipe in the shared directory
        {
            let view = GuestView {
                user: &user,
                x_display: Some(X_DISPLAY),
                session_bus: Some(Path::new("/run/user/1000/bus")),
                is_dir: &shared,
            };
            let spec = spec(&launch(Display::Forwarded, "/var/home/me/src"), &view);
            assert_eq!(
                spec.argv,
                [
                    "waypipe", "--vsock", "-s", "2:9001", "server", "--", "code", "."
                ]
            );
            assert_eq!(spec.cwd, PathBuf::from("/var/home/me/src"));
            assert_eq!(spec.note, None);
            assert_eq!(
                get(&spec, "HOME"),
                Some("/home/tilo"),
                "the agent owns HOME"
            );
            assert_eq!(get(&spec, "LANG"), Some("de_DE.UTF-8"));
            assert_eq!(get(&spec, "DISPLAY"), Some(":0"));
            assert_eq!(get(&spec, PORT_ENV), None);
            assert_eq!(
                get(&spec, "DBUS_SESSION_BUS_ADDRESS"),
                Some("unix:path=/run/user/1000/bus")
            );
        }
        // phase: a tool runs bare, and an unshared cwd falls back to home with a note
        {
            let view = GuestView {
                user: &user,
                x_display: None,
                session_bus: None,
                is_dir: &shared,
            };
            let spec = spec(&launch(Display::None, "/srv/private"), &view);
            assert_eq!(spec.argv, ["code", "."]);
            assert_eq!(spec.cwd, PathBuf::from("/home/tilo"));
            assert!(
                spec.note
                    .as_deref()
                    .unwrap()
                    .contains("tilo-legacy grant /srv/private")
            );
            assert_eq!(get(&spec, "DISPLAY"), None);
        }
        // phase: the forwarded Xwayland is a waypipe server too
        {
            assert_eq!(
                xwayland_argv(9001),
                [
                    "waypipe",
                    "--vsock",
                    "-s",
                    "2:9001",
                    "server",
                    "--",
                    "xwayland-satellite",
                    ":0"
                ]
            );
        }
    }
}
