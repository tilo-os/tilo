//! The editor's commands as a qOS global-search source.
//!
//! The editor already carries a full command set (see
//! [`Command::catalogue`](crate::command_palette::Command::catalogue)) reachable
//! from its own Ctrl+P palette. This exposes that same set to the **shell-wide
//! Ctrl+K palette**, so "Open Folder", "Save", "Format Document", "Toggle
//! Sidebar" and the rest are discoverable and runnable without first learning
//! the app's private shortcut.
//!
//! Why a [`SearchProvider`] and not the qOS `Resource`/`ActionSpec` path: an
//! `ActionSpec` yields a replayable `EncodedAction`, whose `apply` contract
//! forbids local-only effects. The editor's commands are overwhelmingly
//! presentation — save to disk, focus a panel, toggle the sidebar, open a
//! picker — none of which is a pure CRDT mutation. They are navigation-style
//! commands, so they ride the same `search_providers()` loop the file provider
//! does: a row per command, activated back through the app's single
//! [`apply_command`](crate::app::CodeApp) dispatch point. The one genuine
//! document mutation the editor has (New file → `CreateBuffer`) still travels
//! the `resources()` / action path in [`crate::actions`].

use ui::icons::Icon;
use ui::search::{SearchProvider, SearchResult};

use crate::command_palette::Command;

/// The group header Ctrl+K files these rows under.
pub const COMMAND_KIND: &str = "Commands";

/// The `action_id` namespace for a command row, so a chosen command round-trips
/// back to its [`Command`] without colliding with the file provider's
/// `code.node:` ids.
const ACTION_PREFIX: &str = "code.cmd:";

/// A [`SearchProvider`] over the editor's whole command catalogue.
///
/// Stateless: the catalogue is fixed, so the provider carries nothing. Whether a
/// command *does* anything in the current context (Save with no open buffer) is
/// the app's business when the row is activated, exactly as it is for the same
/// command dispatched from a keybinding — the palette lists the command either
/// way, which is what makes it discoverable.
pub struct CommandSearchProvider;

impl CommandSearchProvider {
    /// The `action_id` a command row hands back.
    pub fn action_id(cmd: Command) -> String {
        format!("{ACTION_PREFIX}{}", cmd.id())
    }

    /// Resolve an `action_id` produced by this provider back into a [`Command`].
    /// `None` for anything that is not one of our command rows.
    pub fn parse_action_id(action_id: &str) -> Option<Command> {
        action_id.strip_prefix(ACTION_PREFIX).and_then(Command::from_id)
    }

    /// The leading glyph for a command, grouped by what it touches so the list
    /// reads at a glance rather than as one undifferentiated column.
    fn icon_for(cmd: Command) -> Icon {
        match cmd {
            Command::OpenFolder => Icon::FolderOpen,
            Command::NewFile => Icon::FilePlus,
            Command::OpenFile => Icon::FileCode,
            Command::Save | Command::SaveAs | Command::SaveAll | Command::RevertFile => {
                Icon::FileText
            }
            Command::Format | Command::ToggleFormatOnSave => Icon::Sparkles,
            Command::Find | Command::Replace | Command::SearchProject | Command::GoToLine => {
                Icon::Search
            }
            Command::ToggleSidebar | Command::ToggleBottomPanel => Icon::Layers,
            Command::CloseTab => Icon::X,
            Command::NextTheme => Icon::Sun,
            _ => Icon::Code,
        }
    }
}

impl SearchProvider for CommandSearchProvider {
    fn name(&self) -> &str {
        COMMAND_KIND
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        let q = query.to_lowercase();
        Command::catalogue()
            .into_iter()
            .filter(|(_, label, keys)| {
                q.is_empty()
                    || label.to_lowercase().contains(&q)
                    || keys.to_lowercase().contains(&q)
            })
            .map(|(cmd, label, keys)| {
                let mut r =
                    SearchResult::new(COMMAND_KIND, label.to_string(), Self::action_id(cmd))
                        .with_icon(Self::icon_for(cmd));
                if !keys.is_empty() {
                    // The advertised shortcut, so a command reachable only by a
                    // chord nobody told you about is still teachable from Ctrl+K.
                    r = r.with_subtitle(keys.to_string());
                }
                r
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_lists_every_command() {
        let provider = CommandSearchProvider;
        let all = provider.search("");
        assert_eq!(
            all.len(),
            Command::catalogue().len(),
            "the browse set is the whole catalogue"
        );
        assert!(all.iter().all(|r| r.kind == COMMAND_KIND));
    }

    #[test]
    fn every_row_round_trips_to_its_command() {
        let provider = CommandSearchProvider;
        for row in provider.search("") {
            let cmd = CommandSearchProvider::parse_action_id(&row.action_id)
                .expect("a command row's id resolves back to a command");
            assert_eq!(CommandSearchProvider::action_id(cmd), row.action_id);
        }
    }

    #[test]
    fn filters_by_label_and_keybinding() {
        let provider = CommandSearchProvider;
        // By label.
        let by_label = provider.search("format");
        assert!(by_label.iter().any(|r| r.title.contains("Format Document")));
        // By the advertised shortcut text.
        let by_key = provider.search("Ctrl+Shift+O");
        assert!(
            by_key.iter().any(|r| r.title.contains("Open Folder")),
            "a query for the chord finds the command it triggers"
        );
    }

    #[test]
    fn open_folder_is_discoverable() {
        // The headline gap this closes: Open Folder was reachable only by a
        // shortcut or the app's private palette, never the shell-wide Ctrl+K.
        let provider = CommandSearchProvider;
        let hit = provider
            .search("open folder")
            .into_iter()
            .find(|r| r.title.contains("Open Folder"))
            .expect("Open Folder is in the Ctrl+K command list");
        assert_eq!(
            CommandSearchProvider::parse_action_id(&hit.action_id),
            Some(Command::OpenFolder)
        );
    }

    #[test]
    fn foreign_ids_are_rejected() {
        assert!(CommandSearchProvider::parse_action_id("code.node:3").is_none());
        assert!(CommandSearchProvider::parse_action_id("code.cmd:not-a-command").is_none());
        assert!(CommandSearchProvider::parse_action_id("settings.pane:general").is_none());
    }
}
