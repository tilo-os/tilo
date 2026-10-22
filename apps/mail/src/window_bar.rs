//! Mail's toolbar, as slots in the window's bar ([`ui::window_toolbar`]).
//!
//! The bar shows the folder as the window's location (or how many messages
//! are selected), and what can be done there: New message, and the open
//! message's actions (reply, archive, delete, star, …), or the selection's
//! while several messages are selected; the rarer ones in the bar's menu.
//! Each slot only queues a [`Command`], which the app applies with the
//! document in hand. Search is the bar's own.

use ui::window_toolbar::{ToolbarAction, ToolbarLocation, WindowToolbar};

use crate::model::Folder;
use crate::state::{BulkAction, Command, CtxAction, MailState};

/// One action of the window's bar and the command it queues.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BarItem {
    pub action: ToolbarAction,
    pub command: Command,
}

impl BarItem {
    fn new(id: &str, icon: &str, name: impl Into<String>, command: Command) -> Self {
        Self {
            action: ToolbarAction::new(id, icon, name),
            command,
        }
    }

    fn on(mut self, on: bool) -> Self {
        self.action = self.action.on(on);
        self
    }

    fn in_menu(mut self) -> Self {
        self.action = self.action.in_menu();
        self
    }
}

/// What can be done to the selected messages: Done, triage, then read
/// state and Select all in the menu.
fn bulk_items(state: &MailState) -> Vec<BarItem> {
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
        BarItem::new("done", "check", "Done", Command::ToggleBulk),
        BarItem::new(
            "bulk-archive",
            "archive",
            BulkAction::Archive.label(),
            Command::Bulk(BulkAction::Archive),
        ),
        BarItem::new(
            "bulk-delete",
            "trash",
            delete.label(),
            Command::Bulk(delete),
        ),
        BarItem::new(
            "bulk-read",
            "mail-open",
            BulkAction::MarkRead.label(),
            Command::Bulk(BulkAction::MarkRead),
        )
        .in_menu(),
        BarItem::new(
            "bulk-unread",
            "mail",
            BulkAction::MarkUnread.label(),
            Command::Bulk(BulkAction::MarkUnread),
        )
        .in_menu(),
        BarItem::new(
            "select-all",
            "list-checks",
            "Select all",
            Command::SelectAllVisible,
        )
        .in_menu(),
    ]
}

/// The open message's actions: Reply and Archive in the row, the rest in
/// the menu.
fn message_items(state: &MailState) -> Vec<BarItem> {
    let Some(message) = state.selected.and_then(|id| state.mailbox.get(id)) else {
        return Vec::new();
    };
    let delete = if message.folder == Folder::Trash {
        "Delete permanently"
    } else {
        "Delete"
    };
    let mut items = vec![
        BarItem::new(
            "reply",
            "reply",
            "Reply",
            Command::Message(CtxAction::Reply),
        ),
        BarItem::new(
            "archive",
            "archive",
            "Archive",
            Command::Message(CtxAction::Archive),
        ),
        BarItem::new(
            "reply-all",
            "reply-all",
            "Reply all",
            Command::Message(CtxAction::ReplyAll),
        )
        .in_menu(),
        BarItem::new(
            "forward",
            "forward",
            "Forward",
            Command::Message(CtxAction::Forward),
        )
        .in_menu(),
    ];
    if message.folder != Folder::Inbox && message.folder != Folder::Drafts {
        items.push(
            BarItem::new(
                "move-to-inbox",
                "inbox",
                "Move to Inbox",
                Command::Message(CtxAction::MoveToInbox),
            )
            .in_menu(),
        );
    }
    items.push(
        BarItem::new(
            "delete",
            "trash",
            delete,
            Command::Message(CtxAction::Delete),
        )
        .in_menu(),
    );
    items.push(
        BarItem::new(
            "star",
            "star",
            "Star",
            Command::Message(CtxAction::ToggleStar),
        )
        .on(message.starred)
        .in_menu(),
    );
    items.push(
        BarItem::new(
            "read",
            if message.read { "mail" } else { "mail-open" },
            CtxAction::ToggleRead.label(message.read, message.starred),
            Command::Message(CtxAction::ToggleRead),
        )
        .in_menu(),
    );
    items
}

/// The bar's actions: New message and the open message's (or the
/// selection's) actions, then selecting several, writing to a contact and
/// adding an account in the menu. Before any account, nothing: the empty
/// window says what to do.
pub(crate) fn bar_items(state: &MailState, first_run: bool) -> Vec<BarItem> {
    let add_account = BarItem::new(
        "add-account",
        "user-plus",
        "Add account",
        Command::AccountSettings,
    );
    // Before any account the empty window's own Add account is the one
    // place to start.
    if first_run {
        return Vec::new();
    }
    if state.bulk_selecting {
        return bulk_items(state);
    }
    let mut items = vec![BarItem::new(
        "new-message",
        "pencil",
        "New message",
        Command::Compose,
    )];
    items.extend(message_items(state));
    items.push(
        BarItem::new(
            "select-messages",
            "square",
            "Select messages",
            Command::ToggleBulk,
        )
        .in_menu(),
    );
    items.push(
        BarItem::new(
            "contact-recipients",
            "users",
            crate::i18n::t("mail-contact-write"),
            Command::ContactRecipients,
        )
        .in_menu(),
    );
    items.push(add_account.in_menu());
    items
}

/// Where the window is: the open folder, every inbox, or how many messages
/// are selected.
pub(crate) fn location(state: &MailState, composing: Option<&str>) -> String {
    if let Some(heading) = composing {
        return heading.to_owned();
    }
    if state.bulk_selecting {
        return match state.selected_bulk_ids().len() {
            0 => "Select messages".to_owned(),
            1 => "1 selected".to_owned(),
            n => format!("{n} selected"),
        };
    }
    // The open message, where it is in view: beside the list, or pushed
    // over it in a narrow window.
    let open = state
        .selected
        .filter(|_| !state.narrow || state.detail_pushed)
        .and_then(|id| state.mailbox.get(id));
    if let Some(message) = open {
        return if message.subject.trim().is_empty() {
            "(no subject)".to_owned()
        } else {
            message.subject.clone()
        };
    }
    if state.all_inboxes {
        "All inboxes".to_owned()
    } else {
        state.folder.title().to_owned()
    }
}

/// The bar's slots: `base` (the shell's own) with the location (what is
/// open: the message being written, the open message, else the folder) and
/// `items`.
pub(crate) fn toolbar(
    mut base: WindowToolbar,
    state: &MailState,
    items: &[BarItem],
    first_run: bool,
    composing: Option<&str>,
) -> WindowToolbar {
    // Before any account there is nothing open: the window is Mail.
    let label = if first_run && composing.is_none() {
        "Mail".to_owned()
    } else {
        location(state, composing)
    };
    base.location = Some(ToolbarLocation {
        label,
        ..ToolbarLocation::default()
    });
    ui::widget::NavShell::push_window_actions(
        &mut base,
        items.iter().map(|item| item.action.clone()),
    );
    base
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> MailState {
        MailState::new_sample(crate::avatars::Avatars::new())
    }

    fn names(items: &[BarItem], menu: bool) -> Vec<&str> {
        items
            .iter()
            .filter(|item| item.action.menu == menu)
            .map(|item| item.action.name.as_str())
            .collect()
    }

    #[test]
    fn the_bar_offers_new_message_and_the_open_messages_actions_and_the_selections() {
        let mut state = state();
        let items = bar_items(&state, false);
        assert_eq!(names(&items, false), ["New message", "Reply", "Archive"]);
        let menu = names(&items, true);
        for name in [
            "Reply all",
            "Forward",
            "Select messages",
            "Write to a contact",
        ] {
            assert!(
                menu.contains(&name),
                "{name} is in the bar's menu: {menu:?}"
            );
        }
        let star = items
            .iter()
            .find(|item| item.action.name == "Star")
            .expect("star toggle");
        let starred = state
            .selected
            .and_then(|id| state.mailbox.get(id))
            .is_some_and(|message| message.starred);
        assert_eq!(star.action.on, starred);

        state.set_bulk_selecting(true);
        let items = bar_items(&state, false);
        assert_eq!(names(&items, false), ["Done", "Archive", "Move to Trash"]);
        assert!(names(&items, true).contains(&"Select all"));
        assert_eq!(location(&state, None), "Select messages");
        assert_eq!(location(&state, Some("New message")), "New message");

        assert!(
            bar_items(&state, true).is_empty(),
            "before any account the empty window's Add account is the one"
        );
    }
}
