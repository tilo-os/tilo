//! The menus of a menu bar the host commands fill, for hosts that have one (macOS, Windows). Commands
//! keep their model order; adjacent groups in one menu are separated.

use super::model::{CommandGroup, HostCommand};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuKind {
    Edit,
    View,
    History,
}

impl MenuKind {
    pub const ALL: [Self; 3] = [Self::Edit, Self::View, Self::History];

    pub fn title_key(self) -> &'static str {
        match self {
            Self::Edit => "host-menu-edit",
            Self::View => "host-menu-view",
            Self::History => "host-menu-history",
        }
    }

    fn holds(self, group: CommandGroup) -> bool {
        let kind = match group {
            CommandGroup::Undo | CommandGroup::Search | CommandGroup::AppActions => Self::Edit,
            CommandGroup::AppViews => Self::View,
            CommandGroup::History | CommandGroup::RecentChanges => Self::History,
        };
        kind == self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry {
    /// An index into the commands the entries were built from.
    Command(usize),
    Separator,
}

pub fn entries(commands: &[HostCommand], menu: MenuKind) -> Vec<Entry> {
    let mut entries = Vec::new();
    let mut previous = None;
    for (index, command) in commands.iter().enumerate() {
        if !menu.holds(command.group) {
            continue;
        }
        if previous.is_some_and(|group| group != command.group) {
            entries.push(Entry::Separator);
        }
        previous = Some(command.group);
        entries.push(Entry::Command(index));
    }
    entries
}
