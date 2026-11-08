//! The session's display variables, handed to the systemd user manager and
//! D-Bus activation.
//!
//! greetd gives `XDG_CURRENT_DESKTOP` and friends to the session process
//! only, and `WAYLAND_DISPLAY` does not exist until the compositor binds its
//! socket. A D-Bus-activated service (a portal, a notification daemon, an
//! app opened by `gio open`) or a systemd user unit started after login
//! would otherwise come up with no display. Once the socket is up the
//! session exports them with `dbus-update-activation-environment --systemd`,
//! which updates both environments in one call; the login wrapper clears
//! them on logout.
//!
//! An X11 display is not exported: every workspace has its own, and only the
//! launch that belongs to a workspace is told that workspace's display.

const UPDATE_PROGRAM: &str = "/usr/bin/dbus-update-activation-environment";

/// What a Tilo session exports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEnvironment {
    /// The compositor's socket in `XDG_RUNTIME_DIR` , e.g. `wayland-1` .
    pub wayland_display: String,
}

impl SessionEnvironment {
    /// The exact `KEY=value` assignments, in a stable order.
    #[must_use]
    pub fn assignments(&self) -> Vec<String> {
        let mut assignments = vec![format!("WAYLAND_DISPLAY={}", self.wayland_display)];
        assignments.extend(
            tilo_app_launch::env::desktop_variables()
                .map(|(name, value)| format!("{name}={value}")),
        );
        assignments
    }

    /// The command that publishes [`Self::assignments`] to both environments.
    #[must_use]
    pub fn command(&self) -> std::process::Command {
        let mut command = std::process::Command::new(UPDATE_PROGRAM);
        command.arg("--systemd").args(self.assignments());
        command
    }

    /// Export the environment on a worker thread when this is a real Tilo
    /// login (the login wrapper names the desktop before starting the
    /// session): a nested run must never repoint the surrounding desktop's
    /// user manager at its own socket. Failures are logged, never fatal;
    /// the session itself works without them.
    pub fn export(self) {
        if !tilo_paths::login::names_tilo(std::env::var_os("XDG_CURRENT_DESKTOP").as_deref()) {
            return;
        }
        let exported = std::thread::Builder::new()
            .name("tilo-session-env".to_string())
            .spawn(move || match self.command().status() {
                Ok(status) if status.success() => tracing::info!(
                    assignments = ?self.assignments(),
                    "exported the display to systemd and D-Bus activation"
                ),
                Ok(status) => tracing::warn!(%status, "dbus-update-activation-environment failed"),
                Err(error) => {
                    tracing::warn!(%error, "dbus-update-activation-environment could not run");
                }
            });
        if let Err(error) = exported {
            tracing::warn!(%error, "the display could not be exported in the background");
        }
    }
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
                "XDG_CURRENT_DESKTOP=tilo:GNOME",
                "XDG_SESSION_DESKTOP=tilo",
                "XDG_SESSION_TYPE=wayland",
            ]
        );
    }
}
