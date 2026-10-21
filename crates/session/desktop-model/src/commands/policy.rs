//! Who may ask for which desktop command: the one check a desktop runs
//! before it offers a control and again before it applies the change, on
//! native and in the browser alike.
//!
//! A workspace belongs to whoever governs it, its owner and moderators:
//! they name it and manage its strips (desks). Everyone who writes in the
//! room opens windows, and closes, moves and arranges the windows they
//! opened and the strips they made; what someone else made is theirs, or a
//! moderator's, to change ([`crate::ownership`]). A public workspace is no
//! different: its guests write in it as members do, so they open and close
//! their own windows, and leave its name and desks to whoever governs it.
//!
//! Commands outside the workspace's topology (shortcuts, grant sets,
//! receipts, namespace claims) are decided by their own governance checks
//! and pass here unchanged.

use tilo_action_policy::Role;

use super::DesktopCommand;
use crate::ownership::{Authors, made_by};
use crate::workspace::Workspace;

/// Who asks, and the role the room holds them to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Standing<'a> {
    /// The person, by the id the desktop's authors name them by.
    pub person: &'a str,
    /// Their role in the room, as its host answers it or its policy reads;
    /// `None` while not known yet, or for someone the room does not list.
    pub role: Option<Role>,
}

impl Standing<'_> {
    /// Whether they govern the workspace: its owner or a moderator.
    #[must_use]
    pub fn governs(&self) -> bool {
        self.role >= Some(Role::Moderator)
    }

    /// Whether they write in the room at all.
    #[must_use]
    pub fn writes(&self) -> bool {
        self.role >= Some(Role::Editor)
    }
}

/// What a workspace's governors alone may change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Governed {
    /// The workspace's name.
    Name,
    /// Its strips: adding them, and renaming, moving or removing the ones
    /// nobody here made themselves.
    Desks,
}

/// Why a command is refused, in words a person reads at once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Only the workspace's owner and moderators change this.
    Governed(Governed),
    /// The window or strip `id` is someone else's: `owner` opened or made
    /// it (`None`: the room itself lists it).
    Others { id: u64, owner: Option<String> },
    /// They may look at this workspace, not change it.
    ReadOnly,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Governed(Governed::Name) => {
                "Only the workspace's owner and moderators can rename it"
            }
            Self::Governed(Governed::Desks) => {
                "Only the workspace's owner and moderators manage its desks"
            }
            Self::Others { .. } => "Only whoever opened it, or a moderator, can change it",
            Self::ReadOnly => "You can look at this workspace, not change it",
        })
    }
}

impl std::error::Error for Refusal {}

fn govern(standing: &Standing<'_>, what: Governed) -> Result<(), Refusal> {
    if standing.governs() {
        Ok(())
    } else {
        Err(Refusal::Governed(what))
    }
}

fn write(standing: &Standing<'_>) -> Result<(), Refusal> {
    if standing.writes() {
        Ok(())
    } else {
        Err(Refusal::ReadOnly)
    }
}

/// Whether `standing` may close, move or reorder the window `tab`: they
/// opened it, or they govern the workspace.
///
/// # Errors
///
/// [`Refusal::Others`] for a window someone else (or nobody) opened.
pub fn may_rearrange_window(
    standing: &Standing<'_>,
    authors: &Authors,
    tab: u64,
) -> Result<(), Refusal> {
    if standing.governs() || made_by(authors, tab, standing.person) {
        Ok(())
    } else {
        Err(Refusal::Others {
            id: tab,
            owner: authors.get(&tab).cloned(),
        })
    }
}

/// Whether `standing` may rename, move or remove the strip `strip`: they
/// made it, or they govern the workspace.
///
/// # Errors
///
/// [`Refusal::Governed`] for a strip they did not make.
pub fn may_manage_strip(
    standing: &Standing<'_>,
    authors: &Authors,
    strip: u64,
) -> Result<(), Refusal> {
    if made_by(authors, strip, standing.person) {
        Ok(())
    } else {
        govern(standing, Governed::Desks)
    }
}

/// Whether `standing` may ask for `command` on `workspace`, whose strips
/// and windows `authors` made. Hosts ask before they offer the control
/// that sends it, and again before they apply it.
///
/// # Errors
///
/// The [`Refusal`] to show the person.
pub fn may_command(
    standing: &Standing<'_>,
    authors: &Authors,
    workspace: &Workspace,
    command: &DesktopCommand,
) -> Result<(), Refusal> {
    match command {
        DesktopCommand::RenameWorkspace { .. } => govern(standing, Governed::Name),
        DesktopCommand::CreateStrip { .. } => govern(standing, Governed::Desks),
        DesktopCommand::RenameStrip { strip, .. }
        | DesktopCommand::RemoveStrip { strip }
        | DesktopCommand::MoveStrip { strip, .. } => may_manage_strip(standing, authors, *strip),
        DesktopCommand::OpenTab { .. }
        | DesktopCommand::OpenApp { .. }
        | DesktopCommand::DuplicateTab { .. } => write(standing),
        DesktopCommand::CloseTab { location }
        | DesktopCommand::ReorderTab { location, .. }
        | DesktopCommand::MoveTab { location, .. }
        | DesktopCommand::SplitTab { location, .. } => {
            may_rearrange_window(standing, authors, location.tab)
        }
        DesktopCommand::ReorderGroup { group, .. } | DesktopCommand::MoveGroup { group, .. } => {
            workspace
                .strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .filter(|candidate| candidate.id == *group)
                .flat_map(|candidate| &candidate.tabs)
                .try_for_each(|tab| may_rearrange_window(standing, authors, tab.id))
        }
        DesktopCommand::SaveShortcut { .. }
        | DesktopCommand::DeleteShortcut { .. }
        | DesktopCommand::ClaimNamespace { .. }
        | DesktopCommand::SavePreset { .. }
        | DesktopCommand::AssignPreset { .. }
        | DesktopCommand::RecordEffectReceipt { .. }
        | DesktopCommand::GrantAppAction { .. }
        | DesktopCommand::RevokeAppAction { .. } => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{TabLocation, TabPlacement};
    use crate::ids::{StripId, WorkspaceId};
    use crate::layout::{Strip, Tab, TileContent, WindowGroup};
    use crate::width::WindowWidth;

    /// A guest who came in through a public workspace's door: the room
    /// lets guests write, as members do.
    const GUEST: Standing<'static> = Standing {
        person: "guest",
        role: Some(Role::Editor),
    };
    const OTHER_GUEST: &str = "other-guest";
    const MEMBER: Standing<'static> = Standing {
        person: "member",
        role: Some(Role::Editor),
    };
    const MODERATOR: Standing<'static> = Standing {
        person: "moderator",
        role: Some(Role::Moderator),
    };
    const OWNER: Standing<'static> = Standing {
        person: "owner",
        role: Some(Role::Owner),
    };

    fn tab(id: u64) -> Tab {
        Tab {
            id,
            title: format!("tab {id}"),
            tile: TileContent::LocalApp {
                app_id: "so.tilo.notes".into(),
                label: "Notes".into(),
                note: String::new(),
            },
        }
    }

    /// The Lounge: one strip (10) the room lists, the guest's window 1 and
    /// another guest's window 2 on it.
    fn lounge() -> (Workspace, Authors) {
        let mut workspace = Workspace::new(WorkspaceId(1), "Lounge");
        let mut strip = Strip::new(StripId(10), "Lounge");
        strip.window_groups = [(100, 1), (200, 2)]
            .into_iter()
            .map(|(group, id)| WindowGroup {
                id: group,
                tabs: vec![tab(id)],
                active_tab: id,
                width: WindowWidth::Normal,
                maximized: false,
                focus_history: Vec::new(),
            })
            .collect();
        workspace.strips = vec![strip];
        let authors = [(1, GUEST.person), (2, OTHER_GUEST)]
            .into_iter()
            .map(|(id, who)| (id, who.to_owned()))
            .collect();
        (workspace, authors)
    }

    fn at(tab: u64, group: u64) -> TabLocation {
        TabLocation {
            strip: 10,
            group,
            tab,
        }
    }

    fn asks(standing: &Standing<'_>, command: &DesktopCommand) -> Result<(), Refusal> {
        let (workspace, authors) = lounge();
        may_command(standing, &authors, &workspace, command)
    }

    #[test]
    fn only_owner_and_moderators_rename_a_public_workspace() {
        let rename = DesktopCommand::RenameWorkspace {
            name: "Hacked".into(),
        };
        assert_eq!(
            asks(&GUEST, &rename),
            Err(Refusal::Governed(Governed::Name))
        );
        assert_eq!(
            asks(&MEMBER, &rename),
            Err(Refusal::Governed(Governed::Name))
        );
        assert_eq!(asks(&MODERATOR, &rename), Ok(()));
        assert_eq!(asks(&OWNER, &rename), Ok(()));
        // Not known yet is not leave to rename.
        let unknown = Standing {
            person: "guest",
            role: None,
        };
        assert!(asks(&unknown, &rename).is_err());
    }

    #[test]
    fn only_owner_and_moderators_add_or_change_the_rooms_desks() {
        let add = DesktopCommand::CreateStrip {
            name: "Mine".into(),
        };
        let rename = DesktopCommand::RenameStrip {
            strip: 10,
            name: "Mine now".into(),
        };
        let remove = DesktopCommand::RemoveStrip { strip: 10 };
        for command in [&add, &rename, &remove] {
            assert_eq!(
                asks(&GUEST, command),
                Err(Refusal::Governed(Governed::Desks))
            );
            assert_eq!(
                asks(&MEMBER, command),
                Err(Refusal::Governed(Governed::Desks))
            );
            assert_eq!(asks(&MODERATOR, command), Ok(()));
            assert_eq!(asks(&OWNER, command), Ok(()));
        }
    }

    #[test]
    fn a_strip_someone_made_stays_theirs_to_manage() {
        let (workspace, mut authors) = lounge();
        authors.insert(10, MEMBER.person.into());
        let rename = DesktopCommand::RenameStrip {
            strip: 10,
            name: "Corner".into(),
        };
        assert_eq!(may_command(&MEMBER, &authors, &workspace, &rename), Ok(()));
        assert!(may_command(&GUEST, &authors, &workspace, &rename).is_err());
    }

    #[test]
    fn guests_close_and_move_their_own_windows_and_moderators_anyones() {
        let mine = DesktopCommand::CloseTab {
            location: at(1, 100),
        };
        let theirs = DesktopCommand::CloseTab {
            location: at(2, 200),
        };
        assert_eq!(asks(&GUEST, &mine), Ok(()));
        assert_eq!(
            asks(&GUEST, &theirs),
            Err(Refusal::Others {
                id: 2,
                owner: Some(OTHER_GUEST.into()),
            })
        );
        assert!(asks(&MEMBER, &theirs).is_err());
        assert_eq!(asks(&MODERATOR, &theirs), Ok(()));
        let move_theirs = DesktopCommand::MoveTab {
            location: at(2, 200),
            placement: TabPlacement::NewGroup {
                strip: 10,
                after: None,
            },
        };
        assert!(asks(&GUEST, &move_theirs).is_err());
        let reorder_theirs = DesktopCommand::ReorderGroup {
            strip: 10,
            group: 200,
            before: Some(100),
        };
        assert!(asks(&GUEST, &reorder_theirs).is_err());
        let reorder_mine = DesktopCommand::ReorderGroup {
            strip: 10,
            group: 100,
            before: None,
        };
        assert_eq!(asks(&GUEST, &reorder_mine), Ok(()));
    }

    #[test]
    fn writers_open_windows_and_watchers_do_not() {
        let open = DesktopCommand::OpenApp {
            app: "so.tilo.notes".into(),
            beside: None,
        };
        assert_eq!(asks(&GUEST, &open), Ok(()));
        assert_eq!(asks(&MEMBER, &open), Ok(()));
        let viewer = Standing {
            person: "viewer",
            role: Some(Role::Viewer),
        };
        assert_eq!(asks(&viewer, &open), Err(Refusal::ReadOnly));
    }

    #[test]
    fn every_refusal_says_why_in_words() {
        for refusal in [
            Refusal::Governed(Governed::Name),
            Refusal::Governed(Governed::Desks),
            Refusal::Others { id: 2, owner: None },
            Refusal::ReadOnly,
        ] {
            let text = refusal.to_string();
            assert!(!text.contains('2') && text.ends_with(char::is_alphabetic));
        }
    }
}
