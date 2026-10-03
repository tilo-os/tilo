//! The menus of a menu bar the host commands fill, for hosts that have one
//! (macOS, Windows). Commands keep their model order; adjacent groups in one
//! menu are separated.

use super::model::{CommandGroup, HostCommand};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuKind {
    Edit,
    View,
    History,
}

impl MenuKind {
    pub(crate) const ALL: [Self; 3] = [Self::Edit, Self::View, Self::History];

    pub(crate) fn title_key(self) -> &'static str {
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
pub(crate) enum Entry {
    /// An index into the commands the entries were built from.
    Command(usize),
    Separator,
}

pub(crate) fn entries(commands: &[HostCommand], menu: MenuKind) -> Vec<Entry> {
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

#[cfg(test)]
mod tests {
    use automerge::ROOT;
    use automerge::transaction::Transactable;

    use super::{Entry, MenuKind, entries};
    use crate::host_chrome::model::CommandGroup;

    #[test]
    fn each_menu_holds_its_groups_in_model_order_with_separators_between() {
        let mut state = crate::State::new();
        for title in ["first", "second"] {
            state.doc.put(ROOT, "title", title).unwrap();
            state.commit_action(None, None);
        }
        let mut app = crate::app::App::new_with_controlled_inputs(
            state,
            Box::new(crate::widget::Text::new("Document")),
        );
        app.enable_document_history_chrome();
        app.init_layout();
        let commands = app.host_commands(true);
        let group = |entry: &Entry| match entry {
            Entry::Command(index) => Some(commands[*index].group),
            Entry::Separator => None,
        };
        let edit: Vec<_> = entries(&commands, MenuKind::Edit)
            .iter()
            .map(group)
            .collect();
        assert_eq!(
            edit,
            [
                Some(CommandGroup::Undo),
                Some(CommandGroup::Undo),
                None,
                Some(CommandGroup::Search),
            ]
        );
        let history: Vec<_> = entries(&commands, MenuKind::History)
            .iter()
            .map(group)
            .collect();
        assert_eq!(
            history,
            [
                Some(CommandGroup::History),
                None,
                Some(CommandGroup::RecentChanges),
                Some(CommandGroup::RecentChanges),
            ]
        );
        assert!(entries(&commands, MenuKind::View).is_empty());
        let titles: Vec<_> = MenuKind::ALL
            .iter()
            .map(|kind| crate::i18n::I18nManager::lookup_plain(kind.title_key()))
            .collect();
        assert_eq!(titles, ["Edit", "View", "History"]);
    }
}
