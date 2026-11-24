//! What the desktop asks of the host, and what the host answers: the host actions the chrome's
//! menus choose, what the person may change of the workspace shown, and what only the host can
//! carry out.

use tilo_desktop_model::commands::DesktopCommand;
use tilo_desktop_model::{StripId, TabId, WorkspaceId};
use ui::shortcut_table::{FixedAction, ShortcutAction};

use crate::desktop::host::chrome::ChromeIntent;

/// The host action that lists the workspaces this computer may join.
pub const JOIN_WORKSPACE: &str = "join-workspace";
/// The host action that leaves the workspace the person is in.
pub const LEAVE_WORKSPACE: &str = "leave-workspace";
/// The host action that deletes the workspace the person is in.
pub const DELETE_WORKSPACE: &str = "delete-workspace";
/// The host action that makes a new workspace.
pub const NEW_WORKSPACE: &str = "new-workspace";
/// The host action that branches the workspace the person is in.
pub const BRANCH_WORKSPACE: &str = "branch-workspace";
/// The host action that opens the workspace's settings.
pub const WORKSPACE_SETTINGS: &str = "workspace-settings";
/// The host action that locks the screen.
pub const SESSION_LOCK: &str = "session-lock";
/// The host action that logs this person out.
pub const SESSION_LOG_OUT: &str = "session-log-out";
/// The host action that restarts the computer.
pub const SESSION_RESTART: &str = "session-restart";
/// The host action that shuts the computer down.
pub const SESSION_SHUT_DOWN: &str = "session-shut-down";
/// The host action that opens the box a report about Tilo itself is written in, to the one place
/// this computer sends reports.
pub const REPORT_PROBLEM: &str = "session-report-problem";
/// What the host answers about the person at this desktop and the workspace it shows.
pub trait DesktopRules {
    /// Whether the person may run `command` on the active workspace.
    ///
    /// # Errors
    /// The refusal, in words for the person.
    fn may(&self, command: &DesktopCommand) -> Result<(), String>;

    /// Whether the person may add a workspace.
    ///
    /// # Errors
    /// The refusal, in words for the person.
    fn may_create_workspace(&self) -> Result<(), String>;

    /// Whether the workspace `workspace` may be deleted: there is nothing of
    /// it left anywhere once it is.
    ///
    /// # Errors
    /// The refusal, in words for the person.
    fn may_delete_workspace(&self, _workspace: WorkspaceId) -> Result<(), String> {
        Err(crate::desktop::i18n::t("desktop-cannot-delete-workspace"))
    }

    /// Whether the person may branch the workspace `workspace` : they read everything it holds,
    /// and may make a workspace of their own.
    ///
    /// # Errors
    /// The refusal, in words for the person.
    fn may_branch_workspace(&self, _workspace: WorkspaceId) -> Result<(), String> {
        self.may_create_workspace()
    }

    /// Whether the workspace `workspace` , which exists already, may be put on this shell: it is
    /// one this device holds. One it holds nothing of is entered (its host takes the device in)
    /// or joined (by an invitation) before it comes on the shell; put here bare, it would be a
    /// fresh workspace of its own under that id.
    ///
    /// # Errors
    /// The refusal, in words for the person.
    fn may_open_workspace(&self, _workspace: WorkspaceId) -> Result<(), String> {
        Ok(())
    }

    /// The person made `strip` : it is theirs from now on.
    fn made_strip(&self, _strip: StripId) {}
}

/// What the desktop asks of its host.
#[derive(Clone, Debug, PartialEq)]
pub enum DesktopAsk {
    /// A request the layers do not answer themselves: who is here, a person's row, inviting, and
    /// pairing another device.
    Intent(ChromeIntent),
    /// The person chose a host action (`id`) in the workspace menu.
    HostAction(String),
    /// The person left the workspace `name` here; the host says so.
    LeftWorkspace(String),
    /// The person asked to delete the workspace `workspace` , called `name` , and it is theirs to
    /// delete: the host asks them to confirm it, and only their answer deletes anything.
    DeleteWorkspace {
        workspace: WorkspaceId,
        name: String,
    },
    /// The shell deleted the workspace `workspace` , which was called `name` : what is left of it in
    /// the host's record goes with it.
    DeletedWorkspace {
        workspace: WorkspaceId,
        name: String,
    },
    /// The person branched the workspace `source` into `workspace` , called `name` : the host
    /// makes it, with what `source` holds, and puts it on the shell.
    BranchWorkspace {
        source: WorkspaceId,
        workspace: WorkspaceId,
        name: String,
    },
    /// The person renamed the workspace `workspace` to `name` on the shell: the name is the
    /// workspace's, not this shell's, so the host writes it where the workspace keeps it and every
    /// device and list of it reads it there.
    RenamedWorkspace {
        workspace: WorkspaceId,
        name: String,
    },
    /// The person asked to report a problem.
    ReportProblem,
    /// A desktop shortcut the layers have no part in (search, quick settings, locking, the
    /// magnifier, …): the host's to carry out.
    Shortcut(ShortcutAction),
    /// A fixed shortcut the layers have no part in (a capture, the shortcut guide, cutting a remote
    /// control): the host's to carry out.
    FixedShortcut(FixedAction),
    /// The person chose Wait on a window that says it stopped responding: its app gets another
    /// period to answer.
    WaitWindow(TabId),
    /// The person chose Force quit on such a window: the host ends its app.
    ForceQuitApp(TabId),
    /// The person asked again for the app whose window ended in `tab`, as the card's Reopen does:
    /// the host launches it into that same tab.
    ReopenEnded(TabId),
    /// The person asked for Settings at the page `link` names, which only
    /// the host can open: its search runs where the workspaces are.
    OpenSettings(String),
    /// The person asked to open what `link`, a `tilo://open#…` address, names: the app that keeps
    /// it shows it, started first when no window of it does.
    OpenLink(String),
    /// The person asked to see `path` in the file manager, which only the
    /// host can open: its search runs where the workspaces are.
    ShowInFiles(String),
}
