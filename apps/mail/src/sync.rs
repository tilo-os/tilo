//! What one folder sync asks a server for, and what it brings back.
//!
//! A folder is fetched in pages of its newest messages. After the first page
//! the app knows the folder's window: the UIDVALIDITY its server numbered it
//! with, and the newest and oldest UID it holds. A refresh then asks only for
//! what arrived after the newest one, plus the flags of the window, which
//! says what another client read, starred or removed. Scrolling past the
//! oldest loaded message asks for the page before it.

use std::collections::HashMap;

use crate::model::{Folder, Mailbox, Message};

/// Where one account's server keeps each folder: Gmail's Sent is
/// `[Gmail]/Sent Mail`, Outlook's Trash is `Deleted Items`. Found from the
/// server's folder list, by the special use the server marks a folder with
/// (RFC 6154) and otherwise by the names servers commonly give it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Places {
    names: HashMap<Folder, String>,
}

/// The names a server commonly gives a folder that is not marked with its
/// use, compared without case against the whole name and its last part.
fn common_names(folder: Folder) -> &'static [&'static str] {
    match folder {
        Folder::Sent => &["sent", "sent items", "sent messages", "sent mail"],
        Folder::Drafts => &["drafts", "draft"],
        Folder::Archive => &["archive", "archives", "all mail"],
        Folder::Trash => &["trash", "deleted items", "deleted messages", "bin"],
        Folder::Junk => &["junk", "spam", "junk e-mail", "junk email", "bulk mail"],
        Folder::Inbox | Folder::Starred => &[],
    }
}

/// The special use (RFC 6154) that marks a folder, as a server lists it.
fn special_use(folder: Folder) -> &'static [&'static str] {
    match folder {
        Folder::Sent => &["Sent"],
        Folder::Drafts => &["Drafts"],
        // Archiving into "All Mail" is how Gmail archives.
        Folder::Archive => &["Archive", "All"],
        Folder::Trash => &["Trash"],
        Folder::Junk => &["Junk"],
        Folder::Inbox | Folder::Starred => &[],
    }
}

impl Places {
    /// The places a server's folder list gives: each listed folder's name
    /// and the special uses it is marked with (`"Sent"`, `"All"`, …).
    pub fn from_list(listed: &[(String, Vec<String>)]) -> Self {
        let mut names = HashMap::new();
        for folder in [
            Folder::Sent,
            Folder::Drafts,
            Folder::Archive,
            Folder::Trash,
            Folder::Junk,
        ] {
            let by_use = special_use(folder).iter().find_map(|wanted| {
                listed
                    .iter()
                    .find(|(_, uses)| uses.iter().any(|used| used.eq_ignore_ascii_case(wanted)))
            });
            let by_name = || {
                listed.iter().find(|(name, _)| {
                    let lower = name.to_lowercase();
                    let last = lower.rsplit(['/', '.']).next().unwrap_or(&lower);
                    common_names(folder)
                        .iter()
                        .any(|common| lower == *common || last == *common)
                })
            };
            if let Some((name, _)) = by_use.or_else(by_name) {
                names.insert(folder, name.clone());
            }
        }
        Self { names }
    }

    /// The name of `folder` on the server.
    pub fn name(&self, folder: Folder) -> &str {
        match folder {
            Folder::Inbox | Folder::Starred => "INBOX",
            other => self
                .names
                .get(&other)
                .map_or_else(|| other.imap_name(), String::as_str),
        }
    }
}

/// `text` as an IMAP quoted string, for a search: `None` for text the
/// quoted form cannot carry (anything but printable ASCII), which only the
/// local search then looks for.
pub fn imap_quoted(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() || !text.chars().all(|c| c.is_ascii() && !c.is_ascii_control()) {
        return None;
    }
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for c in text.chars() {
        if c == '"' || c == '\\' {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    quoted.push('"');
    Some(quoted)
}

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
    fn folders_are_found_by_their_use_then_by_their_usual_names() {
        let list = |entries: &[(&str, &[&str])]| -> Vec<(String, Vec<String>)> {
            entries
                .iter()
                .map(|(name, uses)| {
                    (
                        (*name).to_owned(),
                        uses.iter().map(|used| (*used).to_owned()).collect(),
                    )
                })
                .collect()
        };
        let gmail = Places::from_list(&list(&[
            ("INBOX", &[]),
            ("[Gmail]/All Mail", &["All"]),
            ("[Gmail]/Sent Mail", &["Sent"]),
            ("[Gmail]/Spam", &["Junk"]),
            ("[Gmail]/Trash", &["Trash"]),
            ("[Gmail]/Drafts", &["Drafts"]),
        ]));
        assert_eq!(gmail.name(Folder::Sent), "[Gmail]/Sent Mail");
        assert_eq!(gmail.name(Folder::Archive), "[Gmail]/All Mail");
        assert_eq!(gmail.name(Folder::Junk), "[Gmail]/Spam");
        assert_eq!(gmail.name(Folder::Starred), "INBOX");
        let outlook = Places::from_list(&list(&[
            ("INBOX", &[]),
            ("Sent Items", &[]),
            ("Deleted Items", &[]),
            ("Junk Email", &[]),
            ("Archive", &[]),
        ]));
        assert_eq!(outlook.name(Folder::Sent), "Sent Items");
        assert_eq!(outlook.name(Folder::Trash), "Deleted Items");
        assert_eq!(outlook.name(Folder::Junk), "Junk Email");
        assert_eq!(outlook.name(Folder::Archive), "Archive");
        let dovecot = Places::from_list(&list(&[("INBOX", &[]), ("INBOX.Sent", &[])]));
        assert_eq!(dovecot.name(Folder::Sent), "INBOX.Sent");
        assert_eq!(
            dovecot.name(Folder::Drafts),
            "Drafts",
            "a folder the server does not list keeps its usual name"
        );
    }

    #[test]
    fn search_text_is_quoted_for_imap_or_left_to_the_local_search() {
        assert_eq!(imap_quoted(" invoice ").as_deref(), Some("\"invoice\""));
        assert_eq!(
            imap_quoted(r#"say "hi" \ bye"#).as_deref(),
            Some(r#""say \"hi\" \\ bye""#)
        );
        assert_eq!(imap_quoted("Grüße"), None);
        assert_eq!(imap_quoted("line\nbreak"), None);
        assert_eq!(imap_quoted("  "), None);
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
