//! The mailbox sidebar: one [`NavList`] section per account, each listing
//! that account's folders.
//!
//! The list is the toolkit's; this decorator only keeps its sections and its
//! selection in step with [`MailState`] and turns a chosen row into a
//! [`Command`] the app applies with the document in hand.

use ui::icons::Icon;
use ui::widget::{AppModel, ModelPath, NavAction, NavItem, NavList, NavSection};

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

/// The sidebar's destinations for the current state.
pub(crate) fn sections(state: &MailState) -> Vec<NavSection> {
    state
        .accounts
        .accounts
        .iter()
        .enumerate()
        .map(|(index, account)| {
            // Only the active account's mailbox is loaded, so only its
            // folders can say how much is waiting in them.
            let loaded = index == state.accounts.active;
            let items = AccountStore::folders_for(account)
                .into_iter()
                .map(|folder| {
                    let item = NavItem::new(folder_row_id(index, folder), folder.title())
                        .icon(folder.icon());
                    if !loaded {
                        return item;
                    }
                    match folder {
                        // Unread mail is what waits for the person.
                        Folder::Inbox => match count(state.mailbox.unread_count(folder)) {
                            0 => item,
                            unread => item.live_count(unread),
                        },
                        // How many drafts are kept, and how much junk is
                        // unread, are plain counts.
                        Folder::Drafts => match count(state.mailbox.count(folder)) {
                            0 => item,
                            drafts => item.count(drafts),
                        },
                        Folder::Junk => match count(state.mailbox.unread_count(folder)) {
                            0 => item,
                            unread => item.count(unread),
                        },
                        _ => item,
                    }
                })
                .collect();
            let title = if account.config.email.is_empty() {
                account.label().to_owned()
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

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The row that shows the open folder.
pub(crate) fn selected_row(state: &MailState) -> Option<String> {
    (!state.accounts.is_empty()).then(|| folder_row_id(state.accounts.active, state.folder))
}

/// The accounts-and-folders sidebar.
pub struct MailSidebar {
    list: NavList,
    shared: Shared,
    shown: Vec<NavSection>,
    selected: Option<String>,
}

impl MailSidebar {
    pub fn new(shared: Shared) -> Self {
        let (shown, selected) = {
            let state = shared.borrow();
            (sections(&state), selected_row(&state))
        };
        let on_select = shared.clone();
        let on_section = shared.clone();
        let mut list = NavList::new(shown.clone())
            .with_id("mail-mailboxes")
            .with_accessible_name("Mailboxes")
            .on_select(move |id| {
                if let Some((account, folder)) = parse_folder_row(&id) {
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
            });
        list.set_selected(selected.as_deref());
        Self {
            list,
            shared,
            shown,
            selected,
        }
    }

    /// Bring the list in step with the state: new counts, accounts and the
    /// open folder.
    fn refresh(&mut self) {
        let (next, selected) = {
            let state = self.shared.borrow();
            (sections(&state), selected_row(&state))
        };
        if next != self.shown {
            self.list.set_sections(next.clone());
            self.shown = next;
        }
        if selected != self.selected || self.list.selected_id() != selected {
            self.list.set_selected(selected.as_deref());
            self.selected = selected;
        }
    }
}

impl ui::widget::Decorator for MailSidebar {
    type Inner = NavList;

    fn inner(&self) -> &NavList {
        &self.list
    }

    fn inner_mut(&mut self) -> &mut NavList {
        &mut self.list
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        self.refresh();
        ui::widget::Widget::sync(&mut self.list, state, path);
    }

    fn name(&self) -> &'static str {
        "MailSidebar"
    }
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
        assert_eq!(sections.len(), state.accounts.accounts.len());
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
        assert_eq!(other.count, None, "an unloaded account shows no counts");
        assert_eq!(selected_row(&state), Some(folder_row_id(0, Folder::Inbox)));
    }
}
