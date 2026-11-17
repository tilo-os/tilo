//! The rows every text's context menu begins with.
//!
//! Wherever a person edits text, a secondary click, the Menu key or Shift+F10 offers the same
//! five commands under the same names and keys: cut, copy, paste, paste without formatting and
//! select all. The menu names them; the host of the text runs each through the command its
//! shortcut runs, so a row, a key and an agent's action are one edit.

use super::MenuItem;
use crate::i18n::I18nManager;
use crate::platform::Shortcut;

/// One of the standard editing commands of a text's context menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EditingCommand {
    Cut,
    Copy,
    Paste,
    /// Paste the clipboard's words without their formatting.
    PastePlain,
    SelectAll,
}

impl EditingCommand {
    /// Every command, in the order a menu offers them.
    pub const ALL: [Self; 5] = [
        Self::Cut,
        Self::Copy,
        Self::Paste,
        Self::PastePlain,
        Self::SelectAll,
    ];

    /// The command's name where commands go by a word.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Cut => "cut",
            Self::Copy => "copy",
            Self::Paste => "paste",
            Self::PastePlain => "paste-plain",
            Self::SelectAll => "select-all",
        }
    }

    /// The row that offers the command, with the keys that do the same. `selected` says whether
    /// any text is selected: cut and copy wait for a selection. Paste stays offered: reading the
    /// clipboard to find out would wait on the compositor while the menu opens, and use up a
    /// browser's one read of a paste. A paste of nothing changes nothing.
    #[must_use]
    pub fn row(self, selected: bool) -> MenuItem {
        let (label, shortcut) = match self {
            Self::Cut => ("text-menu-cut", Shortcut::command("X")),
            Self::Copy => ("text-menu-copy", Shortcut::command("C")),
            Self::Paste => ("text-menu-paste", Shortcut::command("V")),
            Self::PastePlain => ("text-menu-paste-plain", Shortcut::command("V").shift()),
            Self::SelectAll => ("text-menu-select-all", Shortcut::command("A")),
        };
        MenuItem::action(I18nManager::lookup_plain(label), || {})
            .with_shortcut(shortcut)
            .with_enabled(selected || !matches!(self, Self::Cut | Self::Copy))
    }

    /// The rows a text's menu begins with, each with the command it offers.
    #[must_use]
    pub fn rows(selected: bool) -> Vec<(MenuItem, Self)> {
        Self::ALL
            .into_iter()
            .map(|command| (command.row(selected), command))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A text's menu offers the five commands in one order, each named and keyed; only cut and
    /// copy wait for a selection.
    #[test]
    fn a_texts_menu_offers_five_commands_and_only_cut_and_copy_wait_for_a_selection() {
        for selected in [false, true] {
            let rows = EditingCommand::rows(selected);
            let commands: Vec<_> = rows.iter().map(|(_, command)| *command).collect();
            assert_eq!(commands, EditingCommand::ALL);
            for (item, command) in &rows {
                assert!(
                    item.label().is_some_and(|label| !label.is_empty()),
                    "{command:?} is named"
                );
                let waits = matches!(command, EditingCommand::Cut | EditingCommand::Copy);
                assert_eq!(
                    item.is_enabled(),
                    selected || !waits,
                    "{command:?} with a selection: {selected}"
                );
            }
        }
        let ids: Vec<_> = EditingCommand::ALL
            .into_iter()
            .map(EditingCommand::id)
            .collect();
        assert_eq!(ids, ["cut", "copy", "paste", "paste-plain", "select-all"]);
    }
}
