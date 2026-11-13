//! Pure data model for the Mail app.
//!
//! Everything here is IO-free, render-free logic. The widgets read these
//! structs; the mailbox record ([`crate::resource::Mail`]) holds the
//! *mutable* per-message state (read/unread, starred, current folder) and the
//! draft. The immutable message corpus (headers + bodies +
//! attachments) is hydrated from the server in the live app, or from the
//! bundled `sample.rs` corpus when running headless (`--shots`).

use std::cell::RefCell;
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use ui::icons::Icon;
use ui::tilo_resource::{self, JsonSchema, Model};
use uuid::Uuid;

pub use accounts::email::{AccountConfig, AuthMethod};

pub type MessageId = Uuid;

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, Model, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Folder {
    #[default]
    Inbox,
    Starred,
    Sent,
    Drafts,
    Archive,
    Trash,
    Junk,
}

impl Folder {
    pub const ALL: [Folder; 7] = [
        Folder::Inbox,
        Folder::Starred,
        Folder::Sent,
        Folder::Drafts,
        Folder::Archive,
        Folder::Junk,
        Folder::Trash,
    ];

    pub fn message(self) -> &'static str {
        match self {
            Folder::Inbox => "mail-folder-inbox",
            Folder::Starred => "mail-folder-starred",
            Folder::Sent => "mail-folder-sent",
            Folder::Drafts => "mail-folder-drafts",
            Folder::Archive => "mail-folder-archive",
            Folder::Trash => "mail-folder-trash",
            Folder::Junk => "mail-folder-junk",
        }
    }

    pub fn icon(self) -> Icon {
        match self {
            Folder::Inbox => Icon::Inbox,
            Folder::Starred => Icon::Star,
            Folder::Sent => Icon::Send,
            Folder::Drafts => Icon::Pencil,
            Folder::Archive => Icon::Archive,
            Folder::Trash => Icon::Trash,
            Folder::Junk => Icon::Ban,
        }
    }

    pub fn as_key(self) -> &'static str {
        match self {
            Folder::Inbox => "inbox",
            Folder::Starred => "starred",
            Folder::Sent => "sent",
            Folder::Drafts => "drafts",
            Folder::Archive => "archive",
            Folder::Trash => "trash",
            Folder::Junk => "junk",
        }
    }

    pub fn from_key(s: &str) -> Option<Folder> {
        Some(match s {
            "inbox" => Folder::Inbox,
            "starred" => Folder::Starred,
            "sent" => Folder::Sent,
            "drafts" => Folder::Drafts,
            "archive" => Folder::Archive,
            "trash" => Folder::Trash,
            "junk" => Folder::Junk,
            _ => return None,
        })
    }

    pub fn imap_name(self) -> &'static str {
        match self {
            Folder::Inbox | Folder::Starred => "INBOX",
            Folder::Sent => "Sent",
            Folder::Drafts => "Drafts",
            Folder::Archive => "Archive",
            Folder::Trash => "Trash",
            Folder::Junk => "Junk",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Addr {
    pub name: Option<String>,
    pub email: String,
}

impl Addr {
    pub fn new(name: impl Into<String>, email: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            name: if name.is_empty() { None } else { Some(name) },
            email: email.into(),
        }
    }

    pub fn bare(email: impl Into<String>) -> Self {
        Self {
            name: None,
            email: email.into(),
        }
    }

    pub fn display(&self) -> String {
        if let Some(n) = &self.name
            && !n.is_empty()
        {
            return n.clone();
        }
        self.email.clone()
    }

    pub fn rfc5322(&self) -> String {
        match &self.name {
            Some(n) if !n.is_empty() => {
                if n.chars().any(|ch| "()<>[]:;@\\,.\"".contains(ch)) {
                    let escaped = n.replace('\\', "\\\\").replace('"', "\\\"");
                    format!("\"{escaped}\" <{}>", self.email)
                } else {
                    format!("{n} <{}>", self.email)
                }
            }
            _ => self.email.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub filename: String,
    pub mime: String,
    pub size: u64,
    pub hydrated: bool,
    #[serde(default)]
    pub content_id: Option<String>,
    #[serde(skip, default)]
    pub content: Vec<u8>,
    #[serde(default)]
    pub kept: bool,
    #[serde(default)]
    pub inline_id: Option<String>,
}

impl Attachment {
    pub fn projected(filename: impl Into<String>, mime: impl Into<String>, size: u64) -> Self {
        Self {
            filename: filename.into(),
            mime: mime.into(),
            size,
            hydrated: false,
            content_id: None,
            content: Vec::new(),
            kept: false,
            inline_id: None,
        }
    }

    pub fn hydrated(
        filename: impl Into<String>,
        mime: impl Into<String>,
        content: Vec<u8>,
    ) -> Self {
        Self {
            filename: filename.into(),
            mime: mime.into(),
            size: content.len() as u64,
            hydrated: true,
            content_id: None,
            content,
            kept: false,
            inline_id: None,
        }
    }

    pub fn is_downloadable(&self) -> bool {
        self.hydrated && (!self.content.is_empty() || self.kept)
    }

    pub fn size_human(&self) -> String {
        ui::format::size(self.size)
    }

    pub fn icon(&self) -> Icon {
        let m = self.mime.to_ascii_lowercase();
        if m.starts_with("image/") {
            Icon::Image
        } else if m.contains("pdf") || m.starts_with("text/") {
            Icon::FileText
        } else if m.contains("zip")
            || m.contains("compressed")
            || m.contains("tar")
            || m.contains("gzip")
        {
            Icon::Archive
        } else if m.contains("calendar") {
            Icon::Calendar
        } else {
            Icon::Paperclip
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub rfc_message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub from: Addr,
    pub to: Vec<Addr>,
    pub cc: Vec<Addr>,
    pub subject: String,
    pub body: String,
    #[serde(default)]
    pub html_body: Option<String>,
    pub attachments: Vec<Attachment>,
    pub timestamp_min: i64,
    pub read: bool,
    pub starred: bool,
    pub folder: Folder,
    /// User-applied labels/tags, mirrored from the CRDT (`flags/<id>/labels`).
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub account: String,
    #[serde(default)]
    pub server: Option<ServerPlace>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ServerPlace {
    pub folder: Folder,
    pub uid: u32,
    pub uid_validity: u32,
}

impl Message {
    pub fn thread_subject(&self) -> String {
        normalize_subject(&self.subject)
    }

    pub fn snippet(&self) -> String {
        let s = self
            .body
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("");
        let cleaned: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
        cleaned
    }

    pub fn has_attachments(&self) -> bool {
        self.shown_attachments().next().is_some()
    }

    pub fn shown_attachments(&self) -> impl Iterator<Item = (usize, &Attachment)> {
        self.attachments
            .iter()
            .enumerate()
            .filter(|(_, attachment)| attachment.inline_id.is_none())
    }
}

pub fn normalize_subject(subject: &str) -> String {
    const PREFIXES: [&str; 3] = ["re:", "fwd:", "fw:"];

    let mut s = subject.trim();
    // Strip the prefix off `s` directly rather than computing a byte offset
    // against a lowercased copy — the offsets only line up while every
    // prefix stays ASCII, and slicing a str at a stale offset panics.
    while let Some(rest) = PREFIXES.iter().find_map(|p| {
        let head = s.get(..p.len())?;
        if head.eq_ignore_ascii_case(p) {
            s.get(p.len()..)
        } else {
            None
        }
    }) {
        s = rest.trim_start();
    }
    s.to_string()
}

fn canonical_message_id(value: Option<&str>) -> Option<String> {
    let value = value?
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn thread_anchor(
    message: &Message,
    messages: &HashMap<MessageId, Message>,
    rfc_ids: &HashMap<String, MessageId>,
) -> String {
    let mut current = message;
    let mut seen = HashSet::new();
    loop {
        // Broken cyclic headers are rare but must not make the view loop. The
        // current message's protocol identity (or UUID) is deterministic.
        if !seen.insert(current.id) {
            return canonical_message_id(current.rfc_message_id.as_deref())
                .unwrap_or_else(|| current.id.to_string());
        }
        let Some(parent_header) = canonical_message_id(current.in_reply_to.as_deref()) else {
            return canonical_message_id(current.rfc_message_id.as_deref())
                .unwrap_or_else(|| current.id.to_string());
        };
        let Some(parent_id) = rfc_ids.get(&parent_header) else {
            return parent_header;
        };
        let Some(parent) = messages.get(parent_id) else {
            return parent_header;
        };
        current = parent;
    }
}

#[derive(Clone, Debug)]
pub struct Thread {
    pub id: String,
    pub subject: String,
    pub message_ids: Vec<MessageId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ComposeFormat {
    #[default]
    Html,
    Text,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
    #[serde(default)]
    pub sender: Option<DraftSender>,
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
    pub body: String,
    #[serde(default)]
    pub html_body: Option<String>,
    /// Bounded RFC 5545 METHOD:REQUEST payload for a calendar invitation.
    #[serde(default)]
    pub calendar_ics: Option<String>,
    #[serde(default)]
    pub attachments: Vec<DraftAttachment>,
    pub in_reply_to: Option<MessageId>,
    #[serde(skip)]
    pub wire: WireIds,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WireIds {
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
}

impl Draft {
    pub fn is_empty(&self) -> bool {
        self.to.is_empty()
            && self.cc.is_empty()
            && self.bcc.is_empty()
            && self.subject.is_empty()
            && self.body.is_empty()
            && self.html_body.as_ref().is_none_or(String::is_empty)
            && self.calendar_ics.is_none()
            && self.attachments.is_empty()
            && self.sender.is_none()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct DraftSender {
    pub account: String,
    pub email: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct DraftAttachment {
    pub content_id: String,
    pub filename: String,
    pub mime: String,
    pub size: u64,
}

tilo_resource::json_model!(DraftSender, DraftAttachment);

#[derive(Clone, Debug)]
pub struct Mailbox {
    pub account: AccountConfig,
    pub messages: HashMap<MessageId, Message>,
    pub order: Vec<MessageId>,
    search_revision: u64,
    threads: RefCell<HashMap<Folder, ThreadMemo>>,
}

#[derive(Clone, Debug)]
struct ThreadMemo {
    revision: u64,
    threads: Rc<[Thread]>,
    of_message: Rc<HashMap<MessageId, usize>>,
}

/// A bounded, newest-first page from the locally-loaded mailbox search.
#[derive(Clone, Debug)]
pub struct MailboxSearchPage<'a> {
    pub messages: Vec<&'a Message>,
    pub inspected: usize,
    pub truncated: bool,
}

static NEXT_SEARCH_REVISION: AtomicU64 = AtomicU64::new(1);

fn next_search_revision() -> u64 {
    NEXT_SEARCH_REVISION.fetch_add(1, Ordering::Relaxed)
}

impl Default for Mailbox {
    fn default() -> Self {
        Self::new(AccountConfig::default())
    }
}

impl Mailbox {
    pub fn new(account: AccountConfig) -> Self {
        Self {
            account,
            messages: HashMap::new(),
            order: Vec::new(),
            search_revision: next_search_revision(),
            threads: RefCell::new(HashMap::new()),
        }
    }

    #[must_use]
    pub fn search_revision(&self) -> u64 {
        self.search_revision
    }

    /// Mark a direct in-place edit as visible to retained projections.
    pub fn touch_search_revision(&mut self) {
        self.search_revision = next_search_revision();
    }

    pub fn insert(&mut self, msg: Message) {
        if !self.messages.contains_key(&msg.id) {
            self.order.push(msg.id);
        }
        self.messages.insert(msg.id, msg);
        self.touch_search_revision();
    }

    pub fn get(&self, id: MessageId) -> Option<&Message> {
        self.messages.get(&id)
    }

    pub fn get_mut(&mut self, id: MessageId) -> Option<&mut Message> {
        self.touch_search_revision();
        self.messages.get_mut(&id)
    }

    fn shows(&self, message: &Message) -> bool {
        message.account.is_empty()
            || self.account.email.is_empty()
            || message.account == self.account.email
    }

    pub fn set_account(&mut self, account: AccountConfig) {
        if self.account.email != account.email {
            self.touch_search_revision();
        }
        self.account = account;
    }

    pub fn unread_count(&self, folder: Folder) -> usize {
        self.messages_in(folder).iter().filter(|m| !m.read).count()
    }

    pub fn unread_count_of(&self, account: &str, folder: Folder) -> usize {
        self.messages
            .values()
            .filter(|m| m.account == account && m.folder == folder && !m.read)
            .count()
    }

    pub fn of_account<'a>(&'a self, account: &'a str) -> impl Iterator<Item = &'a Message> + 'a {
        self.messages.values().filter(move |m| m.account == account)
    }

    pub fn by_uid(&self, account: &str, folder: Folder, uid: u32) -> Option<&Message> {
        self.messages.values().find(|m| {
            m.account == account
                && m.server
                    .is_some_and(|at| at.folder == folder && at.uid == uid)
        })
    }

    pub fn count(&self, folder: Folder) -> usize {
        self.messages_in(folder).len()
    }

    pub fn messages_in(&self, folder: Folder) -> Vec<&Message> {
        let mut v: Vec<&Message> = self
            .order
            .iter()
            .filter_map(|id| self.messages.get(id))
            .filter(|m| self.shows(m))
            .filter(|m| match folder {
                Folder::Starred => m.starred && m.folder != Folder::Trash,
                f => m.folder == f,
            })
            .collect();
        v.sort_by_key(|m| Reverse(m.timestamp_min));
        v
    }

    pub fn threads_in(&self, folder: Folder) -> Vec<Thread> {
        self.threads(folder).to_vec()
    }

    pub fn threads(&self, folder: Folder) -> Rc<[Thread]> {
        self.memo(folder).threads
    }

    pub fn thread_of(&self, folder: Folder, id: MessageId) -> Option<Thread> {
        let memo = self.memo(folder);
        let index = *memo.of_message.get(&id)?;
        memo.threads.get(index).cloned()
    }

    pub fn thread_rooted(&self, folder: Folder, root: &str) -> Option<Thread> {
        self.memo(folder)
            .threads
            .iter()
            .find(|thread| thread.id == root)
            .cloned()
    }

    fn memo(&self, folder: Folder) -> ThreadMemo {
        if let Some(memo) = self.threads.borrow().get(&folder)
            && memo.revision == self.search_revision
        {
            return memo.clone();
        }
        let threads: Rc<[Thread]> = self.compute_threads(folder).into();
        let of_message = threads
            .iter()
            .enumerate()
            .flat_map(|(index, thread)| thread.message_ids.iter().map(move |id| (*id, index)))
            .collect();
        let memo = ThreadMemo {
            revision: self.search_revision,
            threads,
            of_message: Rc::new(of_message),
        };
        let mut memos = self.threads.borrow_mut();
        memos.retain(|_, kept| kept.revision == self.search_revision);
        memos.insert(folder, memo.clone());
        memo
    }

    fn compute_threads(&self, folder: Folder) -> Vec<Thread> {
        let visible = self.messages_in(folder);
        let rfc_ids: HashMap<_, _> = visible
            .iter()
            .filter_map(|message| {
                canonical_message_id(message.rfc_message_id.as_deref())
                    .map(|header| (header, message.id))
            })
            .collect();
        let mut by_anchor: HashMap<String, Vec<MessageId>> = HashMap::new();
        for message in &visible {
            let anchor = thread_anchor(message, &self.messages, &rfc_ids);
            by_anchor.entry(anchor).or_default().push(message.id);
        }
        let mut threads: Vec<Thread> = by_anchor
            .into_iter()
            .filter_map(|(id, mut ids)| {
                ids.sort_by(|left, right| {
                    let left_time = self.messages.get(left).map_or(0, |m| m.timestamp_min);
                    let right_time = self.messages.get(right).map_or(0, |m| m.timestamp_min);
                    left_time.cmp(&right_time).then_with(|| left.cmp(right))
                });
                ids.first()
                    .and_then(|first| self.messages.get(first))
                    .map(|message| Thread {
                        id,
                        subject: message.thread_subject(),
                        message_ids: ids,
                    })
            })
            .collect();
        threads.sort_by(|a, b| {
            let an = a
                .message_ids
                .iter()
                .filter_map(|id| self.messages.get(id).map(|m| m.timestamp_min))
                .max()
                .unwrap_or(0);
            let bn = b
                .message_ids
                .iter()
                .filter_map(|id| self.messages.get(id).map(|m| m.timestamp_min))
                .max()
                .unwrap_or(0);
            bn.cmp(&an)
        });
        threads
    }

    /// Search loaded mail with explicit work and result bounds.
    pub fn search_page(
        &self,
        query: &str,
        scope: Option<Folder>,
        max_inspected: usize,
        max_results: usize,
    ) -> MailboxSearchPage<'_> {
        let q = query.trim().to_ascii_lowercase();
        if q.is_empty() || max_inspected == 0 || max_results == 0 {
            return MailboxSearchPage {
                messages: Vec::new(),
                inspected: 0,
                truncated: false,
            };
        }
        let mut inspected = 0;
        let mut truncated = false;
        let mut v = Vec::with_capacity(max_results.min(64));
        for id in self.order.iter().rev() {
            let Some(m) = self.messages.get(id) else {
                continue;
            };
            if !match scope {
                Some(f) => m.folder == f,
                None => m.folder != Folder::Trash,
            } {
                continue;
            }
            if inspected >= max_inspected || v.len() >= max_results {
                truncated = true;
                break;
            }
            inspected += 1;
            if m.subject.to_ascii_lowercase().contains(&q)
                || m.from.display().to_ascii_lowercase().contains(&q)
                || m.from.email.to_ascii_lowercase().contains(&q)
                || m.body.to_ascii_lowercase().contains(&q)
            {
                v.push(m);
            }
        }
        v.sort_by_key(|m| Reverse(m.timestamp_min));
        MailboxSearchPage {
            messages: v,
            inspected,
            truncated,
        }
    }

    pub fn mark_read(&mut self, id: MessageId, read: bool) {
        if let Some(m) = self.messages.get_mut(&id)
            && m.read != read
        {
            m.read = read;
            self.touch_search_revision();
        }
    }

    pub fn toggle_star(&mut self, id: MessageId) -> bool {
        let starred = if let Some(m) = self.messages.get_mut(&id) {
            m.starred = !m.starred;
            m.starred
        } else {
            return false;
        };
        self.touch_search_revision();
        starred
    }

    pub fn move_to(&mut self, id: MessageId, folder: Folder) {
        if let Some(m) = self.messages.get_mut(&id)
            && m.folder != folder
        {
            m.folder = folder;
            self.touch_search_revision();
        }
    }

    pub fn archive(&mut self, id: MessageId) {
        self.move_to(id, Folder::Archive);
    }

    pub fn delete(&mut self, id: MessageId) {
        let in_trash = self.messages.get(&id).map(|m| m.folder == Folder::Trash);
        match in_trash {
            Some(true) => {
                self.messages.remove(&id);
                self.order.retain(|x| *x != id);
                self.touch_search_revision();
            }
            Some(false) => self.move_to(id, Folder::Trash),
            None => {}
        }
    }

    pub fn purge(&mut self, id: MessageId) {
        if self.messages.remove(&id).is_some() {
            self.order.retain(|candidate| *candidate != id);
            self.touch_search_revision();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_credentials_are_never_serialized_or_debugged() {
        let config = AccountConfig {
            password: "private-mail-password".into(),
            ..Default::default()
        };
        assert!(
            !serde_json::to_string(&config)
                .unwrap()
                .contains("private-mail-password")
        );
        assert!(!format!("{config:?}").contains("private-mail-password"));
    }

    #[test]
    fn a_written_recipient_reads_back_as_the_same_one() {
        for name in [
            "Ada Lovelace",
            "Lovelace, Ada",
            "Ada \"Countess\" Lovelace",
            "Dr. Ada Lovelace",
            "Ada (Analytical) Lovelace",
            "Back\\slash",
        ] {
            let addr = Addr::new(name, "ada@analytical.example");
            assert_eq!(
                crate::transport::parse_addr_list(&addr.rfc5322()),
                vec![addr.clone()],
                "{name:?} written as {:?}",
                addr.rfc5322()
            );
        }
        let two = [
            Addr::new("Lovelace, Ada", "ada@analytical.example"),
            Addr::new("Babbage, Charles", "charles@engine.example"),
        ];
        let written = two.iter().map(Addr::rfc5322).collect::<Vec<_>>().join(", ");
        assert_eq!(crate::transport::parse_addr_list(&written), two.to_vec());
    }

    fn msg(subject: &str, ts: i64, folder: Folder) -> Message {
        Message {
            id: Uuid::new_v4(),
            rfc_message_id: None,
            in_reply_to: None,
            from: Addr::new("Ada Lovelace", "ada@analytical.example"),
            to: vec![Addr::bare("you@example.test")],
            cc: vec![],
            subject: subject.into(),
            body: "First line.\nSecond line.".into(),
            html_body: None,
            attachments: vec![],
            timestamp_min: ts,
            read: false,
            starred: false,
            folder,
            labels: Vec::new(),
            account: String::new(),
            server: None,
        }
    }

    #[test]
    fn threads_follow_rfc_links_and_stay_stable() {
        {
            assert_eq!(normalize_subject("Re: Re: Hello"), "Hello");
            assert_eq!(normalize_subject("FWD: Fw: Plan"), "Plan");
            assert_eq!(normalize_subject("Plain"), "Plain");
        }

        {
            let mut mb = Mailbox::default();
            let mut first = msg("Launch plan", 100, Folder::Inbox);
            first.rfc_message_id = Some("launch-root@example.test".into());
            let mut reply = msg("Re: Launch plan", 200, Folder::Inbox);
            reply.rfc_message_id = Some("launch-reply@example.test".into());
            reply.in_reply_to = Some("<launch-root@example.test>".into());
            let mut unrelated = msg("Launch plan", 150, Folder::Inbox);
            unrelated.rfc_message_id = Some("unrelated@example.test".into());
            mb.insert(first);
            mb.insert(reply);
            mb.insert(unrelated);
            let threads = mb.threads_in(Folder::Inbox);
            assert_eq!(threads.len(), 2);
            assert_eq!(threads[0].subject, "Launch plan");
            assert_eq!(threads[0].message_ids.len(), 2);
            assert_eq!(threads[0].id, "launch-root@example.test");
            assert_eq!(threads[1].message_ids.len(), 1);
            assert_eq!(threads[1].id, "unrelated@example.test");
        }

        {
            let mut mb = Mailbox::default();
            // C's raw mail held `References: <launch-root> <launch-middle>`;
            let mut latest = msg("Re: Launch plan", 300, Folder::Inbox);
            latest.rfc_message_id = Some("launch-latest@example.test".into());
            latest.in_reply_to = Some("launch-root@example.test".into());
            mb.insert(latest);
            let before = mb.threads_in(Folder::Inbox);
            assert_eq!(before[0].id, "launch-root@example.test");

            let mut middle = msg("Re: Launch plan", 200, Folder::Inbox);
            middle.rfc_message_id = Some("launch-middle@example.test".into());
            middle.in_reply_to = Some("launch-root@example.test".into());
            mb.insert(middle);
            let after_middle = mb.threads_in(Folder::Inbox);
            assert_eq!(after_middle.len(), 1);
            assert_eq!(after_middle[0].id, "launch-root@example.test");

            let mut root = msg("Launch plan", 100, Folder::Inbox);
            root.rfc_message_id = Some("launch-root@example.test".into());
            mb.insert(root);
            let after = mb.threads_in(Folder::Inbox);
            assert_eq!(after.len(), 1);
            assert_eq!(after[0].id, "launch-root@example.test");
            assert_eq!(after[0].message_ids.len(), 3);
        }
    }

    #[test]
    fn folder_flags_track_unread_star_and_trash() {
        {
            let mut mb = Mailbox::default();
            let m = msg("Hi", 10, Folder::Inbox);
            let id = m.id;
            mb.insert(m);
            assert_eq!(mb.unread_count(Folder::Inbox), 1);
            mb.mark_read(id, true);
            assert_eq!(mb.unread_count(Folder::Inbox), 0);
        }

        {
            let mut mb = Mailbox::default();
            let m = msg("Hi", 10, Folder::Inbox);
            let id = m.id;
            mb.insert(m);
            assert_eq!(mb.count(Folder::Starred), 0);
            assert!(mb.toggle_star(id));
            assert_eq!(mb.count(Folder::Starred), 1);
        }

        {
            let mut mb = Mailbox::default();
            let m = msg("Hi", 10, Folder::Inbox);
            let id = m.id;
            mb.insert(m);
            mb.delete(id);
            assert_eq!(mb.get(id).unwrap().folder, Folder::Trash);
            mb.delete(id);
            assert!(mb.get(id).is_none());
        }
    }

    #[test]
    fn mailbox_search_is_bounded_and_case_insensitive() {
        {
            let mut mb = Mailbox::default();
            for ts in 0..6 {
                mb.insert(msg("Release notes", ts, Folder::Inbox));
            }

            let page = mb.search_page("release", None, 4, 2);
            assert_eq!(page.inspected, 2, "result bound ends work early");
            assert_eq!(page.messages.len(), 2);
            assert!(
                page.truncated,
                "remaining loaded matches are not enumerated"
            );
            assert!(
                page.messages[0].timestamp_min > page.messages[1].timestamp_min,
                "visible search rows remain newest-first"
            );

            let body_page = mb.search_page("second line", None, 3, 20);
            assert_eq!(body_page.inspected, 3, "body inspection obeys its cap");
            assert_eq!(body_page.messages.len(), 3);
            assert!(body_page.truncated);
        }
    }
}
