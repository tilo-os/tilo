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

/// Stable identifier for a single message. In the live app this is derived
/// from the RFC 5322 `Message-ID` header; for synthetic/sample messages we
/// mint a v4 UUID.
pub type MessageId = Uuid;

/// The canonical system folders every account has. We keep them as an enum
/// (rather than free-form strings) so routing — archive/delete/move — is
/// total and exhaustively checked by the compiler.
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
    /// Spam / junk.
    Junk,
}

impl Folder {
    /// Display order in the sidebar.
    pub const ALL: [Folder; 7] = [
        Folder::Inbox,
        Folder::Starred,
        Folder::Sent,
        Folder::Drafts,
        Folder::Archive,
        Folder::Junk,
        Folder::Trash,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Folder::Inbox => "Inbox",
            Folder::Starred => "Starred",
            Folder::Sent => "Sent",
            Folder::Drafts => "Drafts",
            Folder::Archive => "Archive",
            Folder::Trash => "Trash",
            Folder::Junk => "Junk",
        }
    }

    /// The themed vector [`Icon`] shown to the left of the folder name. These
    /// come from the shared `ui` lucide set so they render as crisp strokes at
    /// any scale — never a font-dependent emoji that falls back to tofu.
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

    /// The stable string key this folder is stored under in the CRDT (and the
    /// key a [`crate::actions::MailAction::Move`] carries). The inverse of
    /// [`Folder::from_key`]. Kept here (rather than in `doc`) so the action layer
    /// and the persistence layer share one canonical spelling.
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

    /// Parse a folder key produced by [`Folder::as_key`]. Returns `None` for an
    /// unrecognised key (e.g. a user-defined local folder id), so a caller can
    /// treat it as a no-op rather than substituting a wrong folder.
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

    /// IMAP mailbox name this folder maps to on a typical server. Used by
    /// the transport layer when issuing `SELECT`.
    pub fn imap_name(self) -> &'static str {
        match self {
            // Starred is virtual: filtered by \Flagged within the inbox.
            Folder::Inbox | Folder::Starred => "INBOX",
            Folder::Sent => "Sent",
            Folder::Drafts => "Drafts",
            Folder::Archive => "Archive",
            Folder::Trash => "Trash",
            Folder::Junk => "Junk",
        }
    }
}

/// A single email address with an optional display name.
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

    /// Best display string: the name if present, otherwise the local-part
    /// of the address, otherwise the full address.
    pub fn display(&self) -> String {
        if let Some(n) = &self.name
            && !n.is_empty()
        {
            return n.clone();
        }
        self.email.clone()
    }

    /// RFC 5322 mailbox rendering: `Name <email>` or bare `email`.
    pub fn rfc5322(&self) -> String {
        match &self.name {
            Some(n) if !n.is_empty() => format!("{} <{}>", n, self.email),
            _ => self.email.clone(),
        }
    }
}

/// A decoded attachment. When the bytes are present locally (`hydrated`) they
/// are held in `content`, so the reader can save the real file to disk; in the
/// live app the bytes are hydrated on demand (same projection-vs-hydration
/// story as the Tilo FUSE layer) and `content` is empty until then.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub filename: String,
    pub mime: String,
    pub size: u64,
    /// Whether the bytes are present locally (vs. server-side only).
    pub hydrated: bool,
    /// Portable draft-content identity when this attachment is a Drafts
    /// projection. Incoming mail leaves this absent; drafts use it to reopen
    /// with the same content-addressed bytes on any collaborator.
    #[serde(default)]
    pub content_id: Option<String>,
    /// The decoded bytes, when hydrated. Never serialised into the shared
    /// settings store (it holds message content, not settings).
    #[serde(skip, default)]
    pub content: Vec<u8>,
    /// The bytes are kept on this device with the message's other kept mail
    /// ([`crate::store`]), to be read when the attachment is opened or saved.
    #[serde(default)]
    pub kept: bool,
}

impl Attachment {
    /// A metadata-only attachment (no bytes yet — projection, not hydrated).
    pub fn projected(filename: impl Into<String>, mime: impl Into<String>, size: u64) -> Self {
        Self {
            filename: filename.into(),
            mime: mime.into(),
            size,
            hydrated: false,
            content_id: None,
            content: Vec::new(),
            kept: false,
        }
    }

    /// A fully-hydrated attachment carrying its decoded bytes.
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
        }
    }

    /// Whether the bytes are on this device and this attachment can be
    /// opened or saved.
    pub fn is_downloadable(&self) -> bool {
        self.hydrated && (!self.content.is_empty() || self.kept)
    }

    /// Human-readable size, e.g. `14 KiB`, `1.2 MiB`.
    pub fn size_human(&self) -> String {
        ui::format::size(self.size)
    }

    /// Coarse category used to pick a themed vector [`Icon`] for the chip.
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

/// A single message. The immutable corpus fields (from/to/subject/body/…)
/// come from the server or sample corpus; the mutable flags
/// (read/starred/folder) are mirrored into the CRDT so they sync and undo.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    /// RFC 5322 `Message-ID` (without angle brackets), if known.
    pub rfc_message_id: Option<String>,
    /// Canonical RFC thread anchor used for grouping: the oldest `References`
    /// token when present, otherwise `In-Reply-To`. The transport deliberately
    /// retains the root rather than a display-subject heuristic.
    pub in_reply_to: Option<String>,
    pub from: Addr,
    pub to: Vec<Addr>,
    pub cc: Vec<Addr>,
    pub subject: String,
    /// Plain-text body (the parsed text/plain part, or an HTML-stripped
    /// fallback).
    pub body: String,
    /// The original `text/html` part, when the message carried one. Rendered
    /// sandboxed in the reader's `WebView` (remote content blocked by default);
    /// `None` for plain-text-only mail, which renders as text. Never
    /// serialised into the shared settings store — it is message content.
    #[serde(default)]
    pub html_body: Option<String>,
    pub attachments: Vec<Attachment>,
    /// Minutes since the Unix epoch — coarse, but enough for relative
    /// "2h ago" style rendering without pulling `chrono`.
    pub timestamp_min: i64,
    // ---- mutable flags (mirrored to the CRDT) ----
    pub read: bool,
    pub starred: bool,
    pub folder: Folder,
    /// User-applied labels/tags, mirrored from the CRDT (`flags/<id>/labels`).
    /// These are set exclusively through [`crate::actions::MailAction::AddLabel`]
    /// / [`crate::actions::MailAction::RemoveLabel`] and overlaid onto the
    /// in-memory message by [`crate::resource::Mail::apply_flags`]; empty for a
    /// message that has never been tagged.
    #[serde(default)]
    pub labels: Vec<String>,
    /// The account whose server holds the message (its address); empty for
    /// mail that is only here (the sample, a draft).
    #[serde(default)]
    pub account: String,
    /// Where its server holds the message, as last reported: `None` for
    /// mail that is only here. A message moved here keeps where it was
    /// until the server reports where it put it.
    #[serde(default)]
    pub server: Option<ServerPlace>,
}

/// Where a server holds a message: its folder, its UID there, and the
/// numbering (UIDVALIDITY) that UID belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ServerPlace {
    pub folder: Folder,
    pub uid: u32,
    pub uid_validity: u32,
}

impl Message {
    /// Normalise a subject for threading: strip leading `Re:` / `Fwd:`
    /// chains and collapse whitespace.
    pub fn thread_subject(&self) -> String {
        normalize_subject(&self.subject)
    }

    /// First non-empty line of the body, used as the list snippet.
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
        !self.attachments.is_empty()
    }
}

/// Strip `Re:` / `RE:` / `Fwd:` / `FW:` prefixes (possibly repeated) and
/// trim, so replies thread under the original subject.
pub fn normalize_subject(subject: &str) -> String {
    /// Reply/forward markers stripped from the front of a subject, matched
    /// case-insensitively.
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

/// Normalise an RFC `Message-ID` / `In-Reply-To` token to the spelling used by
/// the transport's stable message ids. Empty/malformed headers are discarded
/// rather than becoming a shared thread key.
fn canonical_message_id(value: Option<&str>) -> Option<String> {
    let value = value?
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// Resolve one message's durable thread root. The parent header itself is a
/// valid anchor when the parent is not loaded yet; that is what makes a reply's
/// resource address survive incremental history hydration.
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

/// A conversation: 1+ messages connected through RFC `Message-ID` /
/// `In-Reply-To`, ordered oldest → newest. `id` is the root RFC identity (or
/// the root message id for legacy mail with no RFC identity), so it remains
/// stable when older members are hydrated later. The list pane renders one row
/// per thread.
#[derive(Clone, Debug)]
pub struct Thread {
    /// Stable protocol/root identity, never derived from a display subject.
    pub id: String,
    pub subject: String,
    pub message_ids: Vec<MessageId>,
}

/// Which representation the compose body is authored in. Persisted so the
/// user's choice is remembered across compose windows and sessions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ComposeFormat {
    /// WYSIWYG rich text, serialised to an HTML MIME part on send.
    #[default]
    Html,
    /// Plain text, serialised to a `text/plain` MIME part on send.
    Text,
}

/// A draft message being composed. Persisted to the CRDT so it survives a
/// crash and syncs across peers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
    /// Explicit sender for a reviewed Contacts draft; never follows the sidebar.
    #[serde(default)]
    pub sender: Option<DraftSender>,
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
    /// Plain-text body (always populated; the canonical text alternative).
    pub body: String,
    /// HTML body, set when the draft was authored in WYSIWYG mode. Sent as the
    /// `text/html` part alongside `body` as the `text/plain` alternative.
    #[serde(default)]
    pub html_body: Option<String>,
    /// Bounded RFC 5545 METHOD:REQUEST payload for a calendar invitation.
    /// Stored inline so a collaborative draft never grants a host-path read.
    #[serde(default)]
    pub calendar_ics: Option<String>,
    /// Verified portable attachment metadata.  `content_id` names bytes in
    /// the content store; no participant's filesystem path is persisted or replicated.
    #[serde(default)]
    pub attachments: Vec<DraftAttachment>,
    /// If this draft is a reply, the message it answers.
    pub in_reply_to: Option<MessageId>,
    /// How the message names itself and the conversation on the wire, set
    /// when it is handed to a server: its own `Message-ID`, and the
    /// `In-Reply-To` and `References` of a reply. Not part of the draft
    /// anyone edits.
    #[serde(skip)]
    pub wire: WireIds,
}

/// The identities a sent message carries in its headers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WireIds {
    /// Its own `Message-ID`, without angle brackets.
    pub message_id: Option<String>,
    /// The message it answers.
    pub in_reply_to: Option<String>,
    /// The conversation it belongs to, oldest first.
    pub references: Vec<String>,
}

impl Draft {
    /// Whether the persisted composition has any content or reviewed sender.
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

/// Exact sending account reviewed when preparing a Contacts draft.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DraftSender {
    pub account: String,
    pub email: String,
}

/// Metadata for one staged draft attachment. The content is addressed by a
/// verified content-store identity and materialized only at the send boundary.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DraftAttachment {
    pub content_id: String,
    pub filename: String,
    pub mime: String,
    pub size: u64,
}

tilo_resource::json_model!(DraftSender, DraftAttachment);

/// The whole mailbox aggregate: an account, its messages, and the derived
/// folder/thread structure. Pure logic — the widgets ask it questions.
#[derive(Clone, Debug)]
pub struct Mailbox {
    pub account: AccountConfig,
    pub messages: HashMap<MessageId, Message>,
    /// Insertion order is irrelevant; folder views sort by timestamp.
    pub order: Vec<MessageId>,
    /// Monotonically changes whenever a field represented in Mail's durable
    /// search catalogue changes. The palette can therefore retain its index
    /// across keystrokes instead of repeatedly walking the loaded mailbox.
    search_revision: u64,
    /// The conversations of the folder last asked for, kept until the
    /// mailbox changes: the list and the reader ask for them every frame.
    threads: RefCell<HashMap<Folder, ThreadMemo>>,
}

/// One folder's conversations at one revision of the mailbox.
#[derive(Clone, Debug)]
struct ThreadMemo {
    revision: u64,
    threads: Rc<[Thread]>,
    /// The conversation each message belongs to, by its index in `threads`.
    of_message: Rc<HashMap<MessageId, usize>>,
}

/// A bounded, newest-first page from the locally-loaded mailbox search.
///
/// Mail's list filter runs on every edit and paint-adjacent selection update,
/// unlike the retained global-search catalogue.  Returning the work performed
/// makes that hot path honest: callers can keep interaction latency bounded
/// even when an account has a very large locally hydrated history.
#[derive(Clone, Debug)]
pub struct MailboxSearchPage<'a> {
    pub messages: Vec<&'a Message>,
    /// Number of in-scope messages whose searchable fields were inspected.
    pub inspected: usize,
    /// `true` when the caller's work or result limit stopped enumeration.
    pub truncated: bool,
}

/// Process-local sequence for retained in-memory projections. A fresh mailbox
/// must not accidentally share revision zero with a previous account whose
/// catalogue is still retained by the app shell.
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

    /// Revision of the loaded searchable corpus and its resource-facing flags.
    #[must_use]
    pub fn search_revision(&self) -> u64 {
        self.search_revision
    }

    /// Mark a direct in-place edit as visible to retained projections.
    ///
    /// Most callers should use the mailbox mutation methods below. This tiny
    /// escape hatch keeps the document reconciliation path honest without
    /// exposing the revision field itself.
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

    /// The message to change in place. Whatever the caller changes may
    /// change a folder's conversations, so the mailbox counts as changed.
    pub fn get_mut(&mut self, id: MessageId) -> Option<&mut Message> {
        self.touch_search_revision();
        self.messages.get_mut(&id)
    }

    /// Whether the mailbox shows `message`: it belongs to the account the
    /// mailbox is open on, or to no account (mail that is only here).
    fn shows(&self, message: &Message) -> bool {
        message.account.is_empty()
            || self.account.email.is_empty()
            || message.account == self.account.email
    }

    /// Open the mailbox on `account`: its folders show that account's mail.
    pub fn set_account(&mut self, account: AccountConfig) {
        if self.account.email != account.email {
            self.touch_search_revision();
        }
        self.account = account;
    }

    /// Number of unread messages routed to `folder` (Starred is virtual).
    pub fn unread_count(&self, folder: Folder) -> usize {
        self.messages_in(folder).iter().filter(|m| !m.read).count()
    }

    /// Number of unread messages of `account` in `folder`, whichever
    /// account the mailbox is open on.
    pub fn unread_count_of(&self, account: &str, folder: Folder) -> usize {
        self.messages
            .values()
            .filter(|m| m.account == account && m.folder == folder && !m.read)
            .count()
    }

    /// The messages of `account`, every folder.
    pub fn of_account<'a>(&'a self, account: &'a str) -> impl Iterator<Item = &'a Message> + 'a {
        self.messages.values().filter(move |m| m.account == account)
    }

    /// The message `account`'s server holds in `folder` under `uid`.
    pub fn by_uid(&self, account: &str, folder: Folder, uid: u32) -> Option<&Message> {
        self.messages.values().find(|m| {
            m.account == account
                && m.server
                    .is_some_and(|at| at.folder == folder && at.uid == uid)
        })
    }

    /// Total messages visible in `folder`.
    pub fn count(&self, folder: Folder) -> usize {
        self.messages_in(folder).len()
    }

    /// All messages routed into `folder`, newest first. `Starred` is a
    /// virtual folder: any starred message not in Trash.
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

    /// Group the messages of `folder` into RFC-linked threads, each ordered
    /// oldest→newest, and order those threads by most-recent activity.
    ///
    /// A matching subject is intentionally *not* evidence of a conversation:
    /// unrelated "Status update" messages must stay separately addressable.
    /// Legacy messages that have neither a usable `Message-ID` nor a usable
    /// `In-Reply-To` receive their own UUID-backed thread identity. A reply
    /// with a `References` root retains that root even when intermediate or
    /// older members are hydrated later. An `In-Reply-To`-only partial chain
    /// can only anchor at its unloaded immediate parent until more history is
    /// available; this is the RFC's unavoidable information limit.
    pub fn threads_in(&self, folder: Folder) -> Vec<Thread> {
        self.threads(folder).to_vec()
    }

    /// The conversations of `folder`, as [`Self::threads_in`] orders them,
    /// computed once per change of the mailbox.
    pub fn threads(&self, folder: Folder) -> Rc<[Thread]> {
        self.memo(folder).threads
    }

    /// The conversation of `folder` that holds message `id`, oldest message
    /// first.
    pub fn thread_of(&self, folder: Folder, id: MessageId) -> Option<Thread> {
        let memo = self.memo(folder);
        let index = *memo.of_message.get(&id)?;
        memo.threads.get(index).cloned()
    }

    /// The conversation of `folder` whose root is `root`.
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
        // Order threads by their newest message, descending.
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

    /// Case-insensitive full-text search across subject/sender/body, scoped
    /// to a folder or (when `None`) the whole mailbox excluding Trash.
    ///
    /// This compatibility helper is deliberately exhaustive. Interactive UI
    /// callers should use [`Self::search_page`] with explicit bounds instead.
    pub fn search(&self, query: &str, scope: Option<Folder>) -> Vec<&Message> {
        self.search_page(query, scope, usize::MAX, usize::MAX)
            .messages
    }

    /// Search loaded mail with explicit work and result bounds.
    ///
    /// The newest loaded messages are considered first, then matches are kept
    /// in timestamp-descending order. A result is still a canonical message id
    /// and callers resolve the complete message lazily when it is opened; this
    /// method only prevents typing a broad query from walking unbounded bodies
    /// or scheduling an unbounded amount of row paint.
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

    // ---- mutating actions (the reader/list issue these) ----

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

    /// Move to Trash, or permanently drop if already in Trash.
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

    /// Remove a hydrated message without applying the usual first-delete
    /// move-to-Trash affordance. Used when a replicated purge tombstone is
    /// reconciled from the document.
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
        // Phase `normalize_strips_reply_prefixes`.
        {
            assert_eq!(normalize_subject("Re: Re: Hello"), "Hello");
            assert_eq!(normalize_subject("FWD: Fw: Plan"), "Plan");
            assert_eq!(normalize_subject("Plain"), "Plain");
        }

        // Phase `threads_follow_rfc_reply_links_without_merging_same_subject_mail`.
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
            // Most-recent-first: "Launch plan" (200) before "Lunch?" (150).
            assert_eq!(threads[0].subject, "Launch plan");
            assert_eq!(threads[0].message_ids.len(), 2);
            assert_eq!(threads[0].id, "launch-root@example.test");
            assert_eq!(threads[1].message_ids.len(), 1);
            assert_eq!(threads[1].id, "unrelated@example.test");
        }

        // Phase `references_root_keeps_a_multihop_thread_stable_during_hydration`.
        {
            let mut mb = Mailbox::default();
            // C's raw mail held `References: <launch-root> <launch-middle>`;
            // transport stores the oldest/root token in `in_reply_to`. At this
            // point neither B nor A has been hydrated yet.
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
        // Phase `unread_count_and_mark_read`.
        {
            let mut mb = Mailbox::default();
            let m = msg("Hi", 10, Folder::Inbox);
            let id = m.id;
            mb.insert(m);
            assert_eq!(mb.unread_count(Folder::Inbox), 1);
            mb.mark_read(id, true);
            assert_eq!(mb.unread_count(Folder::Inbox), 0);
        }

        // Phase `star_makes_message_appear_in_virtual_folder`.
        {
            let mut mb = Mailbox::default();
            let m = msg("Hi", 10, Folder::Inbox);
            let id = m.id;
            mb.insert(m);
            assert_eq!(mb.count(Folder::Starred), 0);
            assert!(mb.toggle_star(id));
            assert_eq!(mb.count(Folder::Starred), 1);
        }

        // Phase `delete_moves_to_trash_then_purges`.
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
        // Phase `search_page_enforces_work_and_result_bounds_without_losing_newest_order`.
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

        // Phase `search_matches_subject_and_body_case_insensitively`.
        {
            let mut mb = Mailbox::default();
            mb.insert(msg("Launch Plan", 10, Folder::Inbox));
            let hits = mb.search("launch", None);
            assert_eq!(hits.len(), 1);
            let body_hits = mb.search("second line", None);
            assert_eq!(body_hits.len(), 1);
            assert!(mb.search("nothing", None).is_empty());
        }
    }
}
