//! The mailbox sidebar: one [`NavList`] section per account, each listing
//! that account's folders.
//!
//! The list is the toolkit's; this decorator only keeps its sections and its
//! selection in step with [`MailState`] and turns a chosen row into a
//! [`Command`] the app applies with the document in hand.

use ui::icons::Icon;
use ui::num::u32_of_usize;
use ui::widget::{NavAction, NavItem, NavList, NavSection};

use crate::account::AccountStore;
use crate::model::Folder;
use crate::state::{Command, MailState, Shared};

/// The id of the per-account settings action on each section header.
const ACCOUNT_SETTINGS: &str = "account-settings";

/// The row id of `folder` in account `account`.
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

fn parse_section(id: &str) -> Option<usize> {
    id.strip_prefix("account-")?.parse().ok()
}

/// The row id of every account's inbox together.
pub(crate) const ALL_INBOXES: &str = "all/inbox";

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
                    "Account settings",
                ))
        })
        .collect()
}

/// The row that shows the open folder.
pub(crate) fn selected_row(state: &MailState) -> Option<String> {
    if state.all_inboxes {
        return Some(ALL_INBOXES.to_owned());
    }
    (!state.accounts.is_empty()).then(|| folder_row_id(state.accounts.active, state.folder))
}

/// The accounts-and-folders sidebar: the toolkit list, following the
/// accounts and the open folder and reporting choices into `shared`.
pub fn sidebar(shared: &Shared) -> NavList {
    let on_select = shared.clone();
    let on_section = shared.clone();
    let follow = shared.clone();
    NavList::new(Vec::new())
        .with_id("mail-mailboxes")
        .with_accessible_name("Mailboxes")
        .on_select(move |id| {
            if id == ALL_INBOXES {
                on_select.borrow_mut().push_command(Command::OpenAllInboxes);
            } else if let Some((account, folder)) = parse_folder_row(&id) {
                on_select
                    .borrow_mut()
                    .push_command(Command::OpenFolder(account, folder));
            }
        })
        .on_section_action(move |section, action| {
            if action == ACCOUNT_SETTINGS
                && let Some(account) = parse_section(section)
            {
                on_section
                    .borrow_mut()
                    .push_command(Command::AccountSettings(Some(account)));
            }
        })
        .follow(move || {
            let state = follow.try_borrow().ok()?;
            Some((sections(&state), selected_row(&state)))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> MailState {
        MailState::new_sample(crate::app::avatar_store_offline())
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
        assert_eq!(parse_section(&section_id(2)), Some(2));

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
