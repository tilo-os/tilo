//! The mailbox sidebar: one [`NavList`] section per account, each listing
//! that account's folders.
//!
//! The list is the toolkit's; this decorator only keeps its sections and its
//! selection in step with [`MailState`] and turns a chosen row into a
//! [`Command`] the app applies with the document in hand.

use tilo_actions::ActionBinding;
use ui::icons::Icon;
use ui::num::u32_of_usize;
use ui::widget::{NavAction, NavItem, NavList, NavSection};

use crate::account::AccountStore;
use crate::model::Folder;
use crate::state::{Command, MailState, Shared};

/// The id of the per-account settings action on each section header.
const ACCOUNT_SETTINGS: &str = "account-settings";

pub(crate) fn folder_row_id(account: usize, folder: Folder) -> String {
    format!("{account}/{}", folder.as_key())
}

/// The account and folder a row id names.
pub(crate) fn parse_folder_row(id: &str) -> Option<(usize, Folder)> {
    let (account, folder) = id.split_once('/')?;
    Some((account.parse().ok()?, Folder::from_key(folder)?))
}

fn section_id(account: usize) -> String {
    format!("account-{account}")
}

/// The row id of every account's inbox together.
pub(crate) const ALL_INBOXES: &str = "all/inbox";

/// The row id of the messages `account` still has to send.
fn outbox_row_id(account: usize) -> String {
    format!("{account}/outbox")
}

/// The account whose outbox a row id names.
fn parse_outbox_row(id: &str) -> Option<usize> {
    id.strip_suffix("/outbox")?.parse().ok()
}

/// The sidebar's destinations for the current state: with more than one
/// account, every inbox together first, then each account's folders.
pub(crate) fn sections(state: &MailState) -> Vec<NavSection> {
    let mut sections = Vec::new();
    if state.accounts.accounts.len() > 1 {
        let unread: usize = state
            .accounts
            .accounts
            .iter()
            .map(|account| {
                state
                    .mailbox
                    .unread_count_of(&account.config.email, Folder::Inbox)
            })
            .sum();
        let item = NavItem::new(ALL_INBOXES, "All inboxes")
            .icon(Icon::Inbox)
            .live_count(u32_of_usize(unread));
        sections.push(NavSection::untitled("all", vec![item]));
    }
    sections.extend(account_sections(state));
    sections
}

/// One section per account, listing its folders.
fn account_sections(state: &MailState) -> Vec<NavSection> {
    state
        .accounts
        .accounts
        .iter()
        .enumerate()
        .map(|(index, account)| {
            // The active account's folders count what the mailbox shows; an
            // other account's inbox counts the unread mail kept or fetched
            // for it.
            let active = index == state.accounts.active;
            let items = AccountStore::folders_for(account)
                .into_iter()
                .map(|folder| {
                    let item = NavItem::new(folder_row_id(index, folder), folder.title())
                        .icon(folder.icon());
                    if !active {
                        if folder != Folder::Inbox {
                            return item;
                        }
                        return match u32_of_usize(
                            state.mailbox.unread_count_of(&account.config.email, folder),
                        ) {
                            0 => item,
                            unread => item.live_count(unread),
                        };
                    }
                    match folder {
                        // Unread mail is what waits for the person.
                        Folder::Inbox => match u32_of_usize(state.mailbox.unread_count(folder)) {
                            0 => item,
                            unread => item.live_count(unread),
                        },
                        // How many drafts are kept, and how much junk is
                        // unread, are plain counts.
                        Folder::Drafts => match u32_of_usize(state.mailbox.count(folder)) {
                            0 => item,
                            drafts => item.count(drafts),
                        },
                        Folder::Junk => match u32_of_usize(state.mailbox.unread_count(folder)) {
                            0 => item,
                            unread => item.count(unread),
                        },
                        _ => item,
                    }
                })
                // The outbox is not a folder of the mailbox: it is the
                // messages that have not left it, so it stands at the end of
                // the account's own list, counted while anything waits.
                .chain(std::iter::once({
                    let item = NavItem::new(outbox_row_id(index), "Outbox").icon(Icon::Send);
                    match u32_of_usize(state.queued_count(index)) {
                        0 => item,
                        waiting => item.live_count(waiting),
                    }
                }))
                .collect();
            let title = if account.config.email.is_empty() {
                account.config.label().to_owned()
            } else {
                account.config.email.clone()
            };
            NavSection::new(section_id(index), title, items)
                .collapsible()
                .trailing_action(NavAction::new(
                    ACCOUNT_SETTINGS,
                    Icon::Settings,
                    "Account in Settings",
                ))
        })
        .collect()
}

/// The row that shows the open folder.
pub(crate) fn selected_row(state: &MailState) -> Option<String> {
    if state.all_inboxes {
        return Some(ALL_INBOXES.to_owned());
    }
    if state.outbox_open {
        return Some(outbox_row_id(state.outbox_account));
    }
    (!state.accounts.is_empty()).then(|| folder_row_id(state.accounts.active, state.folder))
}

/// The accounts-and-folders sidebar: the toolkit list, following the
/// accounts and the open folder and reporting choices into `shared`.
///
/// A section header's button opens Settings' External Accounts page: it runs
/// the published [`MailEffect::AccountSettings`](crate::actions::MailEffect)
/// when `settings` binds it, so the header, the window bar's own entry and
/// an agent all reach the same action.
pub fn sidebar(shared: &Shared, settings: Option<ActionBinding<String>>) -> NavList {
    let on_select = shared.clone();
    let follow = shared.clone();
    let mut list = NavList::new(Vec::new())
        .with_accessible_name("Mailboxes")
        .on_view("mail-mailboxes", move |id| {
            if id == ALL_INBOXES {
                on_select.borrow_mut().push_command(Command::OpenAllInboxes);
            } else if let Some(account) = parse_outbox_row(&id) {
                on_select
                    .borrow_mut()
                    .push_command(Command::OpenOutbox(account));
            } else if let Some((account, folder)) = parse_folder_row(&id) {
                on_select
                    .borrow_mut()
                    .push_command(Command::OpenFolder(account, folder));
            }
        });
    if let Some(settings) = settings {
        list = list.section_action(ACCOUNT_SETTINGS, settings);
    }
    list.follow(move || {
        let state = follow.try_borrow().ok()?;
        Some((sections(&state), selected_row(&state)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> MailState {
        MailState::new_sample(crate::avatars::Avatars::new())
    }

    #[test]
    fn rows_name_their_account_and_folder_and_only_unread_inbox_is_live() {
        for folder in Folder::ALL {
            assert_eq!(
                parse_folder_row(&folder_row_id(3, folder)),
                Some((3, folder))
            );
        }
        assert_eq!(parse_folder_row("3/unknown"), None);

        let state = state();
        let sections = sections(&state);
        assert_eq!(
            sections.len(),
            state.accounts.accounts.len() + 1,
            "every inbox together, then each account"
        );
        assert_eq!(sections[0].items[0].id, ALL_INBOXES);
        let sections = &sections[1..];
        let first = &sections[0];
        assert_eq!(first.title, state.accounts.accounts[0].config.email);
        let inbox = &first.items[0];
        assert_eq!(inbox.label, "Inbox");
        let unread = u32::try_from(state.mailbox.unread_count(Folder::Inbox)).unwrap();
        assert_eq!(
            inbox.count,
            (unread > 0).then_some(ui::chrome::NavCount::Live(unread))
        );
        let other = &sections[1].items[0];
        assert_eq!(
            other.count, None,
            "an account with no mail kept or fetched shows no counts"
        );
        assert_eq!(selected_row(&state), Some(folder_row_id(0, Folder::Inbox)));
    }
}
