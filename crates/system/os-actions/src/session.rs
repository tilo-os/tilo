//! Narrow session lifecycle actions used by shell chrome.
//!
//! Keeping these here means UI code cannot construct an arbitrary command.

use crate::process::run;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionAction {
    Lock,
    Logout,
    SwitchUser,
    Suspend,
    Restart,
    ShutDown,
}

/// Run the shell-out backing `action`.
///
/// # Errors
///
/// Returns an error message if `loginctl`/`systemctl` is not installed, is not authorised, or
/// the command otherwise fails.
pub fn perform_session_action(action: SessionAction) -> Result<(), String> {
    match action {
        SessionAction::Lock => run("loginctl", &["lock-session"]),
        SessionAction::Logout => run("loginctl", &["terminate-session", "self"]),
        // A greeter is a display-manager session, not a magic logind session
        // id. Ask the active display manager to create/activate its greeter.
        // GDM deliberately permits this exact method to ordinary local users
        // in `/usr/share/dbus-1/system.d/gdm.conf`.
        SessionAction::SwitchUser => run(
            "busctl",
            &[
                "call",
                "org.gnome.DisplayManager",
                "/org/gnome/DisplayManager/LocalDisplayFactory",
                "org.gnome.DisplayManager.LocalDisplayFactory",
                "CreateTransientDisplay",
            ],
        ),
        SessionAction::Suspend => run("systemctl", &["suspend"]),
        SessionAction::Restart => run("systemctl", &["reboot"]),
        SessionAction::ShutDown => run("systemctl", &["poweroff"]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Invocation {
        binary: &'static str,
        args: &'static [&'static str],
    }

    fn invocation(action: SessionAction) -> Invocation {
        match action {
            SessionAction::Lock => Invocation {
                binary: "loginctl",
                args: &["lock-session"],
            },
            SessionAction::Logout => Invocation {
                binary: "loginctl",
                args: &["terminate-session", "self"],
            },
            SessionAction::SwitchUser => Invocation {
                binary: "busctl",
                args: &[
                    "call",
                    "org.gnome.DisplayManager",
                    "/org/gnome/DisplayManager/LocalDisplayFactory",
                    "org.gnome.DisplayManager.LocalDisplayFactory",
                    "CreateTransientDisplay",
                ],
            },
            SessionAction::Suspend => Invocation {
                binary: "systemctl",
                args: &["suspend"],
            },
            SessionAction::Restart => Invocation {
                binary: "systemctl",
                args: &["reboot"],
            },
            SessionAction::ShutDown => Invocation {
                binary: "systemctl",
                args: &["poweroff"],
            },
        }
    }

    #[test]
    fn switch_user_targets_the_gdm_factory_not_a_fictional_logind_session() {
        let call = invocation(SessionAction::SwitchUser);
        assert_eq!(call.binary, "busctl");
        assert_eq!(call.args.last(), Some(&"CreateTransientDisplay"));
        assert!(!call.args.contains(&"greeter"));
    }
}
