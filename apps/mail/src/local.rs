//! Messages the mailbox document holds itself: mail delivered without a
//! mail server (a seeded scenario, an import, an injected event) and drafts
//! written through the mailbox's actions.
//!
//! A server's corpus stays outside the document (see [`crate::doc`]); these
//! messages are document content, so they replicate, replay and undo like
//! every other mailbox edit, and every participant — a person's window, an
//! agent's headless replica, a grader — reads the same mail. Each is stored
//! whole under `local/<id>`; its folder, read and starred state and labels
//! are the ordinary per-message flags, so every message action (archive,
//! star, label, move to Trash) applies to it unchanged.
//!
//! What is read from them — the mailbox `read`, `search-messages` and
//! `export-content` — is projected here from the document alone: nothing
//! reaches a network or an account.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::model::{Addr, Folder, Message, normalize_subject};
use crate::transport::parse_addr_list;

/// Bounds on one stored message, applied wherever one is authored or read
/// back, so a hostile payload cannot grow the document without limit.
pub(crate) const MAX_ADDRESS_CHARS: usize = 8 * 1024;
pub(crate) const MAX_SUBJECT_CHARS: usize = 998;
pub(crate) const MAX_BODY_CHARS: usize = 256 * 1024;
const MAX_ID_CHARS: usize = 256;

/// How much of a body a message listing shows.
const SNIPPET_CHARS: usize = 160;

/// The most messages one listing answers with, newest first.
const MOST_LISTED: usize = 200;

/// One message stored whole in the mailbox document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct LocalMessage {
    /// The mailbox's id for it (a UUID), as `mail.message:<id>` names it.
    pub id: String,
    /// Its RFC 5322 `Message-ID`, without angle brackets.
    #[serde(default)]
    pub message_id: String,
    /// The sender, `Name <address>` or a bare address.
    pub from: String,
    /// Recipients, comma-separated.
    #[serde(default)]
    pub to: String,
    #[serde(default)]
    pub cc: String,
    pub subject: String,
    /// The message as plain text.
    pub body: String,
    /// When it was sent, RFC 3339 in UTC.
    #[serde(default)]
    pub date: String,
    /// The same instant, in minutes since the Unix epoch.
    #[serde(default)]
    pub timestamp_min: i64,
    /// The `Message-ID` of the message it answers.
    #[serde(default)]
    pub in_reply_to: Option<String>,
    /// The conversation it belongs to: the `Message-ID` of the
    /// conversation's first message, or a key the author gave.
    #[serde(default)]
    pub thread: Option<String>,
}

/// Take at most `max` characters of `value`, trimmed, without control
/// characters (bodies keep their line breaks and tabs).
pub(crate) fn bounded(value: &str, max: usize, multiline: bool) -> String {
    value
        .trim()
        .chars()
        .filter(|ch| !ch.is_control() || (multiline && matches!(ch, '\n' | '\r' | '\t')))
        .take(max)
        .collect()
}

/// A `Message-ID` as stored: no angle brackets, no spaces.
pub(crate) fn bare_message_id(value: &str) -> String {
    bounded(value, MAX_ID_CHARS, false)
        .trim_start_matches('<')
        .trim_end_matches('>')
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect()
}

impl LocalMessage {
    /// This message within the document's bounds.
    #[must_use]
    pub fn bounded(&self) -> Self {
        Self {
            id: bounded(&self.id, MAX_ID_CHARS, false),
            message_id: bare_message_id(&self.message_id),
            from: bounded(&self.from, MAX_ADDRESS_CHARS, false),
            to: bounded(&self.to, MAX_ADDRESS_CHARS, false),
            cc: bounded(&self.cc, MAX_ADDRESS_CHARS, false),
            subject: bounded(&self.subject, MAX_SUBJECT_CHARS, false),
            body: bounded(&self.body, MAX_BODY_CHARS, true),
            date: bounded(&self.date, 64, false),
            timestamp_min: self.timestamp_min,
            in_reply_to: self
                .in_reply_to
                .as_deref()
                .map(bare_message_id)
                .filter(|id| !id.is_empty()),
            thread: self
                .thread
                .as_deref()
                .map(|thread| bounded(thread, MAX_ID_CHARS, false))
                .filter(|thread| !thread.is_empty()),
        }
    }

    /// Whether it can be stored: a UUID id and a `Message-ID`.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.id.parse::<uuid::Uuid>().is_ok() && !self.message_id.is_empty()
    }

    /// The conversation it belongs to: its own `Message-ID` when it starts
    /// one.
    #[must_use]
    pub fn thread_key(&self) -> &str {
        self.thread.as_deref().unwrap_or(&self.message_id)
    }

    /// The first non-empty line of the body, at most [`SNIPPET_CHARS`].
    #[must_use]
    pub fn snippet(&self) -> String {
        let line = self
            .body
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with('>'))
            .unwrap_or("");
        let words = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if words.chars().count() > SNIPPET_CHARS {
            let cut: String = words.chars().take(SNIPPET_CHARS).collect();
            format!("{cut}…")
        } else {
            words
        }
    }

    /// The message as the window's corpus holds one, under `folder` with
    /// the flags the document gives it.
    #[must_use]
    pub fn to_message(&self, flags: &Flags) -> Option<Message> {
        let id = self.id.parse().ok()?;
        let mut from = parse_addr_list(&self.from);
        Some(Message {
            id,
            rfc_message_id: Some(self.message_id.clone()),
            in_reply_to: self.in_reply_to.clone(),
            from: if from.is_empty() {
                Addr::bare("unknown@unknown")
            } else {
                from.swap_remove(0)
            },
            to: parse_addr_list(&self.to),
            cc: parse_addr_list(&self.cc),
            subject: self.subject.clone(),
            body: self.body.clone(),
            html_body: None,
            attachments: Vec::new(),
            timestamp_min: self.timestamp_min,
            read: flags.read,
            starred: flags.starred,
            folder: flags.folder,
            labels: flags.labels.clone(),
        })
    }
}

/// A stored message's flags as the document holds them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flags {
    pub folder: Folder,
    pub read: bool,
    pub starred: bool,
    pub labels: Vec<String>,
}

impl Default for Flags {
    fn default() -> Self {
        Self {
            folder: Folder::Inbox,
            read: false,
            starred: false,
            labels: Vec::new(),
        }
    }
}

/// One stored message with its flags: what the mailbox's reads answer from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    pub message: LocalMessage,
    pub flags: Flags,
}

/// A date as an author writes one: RFC 3339 (`2026-10-11T09:30:00Z`,
/// `2026-10-11 09:30`, `2026-10-11`) or RFC 2822 (`Sun, 11 Oct 2026
/// 09:30:00 +0000`). Answers seconds since the Unix epoch.
#[must_use]
pub fn parse_date(text: &str) -> Option<i64> {
    use mail_parser::DateTime;
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let mut candidates = vec![text.to_owned()];
    let spaced = text.replacen(' ', "T", 1);
    if text.len() == 10 {
        candidates.push(format!("{text}T00:00:00Z"));
    }
    for base in [text, spaced.as_str()] {
        candidates.push(base.to_owned());
        candidates.push(format!("{base}Z"));
        candidates.push(format!("{base}:00Z"));
    }
    candidates
        .iter()
        .find_map(|candidate| DateTime::parse_rfc3339(candidate).filter(DateTime::is_valid))
        .or_else(|| DateTime::parse_rfc822(text).filter(DateTime::is_valid))
        .map(|date| date.to_timestamp())
}

/// `seconds` since the Unix epoch, RFC 3339 in UTC.
#[must_use]
pub fn format_date(seconds: i64) -> String {
    mail_parser::DateTime::from_timestamp(seconds).to_rfc3339()
}

/// A `Message-ID` for a message authored here with no id of its own,
/// derived from what it says so the same seed always names the same
/// message.
#[must_use]
pub fn derived_message_id(parts: &[&str]) -> String {
    fn fnv1a(bytes: &[u8], key: u64) -> u64 {
        let mut hasher = fnv::FnvHasher::with_key(key);
        std::hash::Hasher::write(&mut hasher, bytes);
        std::hash::Hasher::finish(&hasher)
    }
    let joined = parts.join("\u{1f}");
    format!(
        "{:016x}{:016x}@mail.tilo.local",
        fnv1a(joined.as_bytes(), 0xcbf2_9ce4_8422_2325),
        fnv1a(joined.as_bytes(), 0x0102_0304_0506_0708)
    )
}

/// `subject` as a reply's: `Re: ` before it, once.
#[must_use]
pub fn reply_subject(subject: &str) -> String {
    let trimmed = subject.trim();
    if trimmed
        .get(..3)
        .is_some_and(|head| head.eq_ignore_ascii_case("re:"))
    {
        trimmed.to_owned()
    } else if trimmed.is_empty() {
        "Re: (no subject)".to_owned()
    } else {
        format!("Re: {trimmed}")
    }
}

/// `body` above the quoted `original`, as a reply is written.
#[must_use]
pub fn reply_body(body: &str, original: &LocalMessage) -> String {
    let quoted: Vec<String> = original
        .body
        .lines()
        .map(|line| {
            if line.is_empty() {
                ">".to_owned()
            } else {
                format!("> {line}")
            }
        })
        .collect();
    format!(
        "{}\n\nOn {}, {} wrote:\n{}",
        body.trim_end(),
        original.date,
        original.from,
        quoted.join("\n")
    )
}

/// The addresses of `list` other than `except`'s, comma-separated.
fn addresses_without(lists: &[&str], except: &[String]) -> String {
    let mut seen: Vec<String> = Vec::new();
    let mut kept = Vec::new();
    for list in lists {
        for addr in parse_addr_list(list) {
            let email = addr.email.to_ascii_lowercase();
            if email.is_empty() || except.contains(&email) || seen.contains(&email) {
                continue;
            }
            seen.push(email);
            kept.push(addr.rfc5322());
        }
    }
    kept.join(", ")
}

/// Who a reply to `original` goes to: its sender, and with `all` everyone
/// else it went to but `me`.
#[must_use]
pub fn reply_recipients(original: &LocalMessage, all: bool, me: &str) -> (String, String) {
    let me: Vec<String> = parse_addr_list(me)
        .into_iter()
        .map(|addr| addr.email.to_ascii_lowercase())
        .collect();
    let to = addresses_without(&[&original.from], &[]);
    if !all {
        return (to, String::new());
    }
    let mut except = me;
    except.extend(
        parse_addr_list(&original.from)
            .into_iter()
            .map(|addr| addr.email.to_ascii_lowercase()),
    );
    let cc = addresses_without(&[&original.to, &original.cc], &except);
    (to, cc)
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

/// `stored` newest first.
fn newest_first(stored: &[Stored]) -> Vec<&Stored> {
    let mut sorted: Vec<&Stored> = stored.iter().collect();
    sorted.sort_by(|a, b| {
        b.message
            .timestamp_min
            .cmp(&a.message.timestamp_min)
            .then_with(|| a.message.id.cmp(&b.message.id))
    });
    sorted
}

/// Set `key` of the object `value` to `field`.
fn put(value: &mut serde_json::Value, key: &str, field: impl Into<serde_json::Value>) {
    if let Some(object) = value.as_object_mut() {
        object.insert(key.to_owned(), field.into());
    }
}

/// One message as a listing shows it.
#[must_use]
pub fn summary(stored: &Stored) -> serde_json::Value {
    let message = &stored.message;
    let mut value = serde_json::json!({
        "item": format!("mail.message:{}", message.id),
        "folder": stored.flags.folder.as_key(),
        "from": message.from,
        "to": message.to,
        "subject": message.subject,
        "date": message.date,
        "snippet": message.snippet(),
        "read": stored.flags.read,
        "thread": format!("mail.thread:{}", message.id),
    });
    if stored.flags.starred {
        put(&mut value, "starred", true);
    }
    if !stored.flags.labels.is_empty() {
        put(&mut value, "labels", stored.flags.labels.clone());
    }
    if let Some(parent) = &message.in_reply_to {
        put(&mut value, "in_reply_to", parent.clone());
    }
    value
}

/// One message whole.
#[must_use]
pub fn full(stored: &Stored) -> serde_json::Value {
    let message = &stored.message;
    let mut value = summary(stored);
    put(&mut value, "cc", message.cc.clone());
    put(&mut value, "body", message.body.clone());
    put(&mut value, "message_id", message.message_id.clone());
    if let Some(object) = value.as_object_mut() {
        object.remove("snippet");
    }
    value
}

/// The messages of the conversation `message` is in, oldest first.
#[must_use]
pub fn thread_of<'a>(stored: &'a [Stored], message: &LocalMessage) -> Vec<&'a Stored> {
    let key = message.thread_key();
    let mut thread: Vec<&Stored> = stored
        .iter()
        .filter(|other| {
            other.message.thread_key() == key
                || other.message.message_id == key
                || other.message.in_reply_to.as_deref() == Some(message.message_id.as_str())
                || message.in_reply_to.as_deref() == Some(other.message.message_id.as_str())
        })
        .collect();
    thread.sort_by(|a, b| {
        a.message
            .timestamp_min
            .cmp(&b.message.timestamp_min)
            .then_with(|| a.message.id.cmp(&b.message.id))
    });
    thread
}

/// The stored message `item` names (`mail.message:<id>`, `mail.thread:<id>`
/// or a bare id).
fn named<'a>(stored: &'a [Stored], item: &str) -> Option<&'a Stored> {
    let id = item
        .strip_prefix("mail.message:")
        .or_else(|| item.strip_prefix("mail.thread:"))
        .unwrap_or(item)
        .trim();
    stored.iter().find(|other| other.message.id == id)
}

/// What the mailbox's `read` answers: with no `item`, its folders and the
/// messages it holds, newest first; with `mail.folder:<key>` that folder's;
/// with `mail.message:<id>` that message whole and its conversation; with
/// `mail.thread:<id>` every message of the conversation whole, oldest first.
///
/// # Errors
/// Names an item the mailbox does not hold.
pub fn read(
    stored: &[Stored],
    other_messages: usize,
    has_draft: bool,
    item: Option<&str>,
) -> Result<serde_json::Value, String> {
    let item = item.map(str::trim).filter(|item| !item.is_empty());
    let Some(item) = item else {
        let folders: Vec<serde_json::Value> = Folder::ALL
            .iter()
            .filter(|folder| **folder != Folder::Starred)
            .map(|folder| {
                let held = stored.iter().filter(|s| s.flags.folder == *folder);
                let (total, unread) = held.fold((0, 0), |(total, unread), s| {
                    (total + 1, unread + usize::from(!s.flags.read))
                });
                serde_json::json!({
                    "item": format!("mail.folder:{}", folder.as_key()),
                    "title": folder.title(),
                    "messages": total,
                    "unread": unread,
                })
            })
            .collect();
        let listed: Vec<serde_json::Value> = newest_first(stored)
            .into_iter()
            .take(MOST_LISTED)
            .map(summary)
            .collect();
        let mut value = serde_json::json!({
            "kind": "mail.mailbox",
            "folders": folders,
            "messages": listed,
        });
        if stored.len() > MOST_LISTED {
            put(&mut value, "more", stored.len() - MOST_LISTED);
        }
        if other_messages > 0 {
            put(&mut value, "server_messages", other_messages);
        }
        if has_draft {
            put(&mut value, "composer_draft", "mail.draft:current");
        }
        return Ok(value);
    };
    if let Some(key) = item.strip_prefix("mail.folder:") {
        let folder = Folder::from_key(key)
            .ok_or_else(|| format!("no folder `{key}`; read the mailbox for its folders"))?;
        let held: Vec<Stored> = stored
            .iter()
            .filter(|s| folder == Folder::Starred && s.flags.starred || s.flags.folder == folder)
            .cloned()
            .collect();
        return Ok(serde_json::json!({
            "kind": "mail.folder",
            "item": item,
            "title": folder.title(),
            "messages": newest_first(&held).into_iter().take(MOST_LISTED).map(summary).collect::<Vec<_>>(),
        }));
    }
    let found = named(stored, item).ok_or_else(|| {
        format!("no message `{item}`; read the mailbox for the messages it holds")
    })?;
    let thread = thread_of(stored, &found.message);
    if item.starts_with("mail.thread:") {
        return Ok(serde_json::json!({
            "kind": "mail.thread",
            "item": item,
            "subject": normalize_subject(&found.message.subject),
            "messages": thread.into_iter().map(full).collect::<Vec<_>>(),
        }));
    }
    let mut value = full(found);
    put(&mut value, "kind", "mail.message");
    if thread.len() > 1 {
        put(
            &mut value,
            "conversation",
            thread
                .into_iter()
                .map(|other| {
                    serde_json::json!({
                        "item": format!("mail.message:{}", other.message.id),
                        "from": other.message.from,
                        "subject": other.message.subject,
                        "date": other.message.date,
                        "folder": other.flags.folder.as_key(),
                    })
                })
                .collect::<Vec<_>>(),
        );
    }
    Ok(value)
}

/// The messages whose sender, recipients, subject or body hold every word
/// of `query` (any case), newest first, in `folder` when one is named.
///
/// # Errors
/// Names a folder that does not exist.
pub fn search(
    stored: &[Stored],
    query: &str,
    folder: Option<&str>,
) -> Result<serde_json::Value, String> {
    let folder = match folder.map(str::trim).filter(|f| !f.is_empty()) {
        Some(key) => Some(
            Folder::from_key(key.strip_prefix("mail.folder:").unwrap_or(key))
                .ok_or_else(|| format!("no folder `{key}`"))?,
        ),
        None => None,
    };
    let words: Vec<String> = query
        .split_whitespace()
        .map(str::to_lowercase)
        .filter(|word| !word.is_empty())
        .collect();
    let matches: Vec<serde_json::Value> = newest_first(stored)
        .into_iter()
        .filter(|s| folder.is_none_or(|folder| s.flags.folder == folder))
        .filter(|s| {
            let m = &s.message;
            let text =
                format!("{}\n{}\n{}\n{}\n{}", m.from, m.to, m.cc, m.subject, m.body).to_lowercase();
            words.iter().all(|word| text.contains(word))
        })
        .take(MOST_LISTED)
        .map(summary)
        .collect();
    Ok(serde_json::json!({
        "query": query,
        "matches": matches,
    }))
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

/// What an export holds: one message, its conversation, a folder or the
/// whole mailbox, oldest first.
///
/// # Errors
/// Names a message or folder the mailbox does not hold.
pub fn export_scope<'a>(
    stored: &'a [Stored],
    folder: Option<&str>,
    message: Option<&str>,
    thread: Option<&str>,
) -> Result<Vec<&'a Stored>, String> {
    fn given(value: Option<&str>) -> Option<&str> {
        value.map(str::trim).filter(|value| !value.is_empty())
    }
    let mut chosen: Vec<&Stored> = if let Some(item) = given(message) {
        vec![named(stored, item).ok_or_else(|| format!("no message `{item}`"))?]
    } else if let Some(item) = given(thread) {
        let found = named(stored, item).ok_or_else(|| format!("no message `{item}`"))?;
        thread_of(stored, &found.message)
    } else if let Some(key) = given(folder) {
        let folder = Folder::from_key(key.strip_prefix("mail.folder:").unwrap_or(key))
            .ok_or_else(|| format!("no folder `{key}`"))?;
        stored
            .iter()
            .filter(|s| folder == Folder::Starred && s.flags.starred || s.flags.folder == folder)
            .collect()
    } else {
        stored.iter().collect()
    };
    chosen.sort_by(|a, b| {
        a.message
            .timestamp_min
            .cmp(&b.message.timestamp_min)
            .then_with(|| a.message.id.cmp(&b.message.id))
    });
    Ok(chosen)
}

/// `stored` as an RFC 5322 message, CRLF line ends.
///
/// # Errors
/// Why it could not be written.
pub fn eml(stored: &Stored) -> Result<Vec<u8>, String> {
    use mail_builder::MessageBuilder;
    use mail_builder::headers::address::Address;
    use mail_builder::headers::text::Text;

    fn addresses(list: &str) -> Option<Address<'static>> {
        let addrs: Vec<Address<'static>> = parse_addr_list(list)
            .into_iter()
            .map(|addr| Address::new_address(addr.name, addr.email))
            .collect();
        (!addrs.is_empty()).then(|| Address::new_list(addrs))
    }
    let message = &stored.message;
    let mut builder = MessageBuilder::new()
        .message_id(message.message_id.clone())
        .subject(message.subject.clone())
        .date(message.timestamp_min * 60)
        .header("X-Tilo-Folder", Text::new(stored.flags.folder.as_key()))
        .text_body(message.body.clone());
    if let Some(from) = addresses(&message.from) {
        builder = builder.from(from);
    }
    if let Some(to) = addresses(&message.to) {
        builder = builder.to(to);
    }
    if let Some(cc) = addresses(&message.cc) {
        builder = builder.cc(cc);
    }
    if let Some(parent) = &message.in_reply_to {
        builder = builder.in_reply_to(parent.clone());
        let root = message.thread_key();
        builder = builder.references(if root == parent || root == message.message_id {
            vec![parent.clone()]
        } else {
            vec![root.to_owned(), parent.clone()]
        });
    }
    builder.write_to_vec().map_err(|error| error.to_string())
}

/// `messages` as an mbox (mboxrd): each message after its `From ` line,
/// `From ` lines in bodies quoted.
///
/// # Errors
/// Why a message could not be written.
pub fn mbox(messages: &[&Stored]) -> Result<Vec<u8>, String> {
    let mut out = String::new();
    for stored in messages {
        let raw = String::from_utf8(eml(stored)?).map_err(|error| error.to_string())?;
        let sender = parse_addr_list(&stored.message.from)
            .into_iter()
            .next()
            .map_or_else(|| "MAILER-DAEMON".to_owned(), |addr| addr.email);
        let date = mail_parser::DateTime::from_timestamp(stored.message.timestamp_min * 60);
        let _ = writeln!(
            out,
            "From {sender} {:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            date.year, date.month, date.day, date.hour, date.minute, date.second
        );
        for line in raw.replace("\r\n", "\n").lines() {
            if line.trim_start_matches('>').starts_with("From ") {
                out.push('>');
            }
            out.push_str(line);
            out.push('\n');
        }
        out.push('\n');
    }
    Ok(out.into_bytes())
}

/// `messages` as Markdown: a section per message with its headers, then its
/// body.
#[must_use]
pub fn markdown(title: &str, messages: &[&Stored]) -> String {
    let mut out = format!("# {title}\n");
    if messages.is_empty() {
        out.push_str("\nNo messages.\n");
    }
    for stored in messages {
        let m = &stored.message;
        let subject = if m.subject.is_empty() {
            "(no subject)"
        } else {
            m.subject.as_str()
        };
        let _ = writeln!(out, "\n## {subject}\n");
        let _ = writeln!(out, "- Folder: {}", stored.flags.folder.title());
        let _ = writeln!(out, "- From: {}", m.from);
        if !m.to.is_empty() {
            let _ = writeln!(out, "- To: {}", m.to);
        }
        if !m.cc.is_empty() {
            let _ = writeln!(out, "- Cc: {}", m.cc);
        }
        let _ = writeln!(out, "- Date: {}", m.date);
        let _ = writeln!(out, "- Message-ID: <{}>", m.message_id);
        if let Some(parent) = &m.in_reply_to {
            let _ = writeln!(out, "- In-Reply-To: <{parent}>");
        }
        let _ = writeln!(out, "- Item: mail.message:{}\n", m.id);
        out.push_str(m.body.trim_end());
        out.push('\n');
    }
    out
}

/// `messages` exported as `format` (`md`, `json`, `mbox` or `eml`).
///
/// # Errors
/// Names an unknown format, or an `eml` export of other than one message.
pub fn export(format: &str, title: &str, messages: &[&Stored]) -> Result<Vec<u8>, String> {
    match format
        .trim()
        .trim_start_matches('.')
        .to_ascii_lowercase()
        .as_str()
    {
        "md" | "markdown" => Ok(markdown(title, messages).into_bytes()),
        "json" => serde_json::to_vec_pretty(&messages.iter().map(|s| full(s)).collect::<Vec<_>>())
            .map_err(|error| error.to_string()),
        "mbox" => mbox(messages),
        "eml" => match messages {
            [one] => eml(one),
            _ => Err(format!(
                "an .eml holds one message and this export holds {}; name the `message`, or \
                 export mbox",
                messages.len()
            )),
        },
        other => Err(format!(
            "no export format `{other}`; choose md, json, mbox or eml"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(id: u128, subject: &str, minute: i64, folder: Folder) -> Stored {
        Stored {
            message: LocalMessage {
                id: uuid::Uuid::from_u128(id).to_string(),
                message_id: format!("m{id}@example.test"),
                from: "Ada Lovelace <ada@example.test>".into(),
                to: "me@example.test".into(),
                subject: subject.into(),
                body: format!("{subject} body\nFrom the start"),
                date: format_date(minute * 60),
                timestamp_min: minute,
                ..LocalMessage::default()
            },
            flags: Flags {
                folder,
                ..Flags::default()
            },
        }
    }

    #[test]
    fn dates_are_read_as_authors_write_them() {
        let noon = parse_date("2026-10-11T12:00:00Z").unwrap();
        assert_eq!(parse_date("2026-10-11 12:00"), Some(noon));
        assert_eq!(parse_date("2026-10-11T12:00"), Some(noon));
        assert_eq!(parse_date("Sun, 11 Oct 2026 12:00:00 +0000"), Some(noon));
        assert_eq!(parse_date("2026-10-11"), Some(noon - 12 * 3600));
        assert_eq!(parse_date("2026-10-11T14:00:00+02:00"), Some(noon));
        assert_eq!(format_date(noon), "2026-10-11T12:00:00Z");
        assert!(parse_date("next tuesday").is_none());
    }

    #[test]
    fn reads_list_newest_first_and_open_one_message_whole() {
        let messages = vec![
            stored(1, "Older", 10, Folder::Inbox),
            stored(2, "Newer", 20, Folder::Inbox),
            stored(3, "Kept", 30, Folder::Archive),
        ];
        let listing = read(&messages, 0, false, None).unwrap();
        let subjects: Vec<&str> = listing["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["subject"].as_str().unwrap())
            .collect();
        assert_eq!(subjects, ["Kept", "Newer", "Older"]);
        assert_eq!(listing["folders"][0]["unread"], 2);
        let inbox = read(&messages, 0, false, Some("mail.folder:inbox")).unwrap();
        assert_eq!(inbox["messages"].as_array().unwrap().len(), 2);
        let item = listing["messages"][1]["item"].as_str().unwrap();
        let one = read(&messages, 0, false, Some(item)).unwrap();
        assert_eq!(one["body"], "Newer body\nFrom the start");
        assert!(read(&messages, 0, false, Some("mail.message:nope")).is_err());
        let found = search(&messages, "newer BODY", None).unwrap();
        assert_eq!(found["matches"].as_array().unwrap().len(), 1);
        let none = search(&messages, "kept", Some("inbox")).unwrap();
        assert!(none["matches"].as_array().unwrap().is_empty());
    }

    #[test]
    fn exports_carry_every_message_in_each_format() {
        let messages = [
            stored(1, "Grüße", 10, Folder::Inbox),
            stored(2, "Plan", 20, Folder::Drafts),
        ];
        let all: Vec<&Stored> = messages.iter().collect();
        let md = String::from_utf8(export("md", "Mailbox", &all).unwrap()).unwrap();
        assert!(
            md.contains("## Grüße") && md.contains("- Folder: Drafts"),
            "{md}"
        );
        let mbox = String::from_utf8(export("mbox", "Mailbox", &all).unwrap()).unwrap();
        assert_eq!(
            mbox.matches("\nFrom ada@example.test ").count() + 1,
            2,
            "{mbox}"
        );
        assert!(
            mbox.contains("\n>From the start"),
            "body From lines are quoted: {mbox}"
        );
        assert!(export("eml", "Mailbox", &all).is_err());
        let eml = String::from_utf8(export("eml", "One", &all[1..]).unwrap()).unwrap();
        assert!(eml.contains("Message-ID: <m2@example.test>"), "{eml}");
        assert!(eml.contains("X-Tilo-Folder: drafts"), "{eml}");
        let parsed = mail_parser::MessageParser::default()
            .parse(eml.as_bytes())
            .unwrap();
        assert_eq!(parsed.subject(), Some("Plan"));
        let json: serde_json::Value =
            serde_json::from_slice(&export("json", "Mailbox", &all).unwrap()).unwrap();
        assert_eq!(json[0]["subject"], "Grüße");
        assert!(export("pdf", "Mailbox", &all).is_err());
    }

    #[test]
    fn a_reply_quotes_and_answers_the_sender_and_with_all_the_others() {
        let original = LocalMessage {
            from: "Ada <ada@example.test>".into(),
            to: "me@example.test, bob@example.test".into(),
            cc: "carol@example.test".into(),
            subject: "Budget".into(),
            body: "Is 40k right?\n\nAda".into(),
            date: "2026-10-11T09:00:00Z".into(),
            ..LocalMessage::default()
        };
        assert_eq!(reply_subject("Budget"), "Re: Budget");
        assert_eq!(reply_subject("RE: Budget"), "RE: Budget");
        assert_eq!(
            reply_recipients(&original, false, "me@example.test"),
            ("Ada <ada@example.test>".into(), String::new())
        );
        assert_eq!(
            reply_recipients(&original, true, "me@example.test"),
            (
                "Ada <ada@example.test>".into(),
                "bob@example.test, carol@example.test".into()
            )
        );
        let body = reply_body("Yes.", &original);
        assert!(body.starts_with("Yes.\n\nOn 2026-10-11T09:00:00Z, Ada <ada@example.test> wrote:\n> Is 40k right?\n>\n> Ada"), "{body}");
    }
}
