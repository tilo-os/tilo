//! The actions in the window's header row: the message list's (search,
//! select, new message, write to a contact; or the selection's actions) and the open
//! message's (reply, archive, star, …).
//!
//! Each is a row of the shell's header buttons, rebuilt from [`MailState`]
//! whenever what it offers changes; a command only queues a [`Command`],
//! which the app applies with the document in hand.

use ui::icons::Icon;
use ui::widget::{Button, ButtonRole, Element};

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

    fn icon_toggle(icon: Icon, label: impl Into<String>, on: bool, command: Command) -> Self {
        Self {
            label: label.into(),
            icon: Some(icon),
            toggle: Some(on),
            command,
        }
    }
}

/// What a header toolbar offers: its commands in groups.
pub(crate) type BarSpec = Vec<Vec<BarItem>>;

/// What can be done to the selected messages, in groups: triage, then
/// read state.
fn bulk_actions(state: &MailState) -> BarSpec {
    let ids = state.selected_bulk_ids();
    let all_trash = !ids.is_empty()
        && ids.iter().all(|id| {
            state
                .mailbox
                .get(*id)
                .is_some_and(|message| message.folder == Folder::Trash)
        });
    let delete = if all_trash {
        BulkAction::Purge
    } else {
        BulkAction::Delete
    };
    vec![
        vec![
            BarItem::icon(
                Icon::Archive,
                BulkAction::Archive.label(),
                Command::Bulk(BulkAction::Archive),
            ),
            BarItem::icon(Icon::Trash, delete.label(), Command::Bulk(delete)),
        ],
        vec![
            BarItem::icon(
                Icon::MailOpen,
                BulkAction::MarkRead.label(),
                Command::Bulk(BulkAction::MarkRead),
            ),
            BarItem::icon(
                Icon::Mail,
                BulkAction::MarkUnread.label(),
                Command::Bulk(BulkAction::MarkUnread),
            ),
        ],
    ]
}

/// The message list's commands. While several messages are selected the
/// list keeps choosing them; what to do with them stands over the message
/// column, or here when that column is not beside the list.
pub(crate) fn list_bar(state: &MailState) -> BarSpec {
    if state.bulk_selecting {
        let select_all = BarItem::button("Select all", Command::SelectAllVisible);
        let done = BarItem::button("Done", Command::ToggleBulk);
        if !state.narrow {
            return vec![vec![select_all, done]];
        }
        // A narrow header moves its last commands into the More menu first:
        // Done stays, the selection's own actions follow it.
        let mut groups = vec![vec![done]];
        groups.extend(bulk_actions(state));
        groups.push(vec![select_all]);
        return groups;
    }
    vec![vec![
        BarItem::icon(
            Icon::Search,
            format!("Search {}", state.folder.title()),
            Command::OpenSearch,
        ),
        BarItem::icon(Icon::Square, "Select messages", Command::ToggleBulk),
        BarItem::icon(Icon::Pencil, "New message", Command::Compose),
        BarItem::icon(
            Icon::Users,
            crate::i18n::t("mail-contact-write"),
            Command::ContactRecipients,
        ),
    ]]
}

/// The open message's commands, the selection's while several messages are
/// selected, and none while nothing is open.
pub(crate) fn reader_bar(state: &MailState) -> BarSpec {
    if state.bulk_selecting {
        return if state.narrow {
            Vec::new()
        } else {
            bulk_actions(state)
        };
    }
    let Some(message) = state.selected.and_then(|id| state.mailbox.get(id)) else {
        return Vec::new();
    };
    let delete = if message.folder == Folder::Trash {
        "Delete permanently"
    } else {
        "Delete"
    };
    if state.narrow {
        // A narrow window's header shares its row with the Back button: the
        // commonest actions stand there as icons and the rest wait in the
        // message's menu.
        return vec![vec![
            BarItem::icon(Icon::Reply, "Reply", Command::Message(CtxAction::Reply)),
            BarItem::icon(
                Icon::Archive,
                "Archive",
                Command::Message(CtxAction::Archive),
            ),
            BarItem::icon(Icon::Trash, delete, Command::Message(CtxAction::Delete)),
            BarItem::icon(Icon::MoreHorizontal, "More actions", Command::MessageMenu),
        ]];
    }
    let mut triage = vec![BarItem::icon(
        Icon::Archive,
        "Archive",
        Command::Message(CtxAction::Archive),
    )];
    if message.folder != Folder::Inbox && message.folder != Folder::Drafts {
        triage.push(BarItem::icon(
            Icon::Inbox,
            "Move to Inbox",
            Command::Message(CtxAction::MoveToInbox),
        ));
    }
    triage.push(BarItem::icon(
        Icon::Trash,
        delete,
        Command::Message(CtxAction::Delete),
    ));
    let read_icon = if message.read {
        Icon::Mail
    } else {
        Icon::MailOpen
    };
    vec![
        vec![
            BarItem::icon(Icon::Reply, "Reply", Command::Message(CtxAction::Reply)),
            BarItem::icon(
                Icon::ReplyAll,
                "Reply all",
                Command::Message(CtxAction::ReplyAll),
            ),
            BarItem::icon(
                Icon::Forward,
                "Forward",
                Command::Message(CtxAction::Forward),
            ),
        ],
        triage,
        vec![
            BarItem::icon_toggle(
                Icon::Star,
                "Star",
                message.starred,
                Command::Message(CtxAction::ToggleStar),
            ),
            BarItem::icon(
                read_icon,
                CtxAction::ToggleRead.label(message.read, message.starred),
                Command::Message(CtxAction::ToggleRead),
            ),
        ],
    ]
}

/// The header's actions for `spec`: one button per command, in order, so
/// the shell collapses labels to icons and moves what does not fit into
/// its More menu one command at a time. `column` names the header in the
/// buttons' ids.
pub(crate) fn header_actions(spec: &BarSpec, shared: &Shared, column: &str) -> Vec<Element> {
    spec.iter()
        .flatten()
        .enumerate()
        .map(|(index, item)| {
            let command = item.command.clone();
            let shared = shared.clone();
            let on_click = move || shared.borrow_mut().push_command(command.clone());
            let id = format!("mail-{column}-action-{index}");
            let button = match item.icon {
                Some(icon) => Button::icon_only(&id, icon, &item.label),
                None => Button::new(&id, &item.label).role(ButtonRole::Plain),
            };
            let button = match item.toggle {
                Some(on) => button.toggled(on),
                None => button,
            };
            Box::new(button.on_click(on_click)) as Element
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> MailState {
        MailState::new_sample(crate::avatars::Avatars::new())
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
            [
                "Search Inbox",
                "Select messages",
                "New message",
                "Write to a contact"
            ]
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
        assert_eq!(labels(&list_bar(&state)), ["Select all", "Done"]);
        assert!(
            labels(&reader_bar(&state)).contains(&"Move to Trash"),
            "the selection's actions stand over the message column"
        );
        state.narrow = true;
        assert!(reader_bar(&state).is_empty());
        state.set_bulk_selecting(false);
        assert_eq!(
            labels(&reader_bar(&state)),
            ["Reply", "Archive", "Delete", "More actions"],
            "a narrow header leaves room for the Back button"
        );
        state.set_bulk_selecting(true);
        let narrow = list_bar(&state);
        assert!(labels(&narrow).contains(&"Move to Trash"));
        assert!(labels(&narrow).contains(&"Done"));
    }
}
