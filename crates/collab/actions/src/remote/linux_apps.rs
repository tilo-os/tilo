//! A workspace's Linux GUI apps, opened in the workspace's VM on the member
//! device that runs it (`docs/hosted-browser.md`, "Linux apps").
//!
//! A hosted workspace's VM runs the workspace's Linux programs. A program
//! with windows needs a display, and the person needs its windows on their
//! desk, so the first Linux app opened starts a headless Tilo session in the
//! VM: as the workspace's tenant (never as Browser's account), with Xwayland
//! for X11 programs. That session joins the workspace as a device of it, the
//! way a hosted Browser's session does ([`super::browser`]), and every window
//! it runs is shared with the workspace's members, who see it on their desk
//! and drive it with their keyboard and pointer.
//!
//! The relay asks the member device over the device plane, as it asks for
//! Browser:
//!
//! - [`target`] names an open, an action of the workspace's environment
//!   ([`super::browser::RESOURCE`]); the argument is an [`OpenApp`]. It
//!   succeeds with a [`super::browser::OpenedBrowser`] once the session is up
//!   in the workspace and was told to start the app; the app's window joins
//!   the desk as it maps.
//! - [`STOP`] ends the session and every app in it.
//!
//! The session in the VM is started with [`MARK`] in its environment and
//! reads its standard input a line at a time: the first line is the
//! workspace's material (the relay's, the same as Browser's),
//! and every later one is an [`AppLaunch`] in JSON, one app to start. It
//! reports on its standard output as Browser's does
//! ([`super::browser::Report`]): `ready`, `up linux` once it has joined, and
//! `used` while its windows are open.
//!
//! A Linux app is a Linux action: it needs a card on a host that sells VMs,
//! and an account with none is refused before anything starts.

use serde::{Deserialize, Serialize};

use super::RemoteActionTarget;
use super::browser::{PROVIDER, RESOURCE};
use crate::door::ActionAddress;

/// The app a hosted workspace opens its Linux apps through:
/// `app.so.tilo.linux#open`, naming the app to start.
pub const APP: &str = "so.tilo.linux";
/// What people read it as.
pub const TITLE: &str = "Linux apps";
/// Start a Linux app in a workspace's VM, starting its apps session first
/// when none runs.
pub const OPEN: &str = "open-linux-app";
/// End the apps session in a workspace's VM, and every app in it.
pub const STOP: &str = "stop-linux-apps";
/// Set in the apps session's own environment: it serves a workspace's Linux
/// apps, reads what to start from its standard input and reports on its
/// standard output.
pub const MARK: &str = "TILO_HOSTED_APPS";
/// The provider an apps session reports itself up with: it holds no one
/// window, so nothing is addressed to it.
pub const UP_PROVIDER: &str = "linux";
/// The argument of `app.so.tilo.linux#open` naming the app's desktop file.
pub const APP_ARGUMENT: &str = "app";
/// The failure code of an open the account's plan refused: its details are
/// the plans' typed refusal ([`super::host_account::Refusal`]), which says
/// what would let it through.
pub const REFUSED: &str = "refused";

/// What [`OPEN`] is invoked on.
#[must_use]
pub fn target() -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, OPEN),
    }
}

/// What [`STOP`] is invoked on.
#[must_use]
pub fn stop_target() -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, STOP),
    }
}

/// One Linux app to start in the apps session: the desktop file it is
/// installed with (`org.gnome.TextEditor`, `xterm`), as the guest's package
/// manager put it there. What it runs is what that file says, so nothing
/// here is a command line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppLaunch {
    /// The desktop file's id, without `.desktop`.
    pub desktop_id: String,
}

impl AppLaunch {
    /// The launch of the app `desktop_id` names.
    ///
    /// # Errors
    ///
    /// The words to refuse it with: the id is empty, too long, or holds
    /// anything but the letters, digits, `.`, `-` and `_` a desktop file's
    /// name is made of.
    pub fn new(desktop_id: &str) -> Result<Self, String> {
        let desktop_id = desktop_id.trim();
        let desktop_id = desktop_id.strip_suffix(".desktop").unwrap_or(desktop_id);
        if desktop_id.is_empty() {
            return Err(
                "name the Linux app to open: its desktop file's id, such as \
                        org.gnome.TextEditor"
                    .to_owned(),
            );
        }
        if desktop_id.len() > 255
            || desktop_id.starts_with(['.', '-'])
            || !desktop_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        {
            return Err(format!(
                "{desktop_id:?} is not a desktop file's id: letters, digits, '.', '-' and '_'"
            ));
        }
        Ok(Self {
            desktop_id: desktop_id.to_owned(),
        })
    }

    /// The line the apps session reads it from.
    #[must_use]
    pub fn line(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// The launch `line` names, when it names one.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let read: Self = serde_json::from_str(line.trim()).ok()?;
        Self::new(&read.desktop_id).ok()
    }
}

/// A workspace's Linux app to open, and what its apps session joins the
/// workspace with when none runs yet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenApp {
    /// The workspace whose VM runs it: the caller's.
    pub workspace: String,
    /// Who the machine's time is booked to, as the relay knows the account.
    #[serde(default)]
    pub account: String,
    /// What the session joins the workspace with, opaque to the host running
    /// it and to the VM service: the first line of the session's input.
    pub material: String,
    /// The app to start.
    pub launch: AppLaunch,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_app_open_names_the_environments_own_action() {
        assert_eq!(target().provider, PROVIDER);
        assert_eq!(target().address.resource, RESOURCE);
        assert_eq!(target().address.action, OPEN);
        assert_eq!(stop_target().address.action, STOP);
        assert_ne!(OPEN, super::super::browser::OPEN);
    }

    #[test]
    fn an_app_is_named_by_its_desktop_file_and_nothing_else() {
        assert_eq!(
            AppLaunch::new(" org.gnome.TextEditor.desktop ").unwrap(),
            AppLaunch {
                desktop_id: "org.gnome.TextEditor".into()
            }
        );
        assert_eq!(AppLaunch::new("xterm").unwrap().desktop_id, "xterm");
        for refused in [
            "",
            "  ",
            "xterm -e sh",
            "../xterm",
            "-x",
            "a;b",
            "a/b",
            "émacs",
        ] {
            assert!(AppLaunch::new(refused).is_err(), "{refused:?}");
        }
    }

    #[test]
    fn a_launch_travels_as_one_line_and_reads_back() {
        let launch = AppLaunch::new("org.gnome.TextEditor").unwrap();
        let line = launch.line();
        assert!(!line.contains('\n'));
        assert_eq!(AppLaunch::parse(&line), Some(launch));
        // A line that names something other than a desktop file starts
        // nothing.
        assert_eq!(AppLaunch::parse(r#"{"desktop_id":"sh -c id"}"#), None);
        assert_eq!(AppLaunch::parse("xterm"), None);
    }
}
