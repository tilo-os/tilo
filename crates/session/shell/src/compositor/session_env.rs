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
//! `DISPLAY` is deliberately not exported: each workspace has its own Xwayland,
//! so there is no session-wide X11 display a workspace-less service could be
//! given without reaching into some workspace's windows.

use std::path::Path;

/// The desktop name exported as `XDG_CURRENT_DESKTOP`.
pub const CURRENT_DESKTOP: &str = "tilo";

const UPDATE_PROGRAM: &str = "/usr/bin/dbus-update-activation-environment";

/// What a Tilo session exports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEnvironment {
    /// File name of the compositor's socket in `XDG_RUNTIME_DIR`, e.g. `wayland-1`.
    pub wayland_display: String,
}

impl SessionEnvironment {
    /// The exact `KEY=value` assignments, in a stable order.
    pub fn assignments(&self) -> Vec<String> {
        vec![
            format!("WAYLAND_DISPLAY={}", self.wayland_display),
            format!("XDG_CURRENT_DESKTOP={CURRENT_DESKTOP}"),
            format!("XDG_SESSION_DESKTOP={CURRENT_DESKTOP}"),
            "XDG_SESSION_TYPE=wayland".to_string(),
        ]
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
    std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|value| value == CURRENT_DESKTOP)
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
    let environment = SessionEnvironment {
        wayland_display: name.to_string_lossy().into_owned(),
    };
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
                "XDG_CURRENT_DESKTOP=tilo",
                "XDG_SESSION_DESKTOP=tilo",
                "XDG_SESSION_TYPE=wayland",
            ]
        );
    }
}
