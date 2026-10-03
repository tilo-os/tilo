//! The actions in the window's header row: the message list's (search,
//! select, refresh, new message; or the selection's actions) and the open
//! message's (reply, archive, star, …).
//!
//! Each is the toolkit [`Toolbar`], rebuilt from [`MailState`] whenever what
//! it offers changes; a command only queues a [`Command`], which the app
//! applies with the document in hand.

use ui::icons::Icon;
use ui::widget::toolbar::Orientation;
use ui::widget::{AppModel, ModelPath, Toolbar};

use crate::model::Folder;
use crate::state::{BulkAction, Command, CtxAction, MailState, Shared};

/// One command of a header toolbar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BarItem {
    label: String,
    icon: Option<Icon>,
    /// `Some` for a command that is on or off.
    toggle: Option<bool>,
    command: Command,
}

impl BarItem {
    fn button(label: impl Into<String>, command: Command) -> Self {
        Self {
            label: label.into(),
            icon: None,
            toggle: None,
            command,
        }
    }

    fn icon(icon: Icon, label: impl Into<String>, command: Command) -> Self {
        Self {
            label: label.into(),
            icon: Some(icon),
            toggle: None,
            command,
        }
    }

    fn toggle(label: impl Into<String>, on: bool, command: Command) -> Self {
        Self {
            label: label.into(),
            icon: None,
            toggle: Some(on),
            command,
        }
    }
}

/// What a header toolbar offers: its commands in groups.
pub(crate) type BarSpec = Vec<Vec<BarItem>>;

/// The message list's commands.
pub(crate) fn list_bar(state: &MailState) -> BarSpec {
    if state.bulk_selecting {
        let ids = state.selected_bulk_ids();
        let all_trash = !ids.is_empty()
            && ids.iter().all(|id| {
                state
                    .mailbox
                    .get(*id)
                    .is_some_and(|message| message.folder == Folder::Trash)
            });
        let delete = if all_trash {
            BarItem::icon(
                Icon::Trash2,
                BulkAction::Purge.label(),
                Command::Bulk(BulkAction::Purge),
            )
        } else {
            BarItem::icon(
                Icon::Trash2,
                BulkAction::Delete.label(),
                Command::Bulk(BulkAction::Delete),
            )
        };
        return vec![
            vec![
                BarItem::icon(
                    Icon::Archive,
                    BulkAction::Archive.label(),
                    Command::Bulk(BulkAction::Archive),
                ),
                delete,
                BarItem::button(
                    BulkAction::MarkRead.label(),
                    Command::Bulk(BulkAction::MarkRead),
                ),
                BarItem::button(
                    BulkAction::MarkUnread.label(),
                    Command::Bulk(BulkAction::MarkUnread),
                ),
                BarItem::button("Select all", Command::SelectAllVisible),
            ],
            vec![BarItem::button("Done", Command::ToggleBulk)],
        ];
    }
    vec![vec![
        BarItem::icon(
            Icon::Search,
            format!("Search {}", state.folder.title()),
            Command::OpenSearch,
        ),
        BarItem::icon(Icon::Square, "Select messages", Command::ToggleBulk),
        BarItem::icon(Icon::Refresh, "Refresh", Command::Refresh),
        BarItem::icon(Icon::Pencil, "New message", Command::Compose),
    ]]
}

/// The open message's commands; none while nothing is open or several
/// messages are being selected.
pub(crate) fn reader_bar(state: &MailState) -> BarSpec {
    if state.bulk_selecting {
        return Vec::new();
    }
    let Some(message) = state.selected.and_then(|id| state.mailbox.get(id)) else {
        return Vec::new();
    };
    let delete = if message.folder == Folder::Trash {
        "Delete permanently"
    } else {
        "Delete"
    };
    let mut triage = vec![BarItem::button(
        "Archive",
        Command::Message(CtxAction::Archive),
    )];
    if message.folder != Folder::Inbox && message.folder != Folder::Drafts {
        triage.push(BarItem::button(
            "Move to Inbox",
            Command::Message(CtxAction::MoveToInbox),
        ));
    }
    triage.push(BarItem::button(delete, Command::Message(CtxAction::Delete)));
    vec![
        vec![
            BarItem::button("Reply", Command::Message(CtxAction::Reply)),
            BarItem::button("Reply all", Command::Message(CtxAction::ReplyAll)),
            BarItem::button("Forward", Command::Message(CtxAction::Forward)),
        ],
        triage,
        vec![
            BarItem::toggle(
                "Star",
                message.starred,
                Command::Message(CtxAction::ToggleStar),
            ),
            BarItem::button(
                CtxAction::ToggleRead.label(message.read, message.starred),
                Command::Message(CtxAction::ToggleRead),
            ),
        ],
    ]
}

fn build(spec: &BarSpec, shared: &Shared) -> Toolbar {
    let mut bar = Toolbar::new(Orientation::Horizontal);
    for (index, group) in spec.iter().enumerate() {
        if index > 0 {
            bar = bar.push_separator();
        }
        for item in group {
            let command = item.command.clone();
            let shared = shared.clone();
            let on_click = move || shared.borrow_mut().push_command(command.clone());
            bar = match (item.icon, item.toggle) {
                (_, Some(on)) => bar.push_toggle(&item.label, on, on_click),
                (Some(icon), None) => bar.push_icon(icon, &item.label, on_click),
                (None, None) => bar.push_button(&item.label, on_click),
            };
        }
    }
    bar
}

/// A header toolbar kept in step with the mail state.
pub struct CommandBar {
    bar: Toolbar,
    shared: Shared,
    spec: BarSpec,
    of: fn(&MailState) -> BarSpec,
    rebuilt: bool,
}

impl CommandBar {
    fn new(shared: Shared, of: fn(&MailState) -> BarSpec) -> Self {
        let spec = of(&shared.borrow());
        Self {
            bar: build(&spec, &shared),
            shared,
            spec,
            of,
            rebuilt: false,
        }
    }

    /// The message list's header actions.
    pub fn list(shared: Shared) -> Self {
        Self::new(shared, list_bar)
    }

    /// The open message's header actions.
    pub fn reader(shared: Shared) -> Self {
        Self::new(shared, reader_bar)
    }

    fn refresh(&mut self) {
        let next = (self.of)(&self.shared.borrow());
        if next != self.spec {
            self.bar = build(&next, &self.shared);
            self.spec = next;
            self.rebuilt = true;
        }
    }
}

impl ui::widget::Decorator for CommandBar {
    type Inner = Toolbar;

    fn inner(&self) -> &Toolbar {
        &self.bar
    }

    fn inner_mut(&mut self) -> &mut Toolbar {
        &mut self.bar
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        self.refresh();
        ui::widget::Widget::sync(&mut self.bar, state, path);
    }

    fn take_structure_dirty(&mut self) -> bool {
        let rebuilt = std::mem::take(&mut self.rebuilt);
        ui::widget::Widget::take_structure_dirty(&mut self.bar) || rebuilt
    }

    fn is_layout_dirty(&self) -> bool {
        self.rebuilt || ui::widget::Widget::is_layout_dirty(&self.bar)
    }

    fn name(&self) -> &'static str {
        "MailCommandBar"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> MailState {
        MailState::new_sample(crate::app::avatar_store_offline())
    }

    fn labels(spec: &BarSpec) -> Vec<&str> {
        spec.iter()
            .flatten()
            .map(|item| item.label.as_str())
            .collect()
    }

    #[test]
    fn the_list_offers_search_and_selection_and_the_reader_the_message_actions() {
        let mut state = state();
        let list = list_bar(&state);
        assert_eq!(
            labels(&list),
            ["Search Inbox", "Select messages", "Refresh", "New message"]
        );
        let reader = reader_bar(&state);
        assert!(labels(&reader).contains(&"Reply all"));
        assert!(labels(&reader).contains(&"Archive"));
        let star = reader
            .iter()
            .flatten()
            .find(|item| item.label == "Star")
            .expect("star toggle");
        let starred = state
            .selected
            .and_then(|id| state.mailbox.get(id))
            .is_some_and(|message| message.starred);
        assert_eq!(star.toggle, Some(starred));

        state.set_bulk_selecting(true);
        assert!(reader_bar(&state).is_empty(), "selecting pauses the reader");
        let bulk = list_bar(&state);
        assert!(labels(&bulk).contains(&"Move to Trash"));
        assert!(labels(&bulk).contains(&"Done"));
    }
}
