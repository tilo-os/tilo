//! A workspace's Linux GUI apps, opened in the workspace's VM on the member
//! device that runs it (`docs/hosted-browser.md`, "Linux apps").
//!
//! A hosted workspace's VM runs the workspace's Linux programs. A program
//! with windows needs a display, and the person needs its windows in their
//! shell, so the first Linux app opened starts a headless Tilo session in the
//! VM: as the workspace's tenant (never as Browser's account), with Xwayland
//! for X11 programs. That session joins the workspace as a device of it, the
//! way a hosted Browser's session does ([`super::browser`]), and every window
//! it runs is shared with the workspace's members, who see it in their shell
//! and drive it with their keyboard and pointer.
//!
//! The server asks the member device over the device action service, as it asks for
//! Browser:
//!
//! - [`target`] names an open, an action of the workspace's environment
//!   ([`super::browser::RESOURCE`]); the argument is an [`OpenApp`]. It
//!   succeeds with a [`super::browser::OpenedBrowser`] once the VM's daemon
//!   logged in, and the app is then launched on that daemon
//!   ([`super::environment_window`]); the app's window joins the shell as it
//!   maps.
//! - [`STOP`] ends the VM's daemon and every app in it.
//!
//! The session the daemon starts for the apps carries [`MARK`] in its
//! environment, takes each app to start from the daemon, and says it is in use
//! while its windows are open.
//!
//! A Linux app is a Linux action: it needs a card on a host that sells VMs,
//! and an account with none is refused before anything starts.

use serde::{Deserialize, Serialize};

use super::RemoteActionTarget;
use super::browser::{PROVIDER, RESOURCE};
use crate::api::ActionAddress;

/// The app a hosted workspace opens its Linux apps through:
/// `app.so.tilo.linux#open`, naming the app to start.
pub const APP: &str = "so.tilo.linux";
/// What people read it as.
pub const TITLE: &str = "Linux apps";
/// Start a Linux app in a workspace's VM, starting its apps session first
/// when none runs.
pub const OPEN: &str = "open-linux-app";
/// Set in the environment of the session a workspace VM's daemon starts for its Linux apps.
pub const MARK: &str = "TILO_HOSTED_APPS";
/// The provider an apps session is up under: it holds no one window, so
/// nothing is addressed to it.
pub const UP_PROVIDER: &str = "linux";
/// The argument of `app.so.tilo.linux#open` naming the app's desktop file.
pub const APP_ARGUMENT: &str = "app";
/// The failure code of an open the account's plan refused: its details are
/// the plans' typed refusal ([`super::server_account::Refusal`]), which says
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
}

/// A workspace VM to start for its Linux apps, with the placement its daemon logs in with: the
/// apps themselves are launched on that daemon.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenApp {
    /// The workspace whose VM runs it: the caller's.
    pub workspace: String,
    /// Who the machine's time is booked to, as the server knows the account.
    #[serde(default)]
    pub account: String,
    /// The host's placement, as [`super::browser::OpenBrowser::material`]
    /// carries it.
    pub material: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn a_linux_app_is_named_and_launched_by_its_desktop_file;

        phase an_app_is_named_by_its_desktop_file_and_nothing_else;
        {
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
    }
}
