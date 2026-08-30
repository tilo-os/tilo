//! Narrow session lifecycle actions used by shell chrome.
//!
//! Keeping these here means UI code cannot construct an arbitrary command.

use crate::process::run;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionAction {
    Lock,
    Logout,
    Suspend,
    Restart,
    ShutDown,
}

pub fn perform_session_action(action: SessionAction) -> Result<(), String> {
    match action {
        SessionAction::Lock => run("loginctl", &["lock-session"]),
        SessionAction::Logout => run("loginctl", &["terminate-session", "self"]),
        SessionAction::Suspend => run("systemctl", &["suspend"]),
        SessionAction::Restart => run("systemctl", &["reboot"]),
        SessionAction::ShutDown => run("systemctl", &["poweroff"]),
    }
}
