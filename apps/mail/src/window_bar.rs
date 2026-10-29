//! Mail's toolbar, as slots in the window's bar ([`ui::window_toolbar`]).
//!
//! The bar shows the folder as the window's location (or how many messages
//! are selected), and what can be done there: New message, and the open
//! message's actions (reply, archive, delete, star, …), or the selection's
//! while several messages are selected; the rarer ones in the bar's menu.
//! Each slot only queues a [`Command`], which the app applies with the
//! document in hand. Search is the bar's own.

use ui::framework::BarCommands;
use ui::window_toolbar::{ToolbarAction, ToolbarLocation, WindowToolbar};

use crate::model::Folder;
use crate::state::{BulkAction, Command, CtxAction, MailState};

/// What can be done to the selected messages: Done, triage, then read
/// state and Select all in the menu.
fn bulk_items(bar: &BarCommands<Command>, state: &MailState) -> Vec<ToolbarAction> {
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
        bar.entry("check", "Done", Command::ToggleBulk),
        bar.entry(
            "archive",
            BulkAction::Archive.label(),
            Command::Bulk(BulkAction::Archive),
        ),
        bar.entry("trash", delete.label(), Command::Bulk(delete)),
        bar.entry(
            "mail-open",
            BulkAction::MarkRead.label(),
            Command::Bulk(BulkAction::MarkRead),
        )
        .in_menu(),
        bar.entry(
            "mail",
            BulkAction::MarkUnread.label(),
            Command::Bulk(BulkAction::MarkUnread),
        )
        .in_menu(),
        bar.entry("list-checks", "Select all", Command::SelectAllVisible)
            .in_menu(),
    ]
}

/// The open message's actions: Reply and Archive in the row, the rest in
/// the menu.
fn message_items(bar: &BarCommands<Command>, state: &MailState) -> Vec<ToolbarAction> {
    let Some(message) = state.selected.and_then(|id| state.mailbox.get(id)) else {
        return Vec::new();
    };
    let delete = if message.folder == Folder::Trash {
        "Delete permanently"
    } else {
        "Delete"
    };
    let mut items = vec![
        bar.entry("reply", "Reply", Command::Message(CtxAction::Reply)),
        bar.entry("archive", "Archive", Command::Message(CtxAction::Archive)),
        bar.entry(
            "reply-all",
            "Reply all",
            Command::Message(CtxAction::ReplyAll),
        )
        .in_menu(),
        bar.entry("forward", "Forward", Command::Message(CtxAction::Forward))
            .in_menu(),
    ];
    if message.folder != Folder::Inbox && message.folder != Folder::Drafts {
        items.push(
            bar.entry(
                "inbox",
                "Move to Inbox",
                Command::Message(CtxAction::MoveToInbox),
            )
            .in_menu(),
        );
    }
    items.push(
        bar.entry("trash", delete, Command::Message(CtxAction::Delete))
            .in_menu(),
    );
    items.push(
        bar.entry("star", "Star", Command::Message(CtxAction::ToggleStar))
            .on(message.starred)
            .in_menu(),
    );
    items.push(
        bar.entry(
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
/// adding an account in the menu. While a message is written (`composing`)
/// Discard draft and Close take the place of New message and the open
/// message's actions. Before any account, nothing but those: the empty
/// window says what to do.
pub(crate) fn bar_items(
    bar: &BarCommands<Command>,
    state: &MailState,
    first_run: bool,
    composing: bool,
) -> Vec<ToolbarAction> {
    let add_account = bar.entry("user-plus", "Add account", Command::AccountSettings);
    // The outbox: what waits to leave has one thing to be done with it here,
    // and New message beside it, since the person is plainly about to write.
    if state.outbox_open && !composing {
        let mut items = vec![bar.entry("pencil", "New message", Command::Compose)];
        if let Some(queued) = state.open_queued() {
            items.push(bar.entry(
                "rotate-ccw",
                "Cancel sending",
                Command::CancelSend(queued.id.clone()),
            ));
        }
        items.push(add_account.in_menu());
        return items;
    }
    // Before any account the empty window's own Add account is the one
    // place to start.
    // The message being written: Discard draft, and Close, which keeps
    // anything written in Drafts. Neither wears the window's own ✕.
    let writing: Vec<ToolbarAction> = if composing {
        vec![
            bar.entry("trash-2", "Discard draft", Command::DiscardCompose),
            bar.entry(
                "minimize-2",
                "Close and keep in Drafts",
                Command::CloseCompose,
            ),
        ]
    } else {
        Vec::new()
    };
    if first_run {
        return writing;
    }
    if state.bulk_selecting {
        return writing.into_iter().chain(bulk_items(bar, state)).collect();
    }
    // The message being written stands where the open message did: its
    // actions take the place of New message and the open message's.
    let mut items: Vec<ToolbarAction> = if composing {
        writing
    } else {
        let mut items = vec![bar.entry("pencil", "New message", Command::Compose)];
        items.extend(message_items(bar, state));
        items
    };
    items.push(
        bar.entry("square", "Select messages", Command::ToggleBulk)
            .in_menu(),
    );
    items.push(
        bar.entry(
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
    if state.outbox_open {
        return "Outbox".to_owned();
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
    items: Vec<ToolbarAction>,
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
    ui::widget::NavShell::push_window_actions(&mut base, items);
    base
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> MailState {
        MailState::new_sample(crate::avatars::Avatars::new())
    }

    fn names(items: &[ToolbarAction], menu: bool) -> Vec<&str> {
        items
            .iter()
            .filter(|item| item.menu == menu)
            .map(|item| item.name.as_str())
            .collect()
    }

    /// The command a press on `item` queues.
    fn command(bar: &BarCommands<Command>, item: &ToolbarAction) -> Option<Command> {
        bar.pressed(&ui::window_toolbar::ToolbarEvent::Action {
            id: item.id.clone(),
        })
    }

    #[test]
    fn the_bar_offers_new_message_and_the_open_messages_actions_and_the_selections() {
        let bar = BarCommands::new();
        let mut state = state();
        let items = bar_items(&bar, &state, false, false);
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
            .find(|item| item.name == "Star")
            .expect("star toggle");
        let starred = state
            .selected
            .and_then(|id| state.mailbox.get(id))
            .is_some_and(|message| message.starred);
        assert_eq!(star.on, starred);

        state.set_bulk_selecting(true);
        let items = bar_items(&bar, &state, false, false);
        assert_eq!(names(&items, false), ["Done", "Archive", "Move to Trash"]);
        assert!(names(&items, true).contains(&"Select all"));
        assert_eq!(location(&state, None), "Select messages");
        assert_eq!(location(&state, Some("New message")), "New message");

        assert!(
            bar_items(&bar, &state, true, false).is_empty(),
            "before any account the empty window's Add account is the one"
        );
    }

    #[test]
    fn the_outbox_offers_taking_the_open_message_back_unsent() {
        let bar = BarCommands::new();
        let mut state = state();
        state.outbox = vec![crate::state::QueuedMessage {
            id: "queued".into(),
            account: state.accounts.active_config().email,
            to: "ada@analytical.example".into(),
            subject: "Notes on the second engine".into(),
            body: "Sent from the platform on the train.".into(),
            reason: Some("550 5.7.1 refused as spam".into()),
        }];
        state.open_outbox(state.accounts.active);
        state.outbox_selected = Some("queued".into());
        let items = bar_items(&bar, &state, false, false);
        assert_eq!(names(&items, false), ["New message", "Cancel sending"]);
        assert_eq!(location(&state, None), "Outbox");
        let cancel = items
            .iter()
            .find(|item| item.name == "Cancel sending")
            .expect("the verb is in the bar");
        assert_eq!(
            command(&bar, cancel),
            Some(Command::CancelSend("queued".into()))
        );
        assert!(
            !cancel.destructive,
            "Undo puts the message back in the outbox, so it is no deletion"
        );
        assert!(
            items.iter().all(|item| item.icon != "x"),
            "nothing the outbox offers wears the window's own close"
        );
    }

    #[test]
    fn while_a_message_is_written_the_bar_closes_it() {
        let bar = BarCommands::new();
        let state = state();
        let items = bar_items(&bar, &state, false, true);
        assert_eq!(
            names(&items, false),
            ["Discard draft", "Close and keep in Drafts"]
        );
        assert_eq!(
            items
                .iter()
                .filter(|item| !item.menu)
                .map(|item| command(&bar, item))
                .collect::<Vec<_>>(),
            [Some(Command::DiscardCompose), Some(Command::CloseCompose)]
        );
        assert!(
            items.iter().all(|item| item.icon != "x"),
            "nothing the message offers wears the window's own close"
        );
        assert_eq!(
            items.first().map(|item| item.icon.as_str()),
            Some("trash-2")
        );
        assert!(
            names(&items, true).contains(&"Select messages"),
            "the menu stays"
        );
        assert_eq!(location(&state, Some("New message")), "New message");
    }
}
