//! The native desktop's [`DesktopCommandSink`]: this device owns its
//! topology, so an admitted command commits at once through the workspace
//! tree (cache, room document and restart snapshot together), and a view
//! change is persisted the moment it is accepted.
use crate::shell::chrome_state::ChromeState;
use crate::shell::state::{RoomId, WorkspaceTree};
use tilo_desktop::intents::{LocalViewIntent, apply_local_view};
use tilo_desktop::session_commands::DesktopCommandSink;
use tilo_desktop_model::commands::{BoundCommand, CommandSource, DesktopCommand, admit};
use tilo_desktop_model::{DesktopTree, peer_salt_of};

pub(crate) struct ShellCommands<'a> {
    pub(crate) chrome: &'a mut ChromeState,
}

/// The source room `source` names, parsed exactly as canonical text.
fn source_room(source: &CommandSource) -> Result<RoomId, String> {
    RoomId::parse(&source.room)
        .filter(|room| room.to_string() == source.room)
        .ok_or_else(|| "desktop source does not name a canonical room".to_owned())
}

impl DesktopCommandSink for ShellCommands<'_> {
    fn with_tree<R>(&self, read: impl FnOnce(&DesktopTree) -> R) -> R {
        read(&self.chrome.tree().desktop)
    }

    fn source(&mut self, room: RoomId) -> Result<CommandSource, String> {
        let mut tree = self.chrome.tree_mut();
        // Project the cache first: the source must be the room revision this
        // desktop is about to command, not an older projection of it.
        tree.sync_room_doc(room);
        let heads = tree
            .room_docs
            .get_mut(&room)
            .ok_or_else(|| "room has no admitted desktop revision".to_owned())?
            .get_heads();
        Ok(CommandSource {
            room: room.to_string(),
            heads: heads.into_iter().map(|head| head.0).collect(),
        })
    }

    fn apply_local(&mut self, intent: LocalViewIntent) {
        // The user-maximize slot is what suppresses the top bar;
        // it is cleared only by the group that holds it.
        if let LocalViewIntent::Maximized { group, maximized } = intent {
            if maximized {
                self.chrome.set_maximized_group(Some(group));
            } else if self.chrome.maximized_group() == Some(group) {
                self.chrome.set_maximized_group(None);
            }
        }
        let mut tree = self.chrome.tree_mut();
        if let LocalViewIntent::Maximized { group, .. } = intent {
            crate::shell::stepped_back::settle_stepped_back(&mut tree, group);
        }
        apply_local_view(&mut tree.desktop, intent);
        // View state survives a restart: persist it in the boundary that
        // accepted the gesture.
        tree.queue_save();
    }

    fn submit(&mut self, bound: BoundCommand) -> Result<(), String> {
        let room = source_room(&bound.source)?;
        let command = {
            let mut tree = self.chrome.tree_mut();
            // The cache is this owner's current desktop. Project it before
            // deciding freshness, so a source taken before a local change
            // cannot be applied to the changed tree.
            tree.sync_room_doc(room);
            let local_salt = tree.local_salt();
            let WorkspaceTree {
                desktop, room_docs, ..
            } = &mut *tree;
            let workspace = desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.room() == room)
                .ok_or_else(|| "desktop source room has no workspace".to_owned())?;
            let document = room_docs
                .get_mut(&room)
                .ok_or_else(|| "room has no admitted desktop revision".to_owned())?;
            // Native strips and windows are kept by id namespace, not by who
            // made them: this host owns the local salt and the legacy zero
            // salt, and nothing another device minted.
            admit(workspace, &bound.source, document, &bound.command, |id| {
                let salt = peer_salt_of(id);
                salt == 0 || salt == local_salt
            })?
        };
        match command {
            // One lifecycle for every close door: document, tree, prune,
            // flush, surface close, in that order.
            DesktopCommand::CloseTab { location } => {
                crate::shell::window_actions::close_tab(self.chrome, location.tab)
                    .map_err(|error| format!("{error:?}"))
            }
            command => self.chrome.tree_mut().commit_command(&command).map(drop),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::chrome_state::Chrome;
    use crate::shell::state::WorkspaceTree;
    use tilo_desktop::intents::SessionIntent;
    use tilo_desktop_model::commands::{DesktopCommand, TabLocation};
    use tilo_desktop_model::{
        AppKind, StripId, Tab, TileContent, WindowGroup, WindowWidth, compose_id,
    };

    const LOCAL_SALT: u16 = 0x5a17;

    /// A demo desktop whose windows are minted under the tree's own salt, so
    /// the room projection carries the topology the same way production does.
    fn chrome_with_owned_salt() -> Chrome {
        let mut tree = WorkspaceTree::bootstrap_demo();
        tree.set_salt_override(LOCAL_SALT);
        {
            let workspace = tree
                .desktop
                .active_workspace_mut()
                .expect("active workspace");
            let strip = workspace.strips.first_mut().expect("a strip");
            strip.id = StripId(compose_id(LOCAL_SALT, 1));
            for (index, group) in strip.window_groups.iter_mut().enumerate() {
                let counter = 10 + index as u64 * 10;
                group.id = compose_id(LOCAL_SALT, counter);
                for (tab_index, tab) in group.tabs.iter_mut().enumerate() {
                    tab.id = compose_id(LOCAL_SALT, counter + 1 + tab_index as u64);
                }
                group.active_tab = group.tabs.first().expect("a tab").id;
            }
        }
        tree.seed_doc_from_cache();
        ChromeState::new(tree)
    }

    type Topology = Vec<(u64, Vec<(u64, Vec<(u64, Vec<u64>)>)>)>;

    fn topology(chrome: &Chrome) -> Topology {
        let st = chrome.borrow();
        let tree = st.tree();
        tree.desktop
            .workspaces
            .iter()
            .map(|workspace| {
                let strips = workspace
                    .strips
                    .iter()
                    .map(|strip| {
                        let groups = strip
                            .window_groups
                            .iter()
                            .map(|group| {
                                (
                                    group.id,
                                    group.tabs.iter().map(|tab| tab.id).collect::<Vec<_>>(),
                                )
                            })
                            .collect::<Vec<_>>();
                        (strip.id.0, groups)
                    })
                    .collect::<Vec<_>>();
                (workspace.id.raw(), strips)
            })
            .collect()
    }

    fn home_tab(chrome: &Chrome) -> (RoomId, u64, u64, u64) {
        let st = chrome.borrow();
        let tree = st.tree();
        let workspace = tree.desktop.active_workspace().expect("active workspace");
        let strip = workspace.strips.first().expect("a strip");
        let group = strip.window_groups.first().expect("a group");
        let tab = group.tabs.first().expect("a tab");
        (workspace.room(), strip.id.0, group.id, tab.id)
    }

    fn source_for(chrome: &Chrome, room: RoomId) -> CommandSource {
        let mut st = chrome.borrow_mut();
        let mut sink = ShellCommands { chrome: &mut st };
        sink.source(room).expect("the room has an admitted source")
    }

    fn submit(chrome: &Chrome, bound: BoundCommand) -> Result<(), String> {
        let mut st = chrome.borrow_mut();
        let mut sink = ShellCommands { chrome: &mut st };
        sink.submit(bound)
    }

    #[test]
    fn current_owned_close_runs_the_one_close_lifecycle() {
        let chrome = chrome_with_owned_salt();
        let (room, _, group, tab) = home_tab(&chrome);
        let source = source_for(&chrome, room);
        let bound = tilo_desktop::session_commands::bind_intent(
            &chrome.borrow().tree().desktop,
            source,
            SessionIntent::CloseTab { group, tab },
        )
        .expect("the source names the tab");
        submit(&chrome, bound).expect("the owner may close its own tab");

        let reopened = {
            let mut st = chrome.borrow_mut();
            assert!(
                !st.tree()
                    .desktop
                    .workspaces
                    .iter()
                    .flat_map(|workspace| &workspace.strips)
                    .flat_map(|strip| &strip.window_groups)
                    .flat_map(|group| &group.tabs)
                    .any(|candidate| candidate.id == tab),
                "the admitted close removed the tab"
            );
            st.reopen_closed_tab()
        };
        assert_eq!(
            reopened,
            Some(tab),
            "a close through the sink is remembered for reopening"
        );
    }

    #[test]
    fn current_owned_reorder_commits() {
        let chrome = chrome_with_owned_salt();
        let (room, _, group, _) = home_tab(&chrome);
        let tabs = |chrome: &Chrome| {
            let st = chrome.borrow();
            let tree = st.tree();
            tree.desktop
                .workspaces
                .iter()
                .flat_map(|workspace| &workspace.strips)
                .flat_map(|strip| &strip.window_groups)
                .find(|candidate| candidate.id == group)
                .map(|group| group.tabs.iter().map(|tab| tab.id).collect::<Vec<_>>())
                .expect("the group stays")
        };
        let before = tabs(&chrome);
        let source = source_for(&chrome, room);
        let bound = tilo_desktop::session_commands::bind_intent(
            &chrome.borrow().tree().desktop,
            source,
            SessionIntent::ReorderTab {
                group,
                from: 0,
                to: 1,
            },
        )
        .expect("the source names the tab");
        submit(&chrome, bound).expect("the owner may reorder its own tab");
        let after = tabs(&chrome);
        assert_ne!(after, before, "the admitted reorder committed");
        let expected = before.iter().take(2).rev().copied().collect::<Vec<_>>();
        assert_eq!(
            after.get(..2),
            Some(expected.as_slice()),
            "the source tab moved to the requested slot"
        );
    }

    #[test]
    fn a_stale_source_revision_never_mutates_the_desktop() {
        let chrome = chrome_with_owned_salt();
        let (room, strip, group, tab) = home_tab(&chrome);
        let stale = source_for(&chrome, room);
        chrome
            .borrow_mut()
            .tree_mut()
            .commit_strip_command(&DesktopCommand::RenameStrip {
                strip,
                name: "Later".into(),
            })
            .expect("the owner renames its strip");
        let before = topology(&chrome);
        let bound = BoundCommand {
            source: stale,
            command: DesktopCommand::CloseTab {
                location: TabLocation { strip, group, tab },
            },
        };
        let error = submit(&chrome, bound).expect_err("an old topology is refused");
        assert!(error.contains("stale"), "{error}");
        assert_eq!(topology(&chrome), before, "a refusal changes nothing");
    }

    #[test]
    fn a_source_room_never_reaches_another_workspaces_window() {
        let chrome = chrome_with_owned_salt();
        let (_, strip, group, tab) = home_tab(&chrome);
        let elsewhere = {
            let st = chrome.borrow();
            let tree = st.tree();
            tree.desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.name == "Research")
                .expect("the demo has another workspace")
                .room()
        };
        let source = source_for(&chrome, elsewhere);
        let before = topology(&chrome);
        let bound = BoundCommand {
            source,
            command: DesktopCommand::CloseTab {
                location: TabLocation { strip, group, tab },
            },
        };
        let error = submit(&chrome, bound).expect_err("another room's tab is not ours");
        assert!(error.contains("another topology owner"), "{error}");
        assert_eq!(topology(&chrome), before, "a refusal changes nothing");
    }

    #[test]
    fn a_command_never_reaches_another_devices_namespace() {
        let chrome = chrome_with_owned_salt();
        let (room, strip, _, _) = home_tab(&chrome);
        let local_salt = chrome.borrow().tree().local_salt();
        let foreign_salt = if local_salt == 7 { 8 } else { 7 };
        let foreign_group = compose_id(foreign_salt, 1);
        let foreign_tab = compose_id(foreign_salt, 2);
        {
            let st = chrome.borrow_mut();
            let mut tree = st.tree_mut();
            let workspace = tree
                .desktop
                .active_workspace_mut()
                .expect("active workspace");
            let strip = workspace.strips.first_mut().expect("a strip");
            strip.window_groups.push(WindowGroup {
                id: foreign_group,
                active_tab: foreign_tab,
                width: WindowWidth::Normal,
                maximized: false,
                focus_history: Vec::new(),
                tabs: vec![Tab {
                    id: foreign_tab,
                    title: "Theirs".into(),
                    tile: TileContent::AppPlaceholder {
                        kind: AppKind::Other("so.tilo.notes".into()),
                        label: "Theirs".into(),
                    },
                    app_title: None,
                }],
            });
        }
        let source = source_for(&chrome, room);
        let before = topology(&chrome);
        let bound = BoundCommand {
            source,
            command: DesktopCommand::CloseTab {
                location: TabLocation {
                    strip,
                    group: foreign_group,
                    tab: foreign_tab,
                },
            },
        };
        let error = submit(&chrome, bound).expect_err("another device's window is theirs");
        assert!(error.contains("another topology owner"), "{error}");
        assert_eq!(topology(&chrome), before, "a refusal changes nothing");
    }
}
