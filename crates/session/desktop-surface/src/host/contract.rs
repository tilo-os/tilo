//! What the desktop asks of the host, and what the host answers: the host actions the chrome's
//! menus choose, what the person may change of the workspace shown, and what only the host can
//! carry out.

use tilo_desktop_model::commands::DesktopCommand;
use tilo_desktop_model::{DesktopTree, StripId, TabId, WorkspaceId, WorkspaceSlot};
use ui::shortcut_table::{FixedAction, ShortcutAction};

use crate::host::chrome::ChromeIntent;

/// The person pressed one of the music widget's controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MusicAction {
    PlayPause,
    Previous,
    Next,
}

/// What the pointer and presses on the switcher's panel ask: the walk's position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwitcherAsk {
    /// The pointer stands over this card: the walk goes there.
    Hover(usize),
    /// A press on this card: the walk goes there, and goes to it.
    Choose(usize),
    /// A press off the panel, which is no press on anything behind it: the walk ends where it is.
    Dismiss,
}

/// The host action that lists the workspaces this computer may join.
pub const JOIN_WORKSPACE: &str = "join-workspace";
/// The host action that leaves the workspace the person is in.
pub const LEAVE_WORKSPACE: &str = "leave-workspace";
/// The host action that deletes the workspace the person is in.
pub const DELETE_WORKSPACE: &str = "delete-workspace";
/// The host action that makes a new workspace.
pub const NEW_WORKSPACE: &str = "new-workspace";
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
    /// The tree as the chrome shows it: the surface's, with who is here as the active workspace's
    /// collaborators.
    fn tree(&self) -> DesktopTree;

    /// Changes whenever who is here does, beyond the tree.
    fn people_revision(&self) -> u64 {
        0
    }

    /// The person this desktop acts for.
    fn author(&self) -> String;

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
    fn may_delete_workspace(&self, _workspace: WorkspaceSlot) -> Result<(), String> {
        Err(crate::i18n::t("desktop-cannot-delete-workspace"))
    }

    /// The person made `strip` : it is theirs from now on.
    fn made_strip(&self, _strip: StripId) {}

    /// Where the workspace shown is hosted (`example.org`), named beside the workspace; `None` on
    /// the person's own desktop.
    fn workspace_host(&self) -> Option<String> {
        None
    }
}

/// What the desktop asks of its host.
#[derive(Clone, Debug, PartialEq)]
pub enum DesktopAsk {
    /// A request of the chrome the layers do not answer themselves: the people and profile
    /// popovers, inviting, quick settings, the clock, and the menu a secondary press on the desk
    /// asks for at that place.
    Intent(ChromeIntent),
    /// The person chose a host action (`id`) in the workspace menu.
    HostAction(String),
    /// The person left the workspace `name` here; the host says so.
    LeftWorkspace(String),
    /// The person asked to delete the workspace `slot` , called `name` , and it is theirs to
    /// delete: the host asks them to confirm it, and only their answer deletes anything.
    DeleteWorkspace { slot: WorkspaceSlot, name: String },
    /// The desk deleted the workspace `workspace` , which was called `name` : what is left of it in
    /// the host's record goes with it.
    DeletedWorkspace {
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
    /// The person pressed one of the music widget's controls: the host steers the player it shows.
    Music(MusicAction),
    /// The person chose Wait on a window that says it stopped responding: its app gets another
    /// period to answer.
    WaitWindow(TabId),
    /// The person chose Force quit on such a window: the host ends its app.
    ForceQuitApp(TabId),
    /// The pointer on the switcher's panel: which card it stands over, and whether it pressed it.
    Switcher(SwitcherAsk),
    /// The person took back the agent door's grant `id` in the overview of their Tilo
    /// (`tilo_actions::remote::agent_door::REVOKE`).
    RevokeAgentGrant(String),
    /// The person asked for their account in Settings, where billing is too.
    AccountSettings,
}
