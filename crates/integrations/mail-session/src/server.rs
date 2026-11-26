//! What a signed-in connection to an account's IMAP server is asked: where its folders are,
//! a page of a folder, what it finds, what changed about a message, and a draft or a sent
//! message to keep. Every mail client of Tilo's asks through these, the engine a device's Mail
//! runs and the session a host carries alike, each reading the messages it is answered into its
//! own shape.

use std::collections::HashMap;

pub use crate::shapes::{FetchedMessage, MailFlag, MarkMail};
use futures_util::TryStreamExt as _;

/// A signed-in connection to an account's IMAP server.
pub type ImapSession = async_imap::Session<tilo_account_probe::imap::SecureStream>;

/// The messages one page holds at most, newest first.
pub const PAGE: u32 = 50;

/// What each folder a mail program shows is: its key, the special-use marks a server may give
/// it (RFC 6154), the names the common servers give it, in lower case, and the name it is made
/// under on a server that holds none.
const KINDS: [(&str, &[&str], &[&str], &str); 5] = [
    (
        "sent",
        &["Sent"],
        &["sent", "sent items", "sent messages", "sent mail"],
        "Sent",
    ),
    ("drafts", &["Drafts"], &["drafts", "draft"], "Drafts"),
    (
        "archive",
        &["Archive", "All"],
        &["archive", "archives", "all mail"],
        "Archive",
    ),
    (
        "trash",
        &["Trash"],
        &["trash", "deleted items", "deleted messages", "bin"],
        "Trash",
    ),
    (
        "junk",
        &["Junk"],
        &["junk", "spam", "junk e-mail", "junk email", "bulk mail"],
        "Junk",
    ),
];

/// Where an account's folders live on its server, as the server listed them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Places {
    names: HashMap<&'static str, String>,
}

impl Places {
    /// The places a server's folder list says: each folder found by its special-use mark
    /// first, then by the name the common servers give it, its last part included (Dovecot's
    /// `INBOX.Sent`).
    #[must_use]
    pub fn from_list(listed: &[(String, Vec<String>)]) -> Self {
        let mut names = HashMap::new();
        for (key, uses, common, _) in KINDS {
            let by_use = uses.iter().find_map(|wanted| {
                listed
                    .iter()
                    .find(|(_, marks)| marks.iter().any(|mark| mark.eq_ignore_ascii_case(wanted)))
            });
            let by_name = || {
                listed.iter().find(|(name, _)| {
                    let lower = name.to_lowercase();
                    let last = lower.rsplit(['/', '.']).next().unwrap_or(&lower);
                    common
                        .iter()
                        .any(|common| lower == *common || last == *common)
                })
            };
            if let Some((name, _)) = by_use.or_else(by_name) {
                names.insert(key, name.clone());
            }
        }
        Self { names }
    }

    /// The server's own folder for `folder` (`inbox`, `starred`, `sent`, `drafts`, `archive`,
    /// `trash` or `junk`), when it listed one. Starred is the inbox seen through its flag.
    #[must_use]
    pub fn listed(&self, folder: &str) -> Option<&str> {
        match folder {
            "inbox" | "starred" => Some("INBOX"),
            other => self.names.get(other).map(String::as_str),
        }
    }

    /// [`Self::listed`], or for one the server did not list the name it is made under.
    #[must_use]
    pub fn name(&self, folder: &str) -> Option<&str> {
        self.listed(folder).or_else(|| {
            KINDS
                .iter()
                .find(|(key, ..)| *key == folder)
                .map(|(.., made)| *made)
        })
    }
}

/// The places of the folders the server holds now.
///
/// # Errors
/// The server would not list its folders.
pub async fn places(session: &mut ImapSession) -> Result<Places, String> {
    Ok(Places::from_list(&list_folders(session).await?))
}

/// Every folder the server holds, with the special-use marks it gave each.
///
/// # Errors
/// The server would not list its folders.
pub async fn list_folders(session: &mut ImapSession) -> Result<Vec<(String, Vec<String>)>, String> {
    use async_imap::types::NameAttribute;
    let stream = session
        .list(Some(""), Some("*"))
        .await
        .map_err(|e| e.to_string())?;
    stream
        .map_ok(|listed| {
            let uses = listed
                .attributes()
                .iter()
                .filter_map(|attribute| match attribute {
                    NameAttribute::All => Some("All"),
                    NameAttribute::Archive => Some("Archive"),
                    NameAttribute::Drafts => Some("Drafts"),
                    NameAttribute::Junk => Some("Junk"),
                    NameAttribute::Sent => Some("Sent"),
                    NameAttribute::Trash => Some("Trash"),
                    _ => None,
                })
                .map(str::to_owned)
                .collect();
            (listed.name().to_owned(), uses)
        })
        .try_collect()
        .await
        .map_err(|e| e.to_string())
}

/// One message as its server sent it: where it is, its flags, when the server says it
/// arrived, in minutes since the epoch, and its whole text.
#[derive(Clone, Debug)]
pub struct Raw {
    pub uid: u32,
    pub read: bool,
    pub starred: bool,
    pub minutes: Option<i64>,
    pub body: Vec<u8>,
}

/// What a page of a folder asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    /// The newest page, from the start.
    Newest,
    /// What arrived after `newest`, and the flags of `oldest..=newest` (what another client
    /// read, starred or removed), while the folder is still numbered `uid_validity` and no
    /// more than `catch_up` arrived; the newest page otherwise.
    Since {
        uid_validity: u32,
        newest: u32,
        oldest: u32,
        catch_up: u32,
    },
    /// The page before `uid`, while the folder is still numbered `uid_validity`; the newest
    /// page otherwise. The message itself may be gone; its number still says where the reader
    /// got to.
    Before { uid_validity: u32, uid: u32 },
}

/// What a page of a folder brought back.
#[derive(Clone, Debug, Default)]
pub struct Read {
    pub uid_validity: u32,
    /// The newest UID the folder has given out.
    pub newest_uid: u32,
    /// Whether what the reader held of the folder is replaced: a newest page.
    pub reset: bool,
    /// Whether the folder holds messages before the oldest one of this page, where it says.
    pub has_older: Option<bool>,
    pub messages: Vec<Raw>,
    pub flags: Vec<MailFlag>,
    /// The UIDs `flags` speaks for, both ends included.
    pub window: Option<(u32, u32)>,
}

/// The items a read asks the server for: the message whole, its position and its flags.
const FETCH_ITEMS: &str = "(UID FLAGS INTERNALDATE BODY.PEEK[])";

/// Read `page` of `mailbox`; with `starred`, the newest page of its flagged messages.
///
/// # Errors
/// The server would not select the folder or answer what was asked of it.
pub async fn read_page(
    session: &mut ImapSession,
    mailbox: &str,
    starred: bool,
    page: Page,
) -> Result<Read, String> {
    let selected = session.select(mailbox).await.map_err(|e| e.to_string())?;
    let uid_validity = selected.uid_validity.unwrap_or(0);
    let exists = selected.exists;
    let mut read = Read {
        uid_validity,
        newest_uid: selected.uid_next.unwrap_or(0).saturating_sub(1),
        ..Read::default()
    };
    if starred {
        read.reset = true;
        let newest = newest_of(
            session
                .uid_search("FLAGGED")
                .await
                .map_err(|e| e.to_string())?,
            PAGE as usize,
        );
        read.messages = fetch_uids(session, &newest).await?;
        return Ok(read);
    }
    if exists == 0 {
        read.reset = true;
        read.has_older = Some(false);
        return Ok(read);
    }
    match page {
        Page::Before {
            uid_validity: known,
            uid: before,
        } if known == uid_validity => {
            let older: Vec<u32> = match before.checked_sub(1).filter(|last| *last > 0) {
                Some(last) => session
                    .uid_search(format!("UID 1:{last}"))
                    .await
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .filter(|uid| *uid < before)
                    .collect(),
                None => Vec::new(),
            };
            read.has_older = Some(older.len() > PAGE as usize);
            let page = newest_of(older, PAGE as usize);
            read.messages = fetch_uids(session, &page).await?;
        }
        Page::Since {
            uid_validity: known,
            newest,
            oldest,
            catch_up,
        } if known == uid_validity && read.newest_uid.saturating_sub(newest) <= catch_up => {
            // `n:*` always answers with the last message, even an older one: keep only the new.
            let range = format!("{}:*", newest.saturating_add(1));
            read.messages = fetch_set(session, &range)
                .await?
                .into_iter()
                .filter(|message| message.uid > newest)
                .collect();
            read.flags = fetch_flags(session, oldest, newest).await?;
            read.window = Some((oldest, newest));
        }
        _ => {
            // Nothing held, the folder renumbered, or too much missed: the newest page.
            read.reset = true;
            let first = exists.saturating_sub(PAGE - 1).max(1);
            read.messages = fetch_seq(session, first, exists).await?;
            read.has_older = Some(first > 1);
        }
    }
    Ok(read)
}

/// The newest `most` of `uids`, newest first.
fn newest_of(uids: impl IntoIterator<Item = u32>, most: usize) -> Vec<u32> {
    let mut uids: Vec<u32> = uids.into_iter().collect();
    uids.sort_unstable();
    uids.into_iter().rev().take(most).collect()
}

/// The messages at positions `first..=last`.
///
/// # Errors
/// The server did not answer the fetch.
pub async fn fetch_seq(
    session: &mut ImapSession,
    first: u32,
    last: u32,
) -> Result<Vec<Raw>, String> {
    let stream = session
        .fetch(format!("{first}:{last}"), FETCH_ITEMS)
        .await
        .map_err(|e| e.to_string())?;
    raws(stream).await
}

/// The messages the UID set `uid_set` names.
///
/// # Errors
/// The server did not answer the fetch.
pub async fn fetch_set(session: &mut ImapSession, uid_set: &str) -> Result<Vec<Raw>, String> {
    let stream = session
        .uid_fetch(uid_set, FETCH_ITEMS)
        .await
        .map_err(|e| e.to_string())?;
    raws(stream).await
}

/// The messages with these UIDs.
///
/// # Errors
/// The server did not answer the fetch.
pub async fn fetch_uids(session: &mut ImapSession, uids: &[u32]) -> Result<Vec<Raw>, String> {
    if uids.is_empty() {
        return Ok(Vec::new());
    }
    let set = uids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    fetch_set(session, &set).await
}

async fn raws(
    stream: impl futures_util::Stream<Item = async_imap::error::Result<async_imap::types::Fetch>>,
) -> Result<Vec<Raw>, String> {
    let fetches: Vec<async_imap::types::Fetch> =
        stream.try_collect().await.map_err(|e| e.to_string())?;
    Ok(fetches
        .iter()
        .filter_map(|fetch| {
            Some(Raw {
                uid: fetch.uid?,
                read: seen(fetch),
                starred: flagged(fetch),
                minutes: fetch.internal_date().map(|date| date.timestamp() / 60),
                body: fetch.body()?.to_vec(),
            })
        })
        .collect())
}

async fn fetch_flags(
    session: &mut ImapSession,
    oldest: u32,
    newest: u32,
) -> Result<Vec<MailFlag>, String> {
    if oldest > newest {
        return Ok(Vec::new());
    }
    let stream = session
        .uid_fetch(format!("{oldest}:{newest}"), "(UID FLAGS)")
        .await
        .map_err(|e| e.to_string())?;
    let fetches: Vec<async_imap::types::Fetch> =
        stream.try_collect().await.map_err(|e| e.to_string())?;
    Ok(fetches
        .iter()
        .filter_map(|fetch| {
            let uid = fetch.uid.filter(|uid| (oldest..=newest).contains(uid))?;
            Some(MailFlag {
                uid,
                read: seen(fetch),
                starred: flagged(fetch),
            })
        })
        .collect())
}

fn seen(fetch: &async_imap::types::Fetch) -> bool {
    fetch
        .flags()
        .any(|flag| matches!(flag, async_imap::types::Flag::Seen))
}

fn flagged(fetch: &async_imap::types::Fetch) -> bool {
    fetch
        .flags()
        .any(|flag| matches!(flag, async_imap::types::Flag::Flagged))
}

/// `text` as a quoted string a server is sent, when it can be one: plain letters without a
/// control character, trimmed.
#[must_use]
pub fn quoted(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() || !text.chars().all(|c| c.is_ascii() && !c.is_ascii_control()) {
        return None;
    }
    let mut quoted = String::with_capacity(text.len().saturating_add(2));
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

/// The newest `most` messages of `mailbox` the search `keys` finds, newest first, and the
/// numbering of the folder they are in.
///
/// # Errors
/// The server would not select the folder or answer the search.
pub async fn search_folder(
    session: &mut ImapSession,
    mailbox: &str,
    keys: &str,
    most: usize,
) -> Result<(u32, Vec<Raw>), String> {
    let selected = session.select(mailbox).await.map_err(|e| e.to_string())?;
    let uid_validity = selected.uid_validity.unwrap_or(0);
    let found = session.uid_search(keys).await.map_err(|e| e.to_string())?;
    let mut messages = fetch_uids(session, &newest_of(found, most)).await?;
    messages.sort_by_key(|message| std::cmp::Reverse(message.uid));
    Ok((uid_validity, messages))
}

/// Tell the server what changed about the message `marking` names: found where it was read
/// while its folder keeps its numbering, by its `Message-ID` otherwise.
///
/// # Errors
/// The server holds no such folder or message, or refused the change.
pub async fn mark(
    session: &mut ImapSession,
    places: &Places,
    marking: &MarkMail,
) -> Result<(), String> {
    let mailbox = places
        .listed(&marking.folder)
        .ok_or_else(|| format!("this account's server has no {} folder", marking.folder))?;
    let selected = session.select(mailbox).await.map_err(|e| e.to_string())?;
    let in_place = marking.uid != 0 && selected.uid_validity.unwrap_or(0) == marking.uid_validity;
    let uid = if in_place {
        marking.uid
    } else if let Some(search) = marking.message_id.as_deref().and_then(message_id_query) {
        let found = session
            .uid_search(search)
            .await
            .map_err(|e| e.to_string())?;
        match found.into_iter().max() {
            Some(uid) => uid,
            // A copy to delete that is gone already is deleted.
            None if marking.delete => return Ok(()),
            None => return Err("the message is no longer on its server".into()),
        }
    } else {
        return Err("the folder changed; its messages are read again".into());
    };
    let uid = uid.to_string();
    for (flag, set) in [("\\Seen", marking.read), ("\\Flagged", marking.starred)] {
        let Some(set) = set else {
            continue;
        };
        let flags = format!("{}FLAGS.SILENT ({flag})", if set { "+" } else { "-" });
        let stored = session
            .uid_store(&uid, flags)
            .await
            .map_err(|e| e.to_string())?;
        let _: Vec<_> = stored.try_collect().await.map_err(|e| e.to_string())?;
    }
    if let Some(to) = &marking.to_folder {
        let destination = places
            .listed(to)
            .ok_or_else(|| format!("this account's server has no {to} folder"))?;
        session
            .uid_mv(&uid, destination)
            .await
            .map_err(|e| e.to_string())?;
    }
    if marking.delete {
        let deleted = session
            .uid_store(&uid, "+FLAGS.SILENT (\\Deleted)")
            .await
            .map_err(|e| e.to_string())?;
        let _: Vec<_> = deleted.try_collect().await.map_err(|e| e.to_string())?;
        let expunged = session
            .uid_expunge(&uid)
            .await
            .map_err(|e| format!("server cannot safely expunge this message: {e}"))?;
        let _: Vec<_> = expunged.try_collect().await.map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The search that finds a message by its `Message-ID`, written with or without its angle
/// brackets, when it is one a server can be asked for.
pub(crate) fn message_id_query(id: &str) -> Option<String> {
    let id = id.trim().trim_start_matches('<').trim_end_matches('>');
    let askable = !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_graphic() && c != '"' && c != '\\');
    askable.then(|| format!("HEADER MESSAGE-ID \"{id}\""))
}

/// The folder `folder` is kept in, made under its usual name where the server holds none:
/// its name, and whether it was made now.
async fn kept_in(
    session: &mut ImapSession,
    places: &Places,
    folder: &str,
    what: &str,
) -> Result<(String, bool), String> {
    if let Some(listed) = places.listed(folder) {
        return Ok((listed.to_owned(), false));
    }
    let made = places.name(folder).unwrap_or(folder).to_owned();
    session
        .create(&made)
        .await
        .map_err(|e| format!("the server keeps no {what} folder and would not make one: {e}"))?;
    Ok((made, true))
}

/// Keep the draft `mime` holds in the account's Drafts folder, read and marked as a draft,
/// making the folder where a server holds none. Whether it was made.
///
/// # Errors
/// The server would not make the folder or keep the draft.
pub async fn keep_draft(
    session: &mut ImapSession,
    places: &Places,
    mime: &[u8],
) -> Result<bool, String> {
    let (folder, made) = kept_in(session, places, "drafts", "Drafts").await?;
    session
        .append(&folder, Some("(\\Seen \\Draft)"), None, mime)
        .await
        .map_err(|e| format!("the draft was not kept in {folder}: {e}"))?;
    Ok(made)
}

/// Keep a copy of a message that left in the account's Sent folder, read, as its writer has
/// read it, making the folder where a plain server holds none. A server that files what it
/// sends is left to it, and a folder that already holds the message is not given it twice.
/// Whether the folder was made.
///
/// # Errors
/// The server would not make the folder or take the copy.
pub async fn file_sent(
    session: &mut ImapSession,
    places: &Places,
    hosts: (&str, &str),
    mime: &[u8],
) -> Result<bool, String> {
    let gmail = session
        .capabilities()
        .await
        .map_err(|e| e.to_string())?
        .has_str(accounts::email::GMAIL_CAPABILITY);
    if accounts::email::servers_file_sent(hosts.0, hosts.1, gmail) {
        return Ok(false);
    }
    if let Some(folder) = places.listed("sent")
        && let Some(held) = message_id_search(mime)
    {
        session.select(folder).await.map_err(|e| e.to_string())?;
        let found = session.uid_search(held).await.map_err(|e| e.to_string())?;
        if !found.is_empty() {
            return Ok(false);
        }
    }
    let (folder, made) = kept_in(session, places, "sent", "Sent").await?;
    session
        .append(&folder, Some("(\\Seen)"), None, mime)
        .await
        .map_err(|e| format!("{folder} would not take the copy: {e}"))?;
    Ok(made)
}

/// The search that finds a message by the `Message-ID` its `mime` carries, when it carries one
/// a server can be asked for.
fn message_id_search(mime: &[u8]) -> Option<String> {
    let parsed = mail_parser::MessageParser::default().parse(mime)?;
    message_id_query(parsed.message_id()?)
}
