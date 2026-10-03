//! The native desktop's [`DesktopCommandSink`]: this device owns its
//! topology, so a bound command commits at once through the workspace tree
//! (cache, room document and restart snapshot together), and a view change
//! is persisted the moment it is accepted.
use crate::shell::chrome_state::ChromeState;
use tilo_desktop::intents::{LocalViewIntent, apply_local_view};
use tilo_desktop::session_commands::DesktopCommandSink;
use tilo_desktop_model::commands::{BoundCommand, CommandSource, DesktopCommand};
use tilo_desktop_model::{DesktopTree, RoomId};

pub(crate) struct ShellCommands<'a> {
    pub(crate) chrome: &'a mut ChromeState,
}

impl DesktopCommandSink for ShellCommands<'_> {
    fn with_tree<R>(&self, read: impl FnOnce(&DesktopTree) -> R) -> R {
        read(&self.chrome.tree().desktop)
    }

    fn source(&mut self, room: RoomId) -> Result<CommandSource, String> {
        let heads = self.chrome.tree_mut().doc.get_heads();
        Ok(CommandSource {
            room: room.to_string(),
            heads: heads.into_iter().map(|head| head.0).collect(),
        })
    }

    fn apply_local(&mut self, intent: LocalViewIntent) {
        // The user-maximize slot is what suppresses the top bar and wheel;
        // it is cleared only by the group that holds it.
        if let LocalViewIntent::Maximized { group, maximized } = intent {
            if maximized {
                self.chrome.set_maximized_group(Some(group));
            } else if self.chrome.maximized_group() == Some(group) {
                self.chrome.set_maximized_group(None);
            }
        }
        let mut tree = self.chrome.tree_mut();
        apply_local_view(&mut tree.desktop, intent);
        // View state survives a restart: persist it in the boundary that
        // accepted the gesture.
        tree.flush_to_disk();
    }

    fn submit(&mut self, command: BoundCommand) -> Result<(), String> {
        match command.command {
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
