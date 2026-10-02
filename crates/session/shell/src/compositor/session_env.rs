//! The session's display variables, handed to the systemd user manager and D-Bus
//! activation.
//!
//! greetd gives `XDG_CURRENT_DESKTOP` and friends to the compositor process only,
//! and `WAYLAND_DISPLAY` does not exist until the compositor binds its socket. A
//! D-Bus-activated service (a portal, a notification daemon, a GTK app opened by
//! `gio open`) or a systemd user unit started after login would otherwise come up
//! with no display. Once the socket is up the compositor exports them with
//! `dbus-update-activation-environment --systemd`, which updates both environments
//! in one call; `tilo-session` clears them on logout.
//!
//! `DISPLAY`/`XAUTHORITY` are exported for the *active* workspace once its
//! X11 socket exists, so a D-Bus-activated X11 app can connect. They are
//! updated when the active workspace's display changes, never invented for
//! a workspace that has no X server.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The desktop name Tilo itself announces.
pub const CURRENT_DESKTOP: &str = "tilo";
/// `XDG_CURRENT_DESKTOP` as a colon list: Tilo plus GNOME, so
/// `OnlyShowIn=GNOME` entries remain visible without claiming to *be* GNOME.
pub const CURRENT_DESKTOP_VALUE: &str = "tilo:GNOME";

static LAST_WAYLAND: Mutex<Option<String>> = Mutex::new(None);
static LAST_X11: Mutex<Option<(String, PathBuf)>> = Mutex::new(None);

const UPDATE_PROGRAM: &str = "/usr/bin/dbus-update-activation-environment";

/// What a Tilo session exports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEnvironment {
    /// File name of the compositor's socket in `XDG_RUNTIME_DIR`, e.g. `wayland-1`.
    pub wayland_display: String,
    /// Active workspace X11 display, when one has been bound.
    pub x11_display: Option<String>,
    /// Authority file for [`Self::x11_display`].
    pub x11_authority: Option<PathBuf>,
}

impl SessionEnvironment {
    /// The exact `KEY=value` assignments, in a stable order.
    pub fn assignments(&self) -> Vec<String> {
        let mut assignments = vec![
            format!("WAYLAND_DISPLAY={}", self.wayland_display),
            format!("XDG_CURRENT_DESKTOP={CURRENT_DESKTOP_VALUE}"),
            format!("XDG_SESSION_DESKTOP={CURRENT_DESKTOP}"),
            "XDG_SESSION_TYPE=wayland".to_string(),
        ];
        if let (Some(display), Some(authority)) = (&self.x11_display, &self.x11_authority) {
            assignments.push(format!("DISPLAY={display}"));
            assignments.push(format!("XAUTHORITY={}", authority.display()));
        }
        assignments
    }

    /// The command that publishes [`Self::assignments`] to both environments.
    pub fn command(&self) -> std::process::Command {
        let mut command = std::process::Command::new(UPDATE_PROGRAM);
        command.arg("--systemd").args(self.assignments());
        command
    }
}

/// Only a real Tilo login exports: `tilo-session` announces the desktop before
/// starting the compositor. A nested or host-desktop run must never repoint the
/// surrounding desktop's user manager at its own socket.
pub fn is_tilo_login() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|value| {
        value
            .split(':')
            .any(|part| part.eq_ignore_ascii_case(CURRENT_DESKTOP))
    })
}

/// Export the environment for `wayland_socket` on a worker thread; failures are
/// logged, never fatal — the session itself works without them.
pub fn export(wayland_socket: &Path) {
    if !is_tilo_login() {
        return;
    }
    let Some(name) = wayland_socket.file_name() else {
        return;
    };
    let wayland_display = name.to_string_lossy().into_owned();
    *LAST_WAYLAND
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(wayland_display.clone());
    let environment = SessionEnvironment {
        wayland_display,
        x11_display: None,
        x11_authority: None,
    };
    spawn_export(environment);
}

/// Publish the active workspace's X11 display for D-Bus activation.
pub fn export_x11(display: &str, authority: &Path) {
    if !is_tilo_login() {
        return;
    }
    let next = (display.to_string(), authority.to_path_buf());
    {
        let mut last = LAST_X11
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if last.as_ref() == Some(&next) {
            return;
        }
        *last = Some(next.clone());
    }
    let Some(wayland_display) = LAST_WAYLAND
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
    else {
        return;
    };
    spawn_export(SessionEnvironment {
        wayland_display,
        x11_display: Some(next.0),
        x11_authority: Some(next.1),
    });
}

fn spawn_export(environment: SessionEnvironment) {
    let _ = std::thread::Builder::new()
        .name("tilo-session-env".to_string())
        .spawn(move || match environment.command().status() {
            Ok(status) if status.success() => tracing::info!(
                assignments = ?environment.assignments(),
                "exported the display to systemd and D-Bus activation"
            ),
            Ok(status) => tracing::warn!(%status, "dbus-update-activation-environment failed"),
            Err(error) => {
                tracing::warn!(%error, "dbus-update-activation-environment could not run");
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wayland_socket_and_desktop_are_exported_with_systemd() {
        let environment = SessionEnvironment {
            wayland_display: "wayland-1".into(),
            x11_display: Some(":1042".into()),
            x11_authority: Some(PathBuf::from(
                "/run/user/1000/tilo-session/xwayland/Xauthority-1042",
            )),
        };
        let command = environment.command();
        assert_eq!(command.get_program(), UPDATE_PROGRAM);
        let args: Vec<_> = command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            vec![
                "--systemd",
                "WAYLAND_DISPLAY=wayland-1",
                "XDG_CURRENT_DESKTOP=tilo:GNOME",
                "XDG_SESSION_DESKTOP=tilo",
                "XDG_SESSION_TYPE=wayland",
                "DISPLAY=:1042",
                "XAUTHORITY=/run/user/1000/tilo-session/xwayland/Xauthority-1042",
            ]
        );
    }
}
