//! What one folder sync asks a server for, and what it brings back.
//!
//! A folder is fetched in pages of its newest messages. After the first page
//! the app knows the folder's window: the UIDVALIDITY its server numbered it
//! with, and the newest and oldest UID it holds. A refresh then asks only for
//! what arrived after the newest one, plus the flags of the window, which
//! says what another client read, starred or removed. Scrolling past the
//! oldest loaded message asks for the page before it.

use crate::model::{Folder, Mailbox, Message};

pub const CATCH_UP_LIMIT: u32 = 500;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    pub uid_validity: u32,
    pub newest: u32,
    pub oldest: u32,
}

impl Window {
    pub fn of(mailbox: &Mailbox, account: &str, folder: Folder) -> Option<Self> {
        let mut window: Option<Self> = None;
        for message in mailbox.messages.values() {
            if message.account != account {
                continue;
            }
            let Some(at) = message.server.filter(|at| at.folder == folder) else {
                continue;
            };
            let (uid, validity) = (at.uid, at.uid_validity);
            window = Some(match window {
                None => Self {
                    uid_validity: validity,
                    newest: uid,
                    oldest: uid,
                },
                Some(seen) if seen.uid_validity != validity => return None,
                Some(seen) => Self {
                    uid_validity: validity,
                    newest: seen.newest.max(uid),
                    oldest: seen.oldest.min(uid),
                },
            });
        }
        window
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Want {
    Latest { known: Option<Window> },
    Older { known: Window },
}

#[derive(Clone, Debug, Default)]
pub struct Synced {
    pub uid_validity: u32,
    pub reset: bool,
    pub messages: Vec<Message>,
    pub flags: Vec<(u32, bool, bool)>,
    pub window: Option<(u32, u32)>,
    /// Whether the server has messages older than the oldest one held now;
    pub has_older: Option<bool>,
    pub originals: Vec<(crate::model::MessageId, Vec<u8>)>,
}

impl Synced {
    pub fn receive(&mut self, fetched: Vec<(Message, Vec<u8>)>) {
        for (message, original) in fetched {
            self.originals.push((message.id, original));
            self.messages.push(message);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(account: &str, uid: u32, validity: u32) -> Message {
        let raw =
            format!("From: a@x\r\nSubject: s\r\nMessage-ID: <{uid}.{validity}@x>\r\n\r\nb\r\n");
        let mut message =
            crate::transport::parse_eml(raw.as_bytes(), i64::from(uid), Folder::Inbox).unwrap();
        message.account = account.to_owned();
        message.server = Some(crate::model::ServerPlace {
            folder: Folder::Inbox,
            uid,
            uid_validity: validity,
        });
        message
    }

    tilo_test_env::scenario! {
        /// What a sync asks an IMAP server for.
        fn a_sync_names_its_folders_search_text_and_uid_window;

        phase a_window_spans_the_uids_of_one_account_and_folder;
        {
            let mut mailbox = Mailbox::default();
            assert_eq!(Window::of(&mailbox, "me@x", Folder::Inbox), None);
            for uid in [7, 3, 12] {
                mailbox.insert(message("me@x", uid, 9));
            }
            mailbox.insert(message("other@x", 40, 9));
            assert_eq!(
                Window::of(&mailbox, "me@x", Folder::Inbox),
                Some(Window {
                    uid_validity: 9,
                    newest: 12,
                    oldest: 3
                })
            );
            mailbox.insert(message("me@x", 13, 10));
            assert_eq!(
                Window::of(&mailbox, "me@x", Folder::Inbox),
                None,
                "a renumbered folder starts over"
            );
        }
    }
}
