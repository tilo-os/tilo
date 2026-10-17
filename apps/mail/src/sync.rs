//! What one folder sync asks a server for, and what it brings back.
//!
//! A folder is fetched in pages of its newest messages. After the first page
//! the app knows the folder's window: the UIDVALIDITY its server numbered it
//! with, and the newest and oldest UID it holds. A refresh then asks only for
//! what arrived after the newest one, plus the flags of the window, which
//! says what another client read, starred or removed. Scrolling past the
//! oldest loaded message asks for the page before it.

use crate::model::{Folder, Mailbox, Message};

/// How many messages one page brings.
pub const PAGE: u32 = 50;

/// More new messages than this since the last sync, and the folder starts
/// over from its newest page instead of fetching them all.
pub const CATCH_UP_LIMIT: u32 = 500;

/// The part of a folder this device holds, as its server numbers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    pub uid_validity: u32,
    pub newest: u32,
    pub oldest: u32,
}

impl Window {
    /// The window of `account`'s `folder` in `mailbox`: the UIDs its fetched
    /// messages carry. `None` before the folder was fetched, or when the
    /// messages disagree on the numbering (the server renumbered it).
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

/// What to fetch of a folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Want {
    /// What is new since `known` (or the newest page, when nothing is
    /// known or the server renumbered the folder), and the flags of
    /// `known`'s window.
    Latest { known: Option<Window> },
    /// The page before the oldest message of `known`.
    Older { known: Window },
}

/// What a folder sync brought back.
#[derive(Clone, Debug, Default)]
pub struct Synced {
    /// The numbering the folder has now.
    pub uid_validity: u32,
    /// Whatever the device held of this folder is out of date: drop every
    /// message of it not in `messages`.
    pub reset: bool,
    /// Messages new to the device, with their UIDs.
    pub messages: Vec<Message>,
    /// `(uid, seen, flagged)` of every message of the known window still on
    /// the server; a UID of the window missing here was removed.
    pub flags: Vec<(u32, bool, bool)>,
    /// The window `flags` covers.
    pub window: Option<(u32, u32)>,
    /// Whether the server has messages older than the oldest one held now;
    /// `None` when this sync did not find out.
    pub has_older: Option<bool>,
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

    #[test]
    fn a_window_spans_the_uids_of_one_account_and_folder() {
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
